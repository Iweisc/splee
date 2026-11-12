use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use axum::Router;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::fake::{self, AGENCIES, DENTISTS, Fake, INSTANTLY_KEY, LEMLIST_KEY, SMARTLEAD_KEY};
use super::{Client, Tool, plain_text, splee_fields};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

fn config(fake_url: &str) -> Config {
    Config {
        database_url: String::new(),
        app_secret: "test-secret".into(),
        public_url: "https://splee.test".into(),
        unsubscribe_url: String::new(),
        bind_addr: String::new(),
        agent_model: String::new(),
        agent_concurrency: 0,
        sending_enabled: false,
        bird_bin: None,
        open_signup: true,
        public_agent: false,
        porkbun_api_url: String::new(),
        twilio_api_url: String::new(),
        outreach_api_url: fake_url.into(),
        postmaster_api_url: String::new(),
        bots_api_url: String::new(),
        operator_emails: Vec::new(),
        google: None,
        microsoft: None,
        tumblr: None,
        salesforce: None,
        zoho: None,
        meta: None,
        graph_api_url: String::new(),
        crm_api_url: String::new(),
        payments_api_url: String::new(),
        meetings_api_url: String::new(),
        webflow_api_url: String::new(),
        linkedin_ads: None,
        google_ads_developer_token: None,
        ads_api_url: String::new(),
        ipinfo_token: None,
        slack: None,
    }
}

struct Seeded {
    state: AppState,
    base: String,
    http: reqwest::Client,
    fake: Fake,
    fake_url: String,
    acme: Uuid,
    beta: Uuid,
    /// Acme's project, which already has bo@acme.com as a lead Splee never emailed.
    project: Uuid,
}

impl Seeded {
    /// Two organizations, Acme and Beta, each with an owner signed in as session "acme" or "beta" and a project.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let state = AppState { db: db.clone(), cfg: Arc::new(config(&fake_url)), events: tokio::sync::broadcast::channel(1).0 };
        let mut ids = vec![];
        for name in ["acme", "beta"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name, referral_code) values ($1, $1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                .bind(format!("{name}@example.test"))
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                .bind(sha256_hex(name))
                .bind(user)
                .bind(org)
                .execute(&db)
                .await
                .unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Fieldnote', '{\"country_code\": \"GB\"}') returning id")
                .bind(org)
                .fetch_one(&db)
                .await
                .unwrap();
            ids.push((org, project));
        }
        let ((acme, project), (beta, _)) = (ids[0], ids[1]);
        sqlx::query("insert into leads (project_id, source, email, first_name) values ($1, 'import', 'bo@acme.com', 'Bo')").bind(project).execute(&db).await.unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { state, base, http: reqwest::Client::new(), fake, fake_url, acme, beta, project }
    }

    fn db(&self) -> &PgPool {
        &self.state.db
    }

    async fn call(&self, session: &str, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        self.call("acme", reqwest::Method::POST, path, body).await
    }

    /// Queues an import through the API and runs it the way the worker does.
    async fn import(&self, tool: &str, key: &str, campaigns: Value, blocklist: bool) -> Value {
        let path = format!("/projects/{}/outreach/imports", self.project);
        let (code, queued) = self.post(&path, json!({ "tool": tool, "api_key": key, "campaigns": campaigns, "blocklist": blocklist })).await;
        assert_eq!(code, 200, "{queued}");
        let id: Uuid = queued["id"].as_str().unwrap().parse().unwrap();
        let stored: String = sqlx::query_scalar("select api_key_enc from outreach_imports where id = $1").bind(id).fetch_one(self.db()).await.unwrap();
        assert!(!stored.is_empty() && !stored.contains(key), "the key waits encrypted");
        sqlx::query("update outreach_imports set status = 'running' where id = $1").bind(id).execute(self.db()).await.unwrap();
        super::run(&self.state, id).await.unwrap();
        let (code, list) = self.call("acme", reqwest::Method::GET, &path, json!({})).await;
        assert_eq!(code, 200);
        list.as_array().unwrap().iter().find(|i| i["id"] == queued["id"]).unwrap().clone()
    }

    async fn lead(&self, email: &str) -> (String, Option<chrono::DateTime<chrono::Utc>>, String, String) {
        sqlx::query_as("select status, last_contacted_at, notes, coalesce(personalization ->> 'hook', '') from leads where project_id = $1 and email = $2")
            .bind(self.project)
            .bind(email)
            .fetch_one(self.db())
            .await
            .unwrap()
    }

    async fn suppressions(&self, org: Uuid) -> Vec<(String, String, String)> {
        sqlx::query_as("select value::text, kind, reason from suppressions where org_id = $1 order by value")
            .bind(org)
            .fetch_all(self.db())
            .await
            .unwrap()
    }
}

// The database tests are ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See
// CONTRIBUTING.md.
fn picked(id: &str, name: &str) -> Value {
    json!([{ "id": id, "name": name, "leads": 7 }])
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_key_is_checked_by_listing_the_campaigns(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/outreach/campaigns", s.project);
    for (tool, label) in [("instantly", "Instantly"), ("smartlead", "Smartlead"), ("lemlist", "lemlist")] {
        let (code, body) = s.post(&path, json!({ "tool": tool, "api_key": "wrong" })).await;
        assert_eq!((code, body["error"].as_str().unwrap()), (400, format!("{label} didn't accept that API key. Copy it again from {label}'s settings.").as_str()));
    }
    let (code, body) = s.post(&path, json!({ "tool": "instantly", "api_key": "  " })).await;
    assert_eq!((code, body["error"].as_str().unwrap()), (400, "Paste your Instantly API key"));
    let (code, _) = s.post(&path, json!({ "tool": "mailchimp", "api_key": "x" })).await;
    assert_eq!(code, 400);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaigns_are_listed_across_pages_with_their_lead_counts(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/outreach/campaigns", s.project);

    let (code, body) = s.post(&path, json!({ "tool": "instantly", "api_key": INSTANTLY_KEY })).await;
    assert_eq!(code, 200, "{body}");
    assert_eq!(body["campaigns"], json!([
        {"id": DENTISTS, "name": "Dentists UK", "status": "Active", "leads": 7},
        {"id": AGENCIES, "name": "Agencies", "status": "Paused", "leads": 0},
    ]));
    assert_eq!(s.fake.hits(&format!("/instantly/campaigns?limit=100&starting_after={DENTISTS}")), 1, "the second page asks for what follows the first");

    let (code, body) = s.post(&path, json!({ "tool": "smartlead", "api_key": SMARTLEAD_KEY })).await;
    assert_eq!((code, &body["campaigns"]), (200, &json!([{"id": "101", "name": "SaaS founders", "status": "Active", "leads": 3}])));

    let (code, body) = s.post(&path, json!({ "tool": "lemlist", "api_key": LEMLIST_KEY })).await;
    let campaigns = body["campaigns"].as_array().unwrap();
    assert_eq!((code, campaigns.len()), (200, 101), "a full page of 100 and one more");
    assert_eq!((&campaigns[0]["leads"], &campaigns[100]["id"], &campaigns[100]["leads"]), (&json!(2), &json!("cam_101"), &json!(0)));
    assert_eq!(s.fake.hits("/lemlist/campaigns/reports"), 3, "reports are asked for 50 campaigns at a time");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_instantly_campaign_comes_over_as_a_draft_with_its_leads(db: PgPool) {
    let s = Seeded::start(db).await;
    let import = s.import("instantly", INSTANTLY_KEY, picked(DENTISTS, "Dentists UK"), true).await;
    assert_eq!((&import["status"], &import["blocklist_added"], &import["error"]), (&json!("done"), &json!(2), &json!("")), "{import}");
    let c = &import["campaigns"][0];
    // Ana, Cy, Di and Ed are new. Bo was already a lead. Fay is at a blocklisted domain, and Di and Ed go on the list.
    assert_eq!((&c["status"], &c["steps"], &c["added"], &c["duplicates"], &c["suppressed"], &c["invalid"]),
        (&json!("done"), &json!(3), &json!(4), &json!(1), &json!(3), &json!(1)), "{c}");
    let key: String = sqlx::query_scalar("select api_key_enc from outreach_imports").fetch_one(s.db()).await.unwrap();
    assert_eq!(key, "", "the key is gone once the import ends");

    let segment: Uuid = c["segment_id"].as_str().unwrap().parse().unwrap();
    let (name, status, from): (String, String, String) = sqlx::query_as("select name, status, imported_from from segments where id = $1").bind(segment).fetch_one(s.db()).await.unwrap();
    assert_eq!((name.as_str(), status.as_str(), from.as_str()), ("Dentists UK", "draft", "instantly"));
    let emails: i64 = sqlx::query_scalar("select count(*) from email_messages").fetch_one(s.db()).await.unwrap();
    assert_eq!(emails, 0, "nothing is written or scheduled");

    let (code, steps) = s.call("acme", reqwest::Method::GET, &format!("/segments/{segment}/steps"), json!({})).await;
    assert_eq!((code, &steps["imported_from"]), (200, &json!("Instantly")));
    assert_eq!(steps["steps"], json!([
        {"position": 0, "delay_days": 0, "subject": "{company} x Fieldnote", "body": "Hi {first_name},\n\n{{personalization}} Saw {company} is hiring.\n{sender_name}"},
        {"position": 1, "delay_days": 3, "subject": "", "body": "Any thoughts, {first_name}?"},
        {"position": 2, "delay_days": 4, "subject": "", "body": "Last note from me."},
    ]));

    let ana = s.lead("ana@delta.io").await;
    assert_eq!((ana.0.as_str(), ana.1, ana.2.as_str(), ana.3.as_str()), ("new", None, "From Instantly:\nChairs: 6", "Loved the clinic tour video."));
    let (title, city, domain, segment_of): (String, String, String, Uuid) =
        sqlx::query_as("select title, city, company_domain::text, segment_id from leads where email = 'ana@delta.io'").fetch_one(s.db()).await.unwrap();
    assert_eq!((title.as_str(), city.as_str(), domain.as_str(), segment_of), ("Head of Ops", "Leeds", "delta.io", segment));
    let at = |day: u32| Some(chrono::DateTime::parse_from_rfc3339(&format!("2026-10-{day:02}T09:00:00Z")).unwrap().to_utc());
    assert_eq!(s.lead("bo@acme.com").await.0.as_str(), "contacted", "a duplicate Splee never emailed takes what Instantly knows");
    assert_eq!(s.lead("bo@acme.com").await.1, at(1));
    assert_eq!((s.lead("cy@corp.com").await.0.as_str(), s.lead("cy@corp.com").await.1), ("hot", at(2)));
    assert_eq!((s.lead("di@bounce.com").await.0.as_str(), s.lead("di@bounce.com").await.1), ("bounced", at(3)));
    assert_eq!(s.lead("ed@unsub.com").await.0.as_str(), "unsubscribed");
    let fay: i64 = sqlx::query_scalar("select count(*) from leads where email = 'fay@blocked.io'").fetch_one(s.db()).await.unwrap();
    assert_eq!(fay, 0);
    assert_eq!(s.suppressions(s.acme).await, [
        ("blocked.io".to_string(), "domain".to_string(), "manual".to_string()),
        ("di@bounce.com".to_string(), "email".to_string(), "bounce".to_string()),
        ("ed@unsub.com".to_string(), "email".to_string(), "unsubscribe".to_string()),
        ("nope@example.com".to_string(), "email".to_string(), "manual".to_string()),
    ]);

    // The writer's first emails go only to the lead Instantly never emailed.
    let firsts = crate::agent::write::leads_without_initial(s.db(), s.project, Some(segment)).await.unwrap();
    assert_eq!(firsts.iter().map(|l| l.email.as_deref().unwrap()).collect::<Vec<_>>(), ["ana@delta.io"]);

    // Beta sees none of it.
    assert!(s.suppressions(s.beta).await.is_empty());
    let (code, _) = s.call("beta", reqwest::Method::GET, &format!("/segments/{segment}/steps"), json!({})).await;
    assert_eq!(code, 404);
    let (code, _) = s.call("beta", reqwest::Method::GET, &format!("/projects/{}/outreach/imports", s.project), json!({})).await;
    assert_eq!(code, 404);
    let (code, _) = s
        .call("beta", reqwest::Method::POST, &format!("/projects/{}/outreach/imports", s.project), json!({ "tool": "instantly", "api_key": INSTANTLY_KEY, "campaigns": picked(DENTISTS, "x") }))
        .await;
    assert_eq!(code, 404);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn follow_ups_keep_the_campaigns_own_schedule(db: PgPool) {
    let s = Seeded::start(db).await;
    // The project itself has no follow-ups. Dentists UK's second email waits 3 days.
    sqlx::query("insert into project_settings (project_id, paused, follow_ups) values ($1, false, '[]')").bind(s.project).execute(s.db()).await.unwrap();
    let import = s.import("instantly", INSTANTLY_KEY, picked(DENTISTS, "Dentists UK"), false).await;
    let segment: Uuid = import["campaigns"][0]["segment_id"].as_str().unwrap().parse().unwrap();
    let ana: Uuid = sqlx::query_scalar("update leads set status = 'contacted' where email = 'ana@delta.io' returning id").fetch_one(s.db()).await.unwrap();
    let send = |days: i32| {
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, sent_at)
             values ($1, $2, $3, 'outbound', 'initial', 0, 'Hi', 'Hello', 'sent', now() - make_interval(days => $4))",
        )
        .bind(s.project)
        .bind(ana)
        .bind(segment)
        .bind(days)
        .execute(s.db())
    };
    let queued = || sqlx::query_scalar::<_, i64>("select count(*) from agent_runs where kind = 'write_emails' and input -> 'lead_ids' ? $1").bind(ana.to_string()).fetch_one(s.db());

    send(1).await.unwrap();
    crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
    assert_eq!(queued().await.unwrap(), 0, "not due for two more days");
    sqlx::query("update email_messages set sent_at = now() - interval '3 days' where lead_id = $1").bind(ana).execute(s.db()).await.unwrap();
    crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
    assert_eq!(queued().await.unwrap(), 1);
    // After its last imported email it stops, even though the project now has follow-ups of its own.
    let rules = json!(vec![json!({ "enabled": true, "delay_days": 1 }); 5]);
    sqlx::query("update project_settings set follow_ups = $2 where project_id = $1").bind(s.project).bind(rules).execute(s.db()).await.unwrap();
    for step in [1, 2] {
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, direction, kind, step, subject, body, status, sent_at)
             values ($1, $2, $3, 'outbound', 'follow_up', $4, '', 'Hello again', 'sent', now() - interval '10 days')",
        )
        .bind(s.project)
        .bind(ana)
        .bind(segment)
        .bind(step)
        .execute(s.db())
        .await
        .unwrap();
    }
    crate::mail::sender::schedule_follow_ups(&s.state).await.unwrap();
    assert_eq!(queued().await.unwrap(), 1);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn importing_again_adds_only_what_is_new(db: PgPool) {
    let s = Seeded::start(db).await;
    let first = s.import("instantly", INSTANTLY_KEY, picked(DENTISTS, "Dentists UK"), true).await;
    let again = s.import("instantly", INSTANTLY_KEY, picked(DENTISTS, "Dentists UK"), true).await;
    let c = &again["campaigns"][0];
    assert_eq!((&again["blocklist_added"], &c["added"], &c["duplicates"], &c["suppressed"]), (&json!(0), &json!(0), &json!(5), &json!(1)), "{c}");
    assert_eq!(c["segment_id"], first["campaigns"][0]["segment_id"], "the same draft is filled");
    let counts: (i64, i64, i64, i64) = sqlx::query_as(
        "select (select count(*) from segments), (select count(*) from segment_steps), (select count(*) from leads), (select count(*) from suppressions)",
    )
    .fetch_one(s.db())
    .await
    .unwrap();
    assert_eq!(counts, (1, 3, 5, 4));

    s.fake.instantly_leads.lock().unwrap().push((DENTISTS.to_string(), json!({"id": "l8", "email": "gil@new.io", "status": 1})));
    let third = s.import("instantly", INSTANTLY_KEY, picked(DENTISTS, "Dentists UK"), false).await;
    assert_eq!((&third["campaigns"][0]["added"], &third["campaigns"][0]["duplicates"]), (&json!(1), &json!(5)));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn smartlead_and_lemlist_campaigns_come_over(db: PgPool) {
    let s = Seeded::start(db).await;
    let smartlead = s.import("smartlead", SMARTLEAD_KEY, picked("101", "SaaS founders"), false).await;
    let c = &smartlead["campaigns"][0];
    assert_eq!((&c["added"], &c["suppressed"], &c["steps"]), (&json!(3), &json!(1), &json!(2)), "{c}");
    assert_eq!(s.fake.hits("/smartlead/campaigns/101/leads"), 2, "three leads at two a page");
    assert_eq!(s.fake.hits("/smartlead/leads/get-domain-block-list"), 0, "the blocklist is left alone when unticked");
    let sam = s.lead("sam@saas.io").await;
    assert_eq!((sam.0.as_str(), sam.1.map(|t| t.to_rfc3339())), ("contacted", Some("2026-10-05T10:00:00+00:00".to_string())));
    assert_eq!(s.lead("tia@saas.io").await.0.as_str(), "new");
    assert_eq!(s.lead("uma@gone.io").await.0.as_str(), "bounced");
    let segment: Uuid = c["segment_id"].as_str().unwrap().parse().unwrap();
    let steps: Vec<(i32, String, String)> =
        sqlx::query_as("select delay_days, subject, body from segment_steps where segment_id = $1 order by position").bind(segment).fetch_all(s.db()).await.unwrap();
    assert_eq!(steps, [
        (0, "Quick question, {first_name}".to_string(), "Hi {first_name},\n\nHow does {company} handle {{pain}}?".to_string()),
        (2, String::new(), "Following up, {first_name}.".to_string()),
    ]);

    let lemlist = s.import("lemlist", LEMLIST_KEY, picked("cam_1", "Campaign 1"), true).await;
    let c = &lemlist["campaigns"][0];
    assert_eq!((&lemlist["blocklist_added"], &c["added"], &c["suppressed"]), (&json!(1), &json!(1), &json!(1)), "{lemlist}");
    let vic = s.lead("vic@lem.io").await;
    assert_eq!((vic.0.as_str(), vic.1.is_some(), vic.3.as_str()), ("replied", true, "Congrats on the round."));
    let segment: Uuid = c["segment_id"].as_str().unwrap().parse().unwrap();
    let delays: Vec<i32> = sqlx::query_scalar("select delay_days from segment_steps where segment_id = $1 order by position").bind(segment).fetch_all(s.db()).await.unwrap();
    assert_eq!(delays, [0, 3], "the LinkedIn step's day adds to the next email's wait");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_busy_tool_is_waited_for(db: PgPool) {
    let s = Seeded::start(db).await;
    s.fake.throttle.store(2, Ordering::SeqCst);
    let client = Client::new(Tool::Instantly, &s.fake_url, INSTANTLY_KEY).unwrap();
    let started = Instant::now();
    let campaigns = client.campaigns().await.unwrap();
    assert_eq!(campaigns.len(), 2);
    assert!(started.elapsed() >= Duration::from_secs(2), "Retry-After: 1, twice");
    assert_eq!(s.fake.hits("/instantly/campaigns?limit=100"), 4, "three tries for the first page, then the second");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn one_import_runs_at_a_time_per_project(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/outreach/imports", s.project);
    let body = json!({ "tool": "instantly", "api_key": INSTANTLY_KEY, "campaigns": picked(DENTISTS, "Dentists UK") });
    assert_eq!(s.post(&path, body.clone()).await.0, 200);
    let (code, err) = s.post(&path, body).await;
    assert_eq!((code, err["error"].as_str().unwrap()), (409, "An import is already running for this project. Wait for it to finish."));
    let (code, _) = s.post(&path, json!({ "tool": "instantly", "api_key": INSTANTLY_KEY, "campaigns": [{"id": "../admin", "name": "x"}] })).await;
    assert_eq!(code, 400);
}

#[test]
fn variables_become_splee_fields_and_unknown_ones_stay() {
    assert_eq!(splee_fields("Hi {{firstName}} at {{ company_name }}, {{icebreaker}} {{RANDOM | a | b}} {{first_name"), "Hi {first_name} at {company}, {{icebreaker}} {{RANDOM | a | b}} {{first_name");
    assert_eq!(splee_fields("{{lastName}} {{jobTitle}} {{city}} {{senderName}}"), "{last_name} {title} {city} {sender_name}");
    assert_eq!(plain_text("<p>One &amp; two</p><p>Three<br>four</p>"), "One & two\n\nThree\nfour");
    assert_eq!(plain_text("Plain\ntext"), "Plain\ntext");
}
