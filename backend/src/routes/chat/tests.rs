use std::collections::VecDeque;
use std::sync::Mutex;

use reqwest::StatusCode;
use serde_json::json;

use super::*;
use crate::chat::Reply;

/// Answers from a queue and keeps every prompt it got, so tests never call Claude.
#[derive(Default)]
struct Fake {
    replies: Mutex<VecDeque<Reply>>,
    seen: Mutex<Vec<(String, String)>>,
}

impl Fake {
    fn queue(&self, reply: &str, answered: bool, interested: bool) {
        self.replies.lock().unwrap().push_back(Reply { reply: reply.into(), answered, interested });
    }

    fn calls(&self) -> usize {
        self.seen.lock().unwrap().len()
    }

    fn last(&self) -> (String, String) {
        self.seen.lock().unwrap().last().cloned().unwrap()
    }
}

impl Model for Fake {
    fn answer<'a>(&'a self, state: &'a AppState, system: &'a str, prompt: &'a str) -> futures::future::BoxFuture<'a, anyhow::Result<(Option<Reply>, f64)>> {
        Box::pin(async move {
            self.seen.lock().unwrap().push((system.to_string(), prompt.to_string()));
            // A teammate taking over while the model is still answering.
            if prompt.contains("TAKE OVER NOW") {
                sqlx::query("update chat_conversations set taken_over_at = now()").execute(&state.db).await?;
            }
            let reply = self.replies.lock().unwrap().pop_front().unwrap_or(Reply { reply: "It's in the facts.".into(), answered: true, interested: false });
            Ok((Some(reply), 0.002))
        })
    }
}

struct Seeded {
    state: AppState,
    fake: Arc<Fake>,
    base: String,
    auth: Auth,
    project: Uuid,
    key: String,
}

const ORIGIN: &str = "https://www.fieldnote.example";

/// An organization with a user and a project on fieldnote.example, its chat on, and the routes served with a fake model.
async fn seed(db: &PgPool) -> Seeded {
    let state = crate::autopilot::tests::state(db.clone());
    let (auth, project, key) = project(db, "Fieldnote", "fieldnote.example", "Field notes for survey teams. Plans start at $20 a seat.").await;
    sqlx::query("insert into materials (project_id, kind, title, content) values ($1, 'case_study', 'Kestrel Surveys', 'Kestrel cut report time by half.')")
        .bind(project)
        .execute(db)
        .await
        .unwrap();
    sqlx::query("update chat_settings set enabled = true, faq = 'Q: Do you have an app? A: Yes, for iOS and Android.' where project_id = $1")
        .bind(project)
        .execute(db)
        .await
        .unwrap();
    let fake = Arc::new(Fake::default());
    let app = Router::new().nest("/api", router_with(fake.clone())).with_state(state.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
    Seeded { state, fake, base, auth, project, key }
}

async fn project(db: &PgPool, name: &str, domain: &str, description: &str) -> (Auth, Uuid, String) {
    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
    let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, 'Sam Rivera', '') returning id")
        .bind(format!("sam@{domain}"))
        .fetch_one(db)
        .await
        .unwrap();
    let project: Uuid = sqlx::query_scalar(
        "insert into projects (org_id, domain, name, business_description, company) values ($1, $2, $3, $4, $5) returning id",
    )
    .bind(org)
    .bind(domain)
    .bind(name)
    .bind(description)
    .bind(json!({"name": name, "description": description, "products": ["Field notes"], "value_props": ["Faster reports"]}))
    .fetch_one(db)
    .await
    .unwrap();
    let key: String = sqlx::query_scalar("insert into visitor_sites (project_id, domains) values ($1, $2) returning site_key")
        .bind(project)
        .bind(vec![domain.to_string()])
        .fetch_one(db)
        .await
        .unwrap();
    sqlx::query("insert into chat_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
    (Auth { user_id: user, org_id: org, role: "owner".into(), token_hash: String::new() }, project, key)
}

impl Seeded {
    async fn post(&self, path: &str, ip: &str, origin: Option<&str>, body: Value) -> (StatusCode, Value, Option<String>) {
        let mut req = reqwest::Client::new()
            .post(format!("{}/c/{}/{path}", self.base, self.key))
            .header("content-type", "text/plain;charset=UTF-8")
            .header("x-forwarded-for", ip)
            .body(body.to_string());
        if let Some(origin) = origin {
            req = req.header("origin", origin);
        }
        let res = req.send().await.unwrap();
        let cors = res.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap().to_string());
        (res.status(), res.json().await.unwrap_or_default(), cors)
    }

    async fn say(&self, ip: &str, conversation: Option<(&str, &str)>, text: &str) -> (StatusCode, Value) {
        let mut body = json!({"text": text, "page": "/pricing/", "visitor": "v-1"});
        if let Some((id, token)) = conversation {
            body["conversation"] = json!(id);
            body["token"] = json!(token);
        }
        let (status, body, _) = self.post("messages", ip, Some(ORIGIN), body).await;
        (status, body)
    }

    /// Starts a conversation and returns its id and token.
    async fn start(&self, ip: &str, text: &str) -> (String, String) {
        let (status, body) = self.say(ip, None, text).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (body["conversation"].as_str().unwrap().to_string(), body["token"].as_str().unwrap().to_string())
    }
}

fn roles(body: &Value) -> Vec<(String, String)> {
    body["messages"].as_array().unwrap().iter().map(|m| (m["role"].as_str().unwrap().to_string(), m["body"].as_str().unwrap().to_string())).collect()
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn answers_come_from_this_projects_facts_only(db: PgPool) {
    let s = seed(&db).await;
    let (_, other, _) = project(&db, "Quarry", "quarry.example", "Quarry sells gravel.").await;
    sqlx::query("insert into materials (project_id, kind, title, content) values ($1, 'note', 'Secret', 'QUARRY-PRICE-LIST')").bind(other).execute(&db).await.unwrap();

    s.fake.queue("Plans start at $20 a seat.", true, false);
    let (status, body) = s.say("203.0.113.1", None, "<b>How much</b> is it?").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(roles(&body), [("visitor".to_string(), "How much is it?".to_string()), ("bot".into(), "Plans start at $20 a seat.".into())]);
    assert_eq!(body["ask_email"], false);
    assert!(body["token"].is_string());

    let (system, prompt) = s.fake.last();
    for fact in ["Field notes for survey teams", "Kestrel cut report time by half", "Do you have an app?", "Faster reports"] {
        assert!(system.contains(fact), "the prompt has {fact}");
    }
    assert!(!system.contains("QUARRY") && !system.contains("gravel"), "nothing from another project");
    assert!(prompt.ends_with("Visitor: How much is it?\n\nWrite your reply to the visitor's last message."));

    // The next message carries the conversation so far, and the reply's cost is kept for the monthly cap.
    let (id, token) = (body["conversation"].as_str().unwrap().to_string(), body["token"].as_str().unwrap().to_string());
    let (status, body) = s.say("203.0.113.1", Some((&id, &token)), "And for ten people?").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["token"].is_null(), "the token is only handed out once");
    assert!(s.fake.last().1.contains("Visitor: How much is it?\n\nYou: Plans start at $20 a seat.\n\nVisitor: And for ten people?"));
    let used = usage(&db, s.project).await.unwrap();
    assert_eq!(used.today_messages, 2);
    assert_eq!(used.month_spend_micros, 4000);
    let domain: Option<String> = sqlx::query_scalar("select domain::text from chat_conversations").fetch_one(&db).await.unwrap();
    assert_eq!(domain, None);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_question_the_facts_dont_answer_offers_a_follow_up(db: PgPool) {
    let s = seed(&db).await;
    s.fake.queue("I don't know that one, but someone from the team can follow up.", false, false);
    let (status, body) = s.say("203.0.113.2", None, "Do you integrate with SAP?").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ask_email"], true);
    assert_eq!(roles(&body)[1].1, "I don't know that one, but someone from the team can follow up.");

    // Interest asks for an email too, even when it was answered.
    s.fake.queue("Happy to set up a demo.", true, true);
    let id = body["conversation"].as_str().unwrap().to_string();
    let token = body["token"].as_str().unwrap().to_string();
    let (_, body) = s.say("203.0.113.2", Some((&id, &token)), "Can I get a demo?").await;
    assert_eq!(body["ask_email"], true);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_email_makes_one_hot_lead_linked_to_the_visiting_company(db: PgPool) {
    let s = seed(&db).await;
    // The visitors snippet saw this browser at Kestrel, which the project already has as a company.
    let company: Uuid = sqlx::query_scalar("insert into companies (project_id, domain, name) values ($1, 'kestrel.example', 'Kestrel Surveys') returning id")
        .bind(s.project)
        .fetch_one(&db)
        .await
        .unwrap();
    sqlx::query("insert into visitor_hits (project_id, ip_hash, visitor_id, domain, company, path) values ($1, 'other', 'v-1', 'kestrel.example', 'KESTREL', '/pricing')")
        .bind(s.project)
        .execute(&db)
        .await
        .unwrap();
    s.fake.queue("Happy to set up a demo.", true, true);
    let (id, token) = s.start("203.0.113.3", "Can we get a demo?").await;

    let email = |email: &str, name: &str| json!({"conversation": id, "token": token, "email": email, "name": name, "company": ""});
    let (status, body, _) = s.post("lead", "203.0.113.3", Some(ORIGIN), email("not an email", "")).await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::BAD_REQUEST, Some("Enter a valid email address")));
    let (status, body, cors) = s.post("lead", "203.0.113.3", Some(ORIGIN), email(" Dana@Kestrel.example ", "Dana Lee")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(cors.as_deref(), Some(ORIGIN));
    assert_eq!(roles(&body), [("notice".to_string(), "Thanks, Dana! Someone from the team will email you at dana@kestrel.example.".to_string())]);

    let lead: Lead = sqlx::query_as("select * from leads where project_id = $1").bind(s.project).fetch_one(&db).await.unwrap();
    assert_eq!((lead.source.as_str(), lead.status.as_str(), lead.full_name.as_str()), ("chat", "hot", "Dana Lee"));
    assert_eq!((lead.company_name.as_str(), lead.company_domain.as_deref(), lead.company_id), ("Kestrel Surveys", Some("kestrel.example"), Some(company)));
    assert!(lead.notes.contains("Visitor: Can we get a demo?\nUs: Happy to set up a demo."), "{}", lead.notes);
    let (lead_id, domain): (Option<Uuid>, Option<String>) = sqlx::query_as("select lead_id, domain::text from chat_conversations").fetch_one(&db).await.unwrap();
    assert_eq!((lead_id, domain.as_deref()), (Some(lead.id), Some("kestrel.example")));

    // The same address again, from a second chat, updates that lead instead of adding one, and keeps what it knew.
    s.fake.queue("Sure.", true, true);
    let (id2, token2) = s.start("203.0.113.3", "Me again, pricing please").await;
    let again = json!({"conversation": id2, "token": token2, "email": "dana@kestrel.example", "name": "D", "company": "Other Co"});
    assert_eq!(s.post("lead", "203.0.113.3", Some(ORIGIN), again).await.0, StatusCode::OK);
    let leads: Vec<Lead> = sqlx::query_as("select * from leads where project_id = $1").bind(s.project).fetch_all(&db).await.unwrap();
    assert_eq!(leads.len(), 1);
    assert_eq!((leads[0].full_name.as_str(), leads[0].company_name.as_str()), ("Dana Lee", "Kestrel Surveys"));
    assert!(leads[0].notes.contains("Visitor: Me again, pricing please"));

    // An address on the do-not-contact list stays on the conversation only.
    sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'nope.example', 'domain', 'manual')").bind(s.auth.org_id).execute(&db).await.unwrap();
    let (id3, token3) = s.start("203.0.113.3", "Hello").await;
    let blocked = json!({"conversation": id3, "token": token3, "email": "sam@nope.example"});
    assert_eq!(s.post("lead", "203.0.113.3", Some(ORIGIN), blocked).await.0, StatusCode::OK);
    let count: i64 = sqlx::query_scalar("select count(*) from leads").fetch_one(&db).await.unwrap();
    assert_eq!(count, 1);
    let kept: Option<String> = sqlx::query_scalar("select email::text from chat_conversations where id = $1").bind(id3.parse::<Uuid>().unwrap()).fetch_one(&db).await.unwrap();
    assert_eq!(kept.as_deref(), Some("sam@nope.example"));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_thanks_offers_the_projects_booking_link(db: PgPool) {
    let s = seed(&db).await;
    let thanks = |n: u8| {
        let s = &s;
        async move {
            let ip = format!("203.0.113.{}", 40 + n);
            let (id, token) = s.start(&ip, "Pricing?").await;
            let (_, body, _) = s.post("lead", &ip, Some(ORIGIN), json!({"conversation": id, "token": token, "email": format!("p{n}@kestrel.example")})).await;
            roles(&body)[0].1.clone()
        }
    };
    assert_eq!(thanks(1).await, "Thanks! Someone from the team will email you at p1@kestrel.example.");

    // The organization's booking page, when the project has no link of its own.
    let mailbox: Uuid = sqlx::query_scalar(
        "insert into mailboxes (org_id, email, from_name, smtp_host, smtp_username, smtp_password_enc) values ($1, 'sam@fieldnote.example', 'Sam', 'smtp.test', '', '') returning id",
    )
    .bind(s.auth.org_id)
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query("insert into booking_pages (org_id, mailbox_id, slug) values ($1, $2, 'sam-intro')").bind(s.auth.org_id).bind(mailbox).execute(&db).await.unwrap();
    assert!(thanks(2).await.ends_with("If you'd like to pick a time now, book a call here: http://localhost:3349/book/sam-intro"));

    sqlx::query("insert into project_settings (project_id, booking_link) values ($1, 'https://cal.example/fieldnote')").bind(s.project).execute(&db).await.unwrap();
    assert!(thanks(3).await.ends_with("book a call here: https://cal.example/fieldnote"));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn caps_and_rate_limits_hold_back_the_model(db: PgPool) {
    let s = seed(&db).await;
    let long = "x".repeat(chat::MAX_LENGTH + 1);
    assert_eq!(s.say("203.0.113.5", None, &long).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(s.say("203.0.113.5", None, "  <i></i> ").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(s.fake.calls(), 0);

    // The daily cap: past it, a fixed notice asks for an email and the model isn't called.
    sqlx::query("update chat_settings set daily_messages = 1 where project_id = $1").bind(s.project).execute(&db).await.unwrap();
    let (id, token) = s.start("203.0.113.6", "First").await;
    let (_, body) = s.say("203.0.113.6", Some((&id, &token)), "Second").await;
    assert_eq!(roles(&body)[1], ("notice".to_string(), CAPPED.to_string()));
    assert_eq!(body["ask_email"], true);
    assert_eq!(s.fake.calls(), 1);

    // The monthly spend cap, the same way. Each call holds $0.05 of it while it runs, so a burst can't pass the cap.
    sqlx::query("update chat_settings set daily_messages = 100, monthly_budget_micros = 54000 where project_id = $1").bind(s.project).execute(&db).await.unwrap();
    let (_, body) = s.say("203.0.113.6", Some((&id, &token)), "Third").await;
    assert_eq!(roles(&body)[1].0, "bot");
    // Two fake answers so far at $0.002 each. This brings the month to $0.01, too close to the cap for another hold.
    sqlx::query("update chat_messages set cost_micros = 8000 where id = (select max(id) from chat_messages)").execute(&db).await.unwrap();
    let (_, body) = s.say("203.0.113.6", Some((&id, &token)), "Fourth").await;
    assert_eq!(roles(&body)[1].1, CAPPED);
    assert_eq!(s.fake.calls(), 2);

    // Per conversation: PER_CONVERSATION messages a minute, wherever they come from.
    let (status, body) = s.say("203.0.113.7", Some((&id, &token)), "Fifth").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = s.say("203.0.113.8", Some((&id, &token)), "Sixth").await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("You're sending messages quickly. Wait a minute and try again.")));

    // Per IP: PER_IP messages a minute across its conversations.
    sqlx::query("update chat_settings set monthly_budget_micros = 100000000 where project_id = $1").bind(s.project).execute(&db).await.unwrap();
    let ip = "198.51.100.9";
    let mut chats = vec![];
    for _ in 0..3 {
        chats.push(s.start(ip, "Hi").await);
    }
    let mut statuses = vec![];
    for i in 0..(PER_IP - 3 + 1) as usize {
        let (id, token) = &chats[i % 3];
        statuses.push(s.say(ip, Some((id, token)), "More").await.0);
    }
    assert_eq!(statuses.pop(), Some(StatusCode::TOO_MANY_REQUESTS));
    assert!(statuses.iter().all(|st| *st == StatusCode::OK));

    // New conversations an hour from one IP, so clearing localStorage doesn't get around the per-conversation limits.
    let hash = visitors::ip_hash(&s.state.cfg.app_secret, "198.51.100.11".parse().unwrap());
    sqlx::query("insert into chat_conversations (project_id, token_hash, ip_hash) select $1, 'x', $2 from generate_series(1, $3)")
        .bind(s.project)
        .bind(&hash)
        .bind(CONVERSATIONS_PER_HOUR as i32)
        .execute(&db)
        .await
        .unwrap();
    let (status, body) = s.say("198.51.100.11", None, "Hi").await;
    assert_eq!((status, body["error"].as_str()), (StatusCode::TOO_MANY_REQUESTS, Some("You've started a lot of chats. Try again in an hour.")));

    // A conversation is full after MAX_VISITOR_MESSAGES.
    let (id, token) = s.start("198.51.100.10", "Hi").await;
    sqlx::query("insert into chat_messages (conversation_id, project_id, role, body) select $1, $2, 'visitor', 'x' from generate_series(2, $3)")
        .bind(id.parse::<Uuid>().unwrap())
        .bind(s.project)
        .bind(MAX_VISITOR_MESSAGES as i32)
        .execute(&db)
        .await
        .unwrap();
    let (status, body) = s.say("198.51.100.10", Some((&id, &token)), "One more").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn only_the_projects_domains_get_in(db: PgPool) {
    let s = seed(&db).await;
    let client = reqwest::Client::new();
    let script = client.get(format!("{}/c/{}.js", s.base, s.key)).send().await.unwrap();
    assert_eq!((script.status(), script.headers()["content-type"].to_str().unwrap()), (StatusCode::OK, "text/javascript; charset=utf-8"));
    assert_eq!(script.text().await.unwrap(), chat::WIDGET);
    assert_eq!(client.get(format!("{}/c/nope.js", s.base)).send().await.unwrap().status(), StatusCode::NOT_FOUND);

    let config = |origin: Option<&'static str>| {
        let mut req = client.get(format!("{}/c/{}/config", s.base, s.key)).header("x-forwarded-for", "203.0.113.20");
        if let Some(origin) = origin {
            req = req.header("origin", origin);
        }
        async move {
            let res = req.send().await.unwrap();
            let cors = res.headers().get("access-control-allow-origin").map(|v| v.to_str().unwrap().to_string());
            (res.status(), cors, res.json::<Value>().await.unwrap_or_default())
        }
    };
    let (status, cors, body) = config(Some(ORIGIN)).await;
    assert_eq!((status, cors.as_deref()), (StatusCode::OK, Some(ORIGIN)));
    assert_eq!((body["enabled"].as_bool(), body["title"].as_str(), body["accent"].as_str()), (Some(true), Some("Fieldnote"), Some("#111111")));
    assert!(body.get("faq").is_none(), "the FAQ stays on the server");
    for refused in [None, Some("https://evil.example"), Some("https://fieldnote.example.evil.example"), Some("null"), Some("file://fieldnote.example")] {
        let (status, cors, _) = config(refused).await;
        assert_eq!((status, cors), (StatusCode::FORBIDDEN, None), "{refused:?}");
    }
    let preflight = client.request(reqwest::Method::OPTIONS, format!("{}/c/{}/messages", s.base, s.key)).header("origin", ORIGIN).send().await.unwrap();
    assert_eq!(preflight.status(), StatusCode::NO_CONTENT);
    assert_eq!(preflight.headers()["access-control-allow-origin"], ORIGIN);
    let refused = client.request(reqwest::Method::OPTIONS, format!("{}/c/{}/messages", s.base, s.key)).header("origin", "https://evil.example").send().await.unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    let (status, _, cors) = s.post("messages", "203.0.113.21", Some("https://evil.example"), json!({"text": "hi"})).await;
    assert_eq!((status, cors), (StatusCode::FORBIDDEN, None));
    assert_eq!(s.post("messages", "203.0.113.21", None, json!({"text": "hi"})).await.0, StatusCode::FORBIDDEN);
    assert_eq!(s.post("messages", "203.0.113.21", Some(ORIGIN), json!("not a message")).await.0, StatusCode::BAD_REQUEST);
    // A subdomain of an allowed domain is allowed.
    let (status, _, cors) = s.post("messages", "203.0.113.21", Some("https://docs.fieldnote.example"), json!({"text": "hi"})).await;
    assert_eq!((status, cors.as_deref()), (StatusCode::OK, Some("https://docs.fieldnote.example")));

    // Turned off, the widget learns it from its settings and can't send.
    sqlx::query("update chat_settings set enabled = false where project_id = $1").bind(s.project).execute(&db).await.unwrap();
    assert_eq!(config(Some(ORIGIN)).await.2, json!({"enabled": false}));
    let (status, body, cors) = s.post("messages", "203.0.113.22", Some(ORIGIN), json!({"text": "hi"})).await;
    assert_eq!((status, body["error"].as_str(), cors.as_deref()), (StatusCode::FORBIDDEN, Some("Chat is turned off on this site"), Some(ORIGIN)));
    assert_eq!(s.fake.calls(), 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_teammate_takes_over_and_their_replies_reach_the_widget(db: PgPool) {
    let s = seed(&db).await;
    let mut events = s.state.events.subscribe();
    let (id, token) = s.start("203.0.113.30", "Is there a free trial?").await;
    let conversation: Uuid = id.parse().unwrap();
    let changed = events.recv().await.unwrap();
    assert_eq!((changed.kind.as_str(), changed.run_id, changed.data["project_id"].clone()), (chat::CHAT_EVENT, conversation, json!(s.project)));

    let Json(page) = list(State(s.state.clone()), s.auth.clone(), Path(s.project), Query(PageQuery { page: None, per_page: None })).await.unwrap();
    assert_eq!(page.total, 1);
    assert_eq!((page.items[0].live, page.items[0].taken_over, page.items[0].messages), (true, false, 2));

    let Json(after) = reply(State(s.state.clone()), s.auth.clone(), Path(conversation), Json(ReplyBody { body: "Hi, Sam here. Yes, 14 days.".into() })).await.unwrap();
    assert_eq!(after["chat"]["taken_over"], true);
    assert_eq!(after["messages"][2]["who"], "Sam Rivera");

    let (status, body, _) = s.post("poll", "203.0.113.30", Some(ORIGIN), json!({"conversation": id, "token": token, "after": 2})).await;
    assert_eq!(status, StatusCode::OK);
    let polled = body["messages"].as_array().unwrap();
    assert_eq!(polled.len(), 1);
    assert_eq!((polled[0]["role"].as_str(), polled[0]["who"].as_str(), polled[0]["body"].as_str()), (Some("team"), Some("Sam"), Some("Hi, Sam here. Yes, 14 days.")));
    let (status, _, _) = s.post("poll", "203.0.113.30", Some(ORIGIN), json!({"conversation": id, "token": "wrong", "after": 0})).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Taken over, the visitor's next message waits for the team instead of the model.
    let (_, body) = s.say("203.0.113.30", Some((&id, &token)), "Great, how do I start?").await;
    assert_eq!(roles(&body), [("visitor".to_string(), "Great, how do I start?".to_string())]);
    assert_eq!(s.fake.calls(), 1);

    // Handed back, the model answers again.
    let Json(back) = takeover(State(s.state.clone()), s.auth.clone(), Path(conversation), Json(TakeoverBody { on: false })).await.unwrap();
    assert_eq!(back["chat"]["taken_over"], false);
    let (_, body) = s.say("203.0.113.30", Some((&id, &token)), "Thanks").await;
    assert_eq!(roles(&body).len(), 2);
    assert_eq!(s.fake.calls(), 2);
    // Taken over while the model is answering, its answer is dropped and only its cost is kept.
    let (_, body) = s.say("203.0.113.30", Some((&id, &token)), "TAKE OVER NOW").await;
    assert_eq!(roles(&body), [("visitor".to_string(), "TAKE OVER NOW".to_string())]);
    let cost: i64 = sqlx::query_scalar("select cost_micros from chat_messages where body = 'TAKE OVER NOW'").fetch_one(&db).await.unwrap();
    assert_eq!(cost, 2000);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn chats_and_settings_stay_in_the_organization(db: PgPool) {
    let s = seed(&db).await;
    let (other, other_project, other_key) = project(&db, "Quarry", "quarry.example", "Gravel.").await;
    sqlx::query("update chat_settings set enabled = true where project_id = $1").bind(other_project).execute(&db).await.unwrap();
    let (id, token) = s.start("203.0.113.50", "Hello").await;
    let conversation: Uuid = id.parse().unwrap();

    // Another project's key doesn't open this conversation, even from that project's own site.
    let res = reqwest::Client::new()
        .post(format!("{}/c/{other_key}/poll", s.base))
        .header("origin", "https://quarry.example")
        .body(json!({"conversation": id, "token": token}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);

    let page = Query(PageQuery { page: None, per_page: None });
    assert!(matches!(list(State(s.state.clone()), other.clone(), Path(s.project), page).await, Err(AppError::NotFound)));
    assert!(matches!(thread(State(s.state.clone()), other.clone(), Path(conversation)).await, Err(AppError::NotFound)));
    let sneaky = reply(State(s.state.clone()), other.clone(), Path(conversation), Json(ReplyBody { body: "hi".into() })).await;
    assert!(matches!(sneaky, Err(AppError::NotFound)));
    assert!(matches!(takeover(State(s.state.clone()), other.clone(), Path(conversation), Json(TakeoverBody { on: true })).await, Err(AppError::NotFound)));
    assert!(matches!(settings(State(s.state.clone()), other.clone(), Path(s.project)).await, Err(AppError::NotFound)));
    let body = || Json(serde_json::from_value::<SettingsBody>(json!({"enabled": false})).unwrap());
    assert!(matches!(update_settings(State(s.state.clone()), other.clone(), Path(s.project), body()).await, Err(AppError::NotFound)));
    let Json(theirs) = list(State(s.state.clone()), other.clone(), Path(other_project), Query(PageQuery { page: None, per_page: None })).await.unwrap();
    assert_eq!(theirs.total, 0);
    let messages: i64 = sqlx::query_scalar("select count(*) from chat_messages where role = 'team'").fetch_one(&db).await.unwrap();
    assert_eq!(messages, 0);
    let on: bool = sqlx::query_scalar("select enabled from chat_settings where project_id = $1").bind(s.project).fetch_one(&db).await.unwrap();
    assert!(on);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn settings_are_checked_and_show_the_usage(db: PgPool) {
    let s = seed(&db).await;
    let update = |body: Value| update_settings(State(s.state.clone()), s.auth.clone(), Path(s.project), Json(serde_json::from_value(body).unwrap()));
    let Json(page) = update(json!({"accent": "#2563EB", "greeting": " Hi <b>there</b> ", "position": "left", "paths": ["pricing", "https://fieldnote.example/docs/"],
        "daily_messages": 50, "monthly_budget_micros": 5_500_000}))
    .await
    .unwrap();
    let saved = &page.settings;
    assert_eq!((saved.accent.as_str(), saved.greeting.as_str(), saved.position.as_str()), ("#2563eb", "Hi there", "left"));
    assert_eq!(saved.paths, ["/pricing", "/docs"]);
    assert_eq!((saved.daily_messages, saved.monthly_budget_micros), (50, 5_500_000));
    assert_eq!(page.usage.today_messages, 0);
    for bad in [json!({"accent": "blue"}), json!({"greeting": ""}), json!({"position": "top"}), json!({"paths": ["/a b"]}), json!({"daily_messages": 0}),
        json!({"monthly_budget_micros": -1}), json!({"faq": "x".repeat(chat::FAQ_CHARS + 1)})]
    {
        assert!(matches!(update(bad.clone()).await, Err(AppError::BadRequest(_))), "{bad}");
    }
}
