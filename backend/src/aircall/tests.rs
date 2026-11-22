use std::sync::Arc;

use axum::Router;
use chrono::{Timelike, Utc};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::EVENTS;
use super::fake::{self, Fake};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

struct Seeded {
    state: AppState,
    base: String,
    http: reqwest::Client,
    fake: Fake,
    project: Uuid,
    /// Sarah, +44 7700 900123, with an open call step. Tom, +33 6 12 34 56 78, without one.
    sarah: Uuid,
    tom: Uuid,
    jo: Uuid,
}

impl Seeded {
    /// Acme and Beta, each with an owner signed in as session "acme" or "beta", and Acme's project. Jo is on Acme's team and
    /// calls from Aircall as jo@acme.example.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let base_state = crate::autopilot::tests::state(db.clone());
        let cfg = Config { app_secret: "test-secret".into(), aircall_api_url: fake_url, public_url: "https://splee.example".into(), ..(*base_state.cfg).clone() };
        let state = AppState { cfg: Arc::new(cfg), ..base_state };
        let mut orgs = vec![];
        for name in ["acme", "beta"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                .bind(format!("{name}@{name}.example"))
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
            orgs.push(org);
        }
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(orgs[0]).fetch_one(&db).await.unwrap();
        let jo: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('jo@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(orgs[0]).bind(jo).execute(&db).await.unwrap();
        let lead = |name: &'static str, phone: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, phone, status) values ($1, $2, $3, 'contacted') returning id")
                .bind(project)
                .bind(name)
                .bind(phone)
                .fetch_one(&db)
        };
        let (sarah, tom) = (lead("Sarah Kim", "+447700900123").await.unwrap(), lead("Tom Ruiz", "+33612345678").await.unwrap());
        sqlx::query("insert into calls (project_id, lead_id, kind, step, status, to_number) values ($1, $2, 'sequence', 2, 'open', '+447700900123')")
            .bind(project)
            .bind(sarah)
            .execute(&db)
            .await
            .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        // Not following redirects, so a test sees where the recording sends the player.
        let http = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
        Self { state, base, http, fake, project, sarah, tom, jo }
    }

    fn db(&self) -> &PgPool {
        &self.state.db
    }

    async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn connect(&self) -> (u16, Value) {
        let body = json!({ "api_id": fake::API_ID, "api_token": format!(" {} ", fake::API_TOKEN) });
        self.call("acme", Method::PUT, &format!("/projects/{}/aircall", self.project), body).await
    }

    async fn path_token(&self) -> String {
        sqlx::query_scalar("select path_token from aircall_connections").fetch_one(self.db()).await.unwrap()
    }

    /// Posts an event to the webhook, signed with the token Aircall gave the nth webhook.
    async fn event(&self, path_token: &str, n: usize, event: &str, data: Value) -> (u16, Value) {
        let body = json!({ "resource": event.split('.').next(), "event": event, "timestamp": Utc::now().timestamp(), "token": format!("token-{n}"), "data": data });
        let res = self.http.post(format!("{}/aircall-webhooks/{path_token}", self.base)).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn calls_of(&self, lead: Uuid) -> Vec<Value> {
        sqlx::query_scalar("select to_jsonb(c) from calls c where lead_id = $1 order by created_at").bind(lead).fetch_all(self.db()).await.unwrap()
    }
}

/// An Aircall call between Jo and `digits`, `secs` long once answered, starting `ago` seconds ago.
fn aircall_call(id: i64, digits: &str, direction: &str, answered: bool, ago: i64, secs: i64) -> Value {
    let start = Utc::now().timestamp() - ago;
    json!({
        "id": id, "direction": direction, "status": "done", "raw_digits": digits, "started_at": start,
        "answered_at": if answered { json!(start + 10) } else { Value::Null }, "ended_at": start + 10 + secs, "duration": 10 + secs,
        "user": { "id": 7, "name": "Jo", "email": "Jo@Acme.example" }, "recording": null, "voicemail": null, "asset": null,
    })
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn connecting_registers_splees_webhook_and_disconnecting_deletes_it(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/aircall", s.project);
    assert_eq!(s.call("acme", Method::GET, &path, json!(null)).await, (200, Value::Null));
    let (code, body) = s.call("acme", Method::PUT, &path, json!({ "api_id": fake::API_ID, "api_token": "wrong" })).await;
    assert_eq!((code, body["error"].as_str().unwrap_or_default().contains("didn't accept")), (400, true), "{body}");
    assert!(s.fake.webhooks.lock().unwrap().is_empty());
    assert_eq!(s.call("acme", Method::PUT, &path, json!({ "api_id": "", "api_token": "x" })).await.0, 400);

    let (code, row) = s.connect().await;
    assert_eq!((code, row["calls"].as_i64(), row["error"].as_str()), (200, Some(0), Some("")), "{row}");
    let token = s.path_token().await;
    let first = s.fake.webhooks.lock().unwrap()[0].clone();
    assert_eq!(first, json!({ "custom_name": "Splee", "url": format!("https://splee.example/api/aircall-webhooks/{token}"), "events": EVENTS }));
    let stored: (String, String, String) =
        sqlx::query_as("select webhook_id, webhook_token_hash, credentials_enc from aircall_connections").fetch_one(s.db()).await.unwrap();
    assert_eq!(stored.0, "wh-1");
    assert!(!stored.1.contains("token-1") && !stored.2.contains(fake::API_TOKEN), "neither the webhook token nor the API token is stored as it is");
    assert_eq!(s.call("beta", Method::GET, &path, json!(null)).await.0, 404, "another organization's project");

    // Connecting again keeps the URL, registers a new webhook and deletes the old one.
    assert_eq!(s.connect().await.0, 200);
    assert_eq!(s.path_token().await, token);
    assert_eq!(s.fake.webhooks.lock().unwrap()[1]["url"], first["url"]);
    assert_eq!(*s.fake.deleted.lock().unwrap(), ["wh-1"]);
    assert_eq!(s.event(&token, 1, "call.created", json!({})).await.0, 401, "the old webhook's token is no good any more");
    assert_eq!(s.event(&token, 2, "call.created", json!({})).await, (200, json!({ "matched": false })));

    assert_eq!(s.call("acme", Method::DELETE, &path, json!(null)).await, (200, Value::Null));
    assert_eq!(*s.fake.deleted.lock().unwrap(), ["wh-1", "wh-2"]);
    assert_eq!(s.event(&token, 2, "call.created", json!({})).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn two_connects_at_once_leave_one_webhook_that_works(db: PgPool) {
    let s = Seeded::start(db).await;
    let (a, b) = tokio::join!(s.connect(), s.connect());
    assert_eq!((a.0, b.0), (200, 200));
    let token = s.path_token().await;
    let hooks = s.fake.webhooks.lock().unwrap().clone();
    assert!(hooks.len() == 2 && hooks.iter().all(|h| h["url"].as_str().unwrap().ends_with(&token)), "the second waits and keeps the URL: {hooks:?}");
    assert_eq!(*s.fake.deleted.lock().unwrap(), ["wh-1"]);
    assert_eq!(s.event(&token, 2, "call.created", json!({})).await, (200, json!({ "matched": false })));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn calls_with_leads_are_kept_once_and_their_transcripts_go_to_the_agent(db: PgPool) {
    let s = Seeded::start(db).await;
    assert_eq!(s.connect().await.0, 200);
    let token = s.path_token().await;
    assert_eq!(s.event(&token, 9, "call.ended", json!({})).await.0, 401);
    assert_eq!(s.event("nope", 1, "call.ended", json!({})).await.0, 404);

    // Jo calls Sarah, which closes her open call step with what happened.
    let mut to_sarah = aircall_call(101, "+44 7700 900123", "outbound", true, 600, 120);
    to_sarah["recording"] = json!("https://recordings.aircall.example/101.mp3");
    to_sarah["asset"] = json!("https://assets.aircall.io/recording/101");
    s.fake.calls.lock().unwrap().insert(101, to_sarah.clone());
    assert_eq!(s.event(&token, 1, "call.ended", to_sarah.clone()).await, (200, json!({ "matched": true })));
    assert_eq!(s.event(&token, 1, "call.ended", to_sarah).await, (200, json!({ "matched": true })), "a repeated event");
    let sarahs = s.calls_of(s.sarah).await;
    assert_eq!(sarahs.len(), 1, "{sarahs:?}");
    let c = &sarahs[0];
    assert_eq!(
        (c["kind"].as_str(), c["status"].as_str(), c["outcome"].as_str(), c["aircall_id"].as_i64(), c["duration_secs"].as_i64(), c["call_status"].as_str()),
        (Some("sequence"), Some("done"), Some("connected"), Some(101), Some(120), Some("completed"))
    );
    assert_eq!((c["called_by"].as_str(), c["recording_url"].as_str()), (Some(s.jo.to_string().as_str()), Some("https://assets.aircall.io/recording/101")));
    let contacted: bool = sqlx::query_scalar("select last_contacted_at is not null from leads where id = $1").bind(s.sarah).fetch_one(s.db()).await.unwrap();
    assert!(contacted);

    // Numbers that aren't a lead's, or that Aircall hides, aren't kept.
    assert_eq!(s.event(&token, 1, "call.ended", aircall_call(102, "+1 415 555 0100", "outbound", true, 60, 30)).await.1, json!({ "matched": false }));
    assert_eq!(s.event(&token, 1, "call.ended", aircall_call(103, "anonymous", "inbound", true, 60, 30)).await.1, json!({ "matched": false }));

    // Tom calls and nobody answers. His voicemail comes later, with the recording's page.
    let from_tom = aircall_call(104, "+33 6 12 34 56 78", "inbound", false, 300, 20);
    assert_eq!(s.event(&token, 1, "call.ended", from_tom.clone()).await.1, json!({ "matched": true }));
    let toms = s.calls_of(s.tom).await;
    assert_eq!(
        (toms[0]["kind"].as_str(), toms[0]["outcome"].as_str(), toms[0]["notes"].as_str(), toms[0]["duration_secs"].as_i64(), toms[0]["recording_url"].as_str()),
        (Some("manual"), Some("no_answer"), Some("They called and nobody answered."), None, Some(""))
    );
    let mut assets = from_tom;
    assets["voicemail"] = json!("https://recordings.aircall.example/104-voicemail.mp3");
    assets["asset"] = json!("https://assets.aircall.io/voicemail/104");
    assert_eq!(s.event(&token, 1, "call.comm_assets_generated", assets).await.1, json!({ "matched": true }));
    assert_eq!(s.calls_of(s.tom).await[0]["recording_url"], "https://assets.aircall.io/voicemail/104");

    // A transcript for a call Splee hasn't had the event for: the call is read from Aircall and kept first, then the agent
    // is asked for a summary, once.
    s.fake.calls.lock().unwrap().insert(105, aircall_call(105, "+447700900123", "outbound", true, 120, 60));
    let said = json!([{ "start_time": 1.2, "end_time": 2.0, "text": "Hi Sarah.", "participant_type": "internal", "user_id": 7 },
                      { "start_time": 2.5, "end_time": 3.0, "text": "Hi!", "participant_type": "external", "phone_number": "+447700900123" },
                      { "text": " ", "participant_type": "external" }]);
    s.fake.transcripts.lock().unwrap().insert(105, said);
    for _ in 0..2 {
        assert_eq!(s.event(&token, 1, "transcription.created", json!({ "id": 1, "call_id": "105", "type": "call" })).await.1, json!({ "matched": true }));
    }
    let sarahs = s.calls_of(s.sarah).await;
    let transcribed = sarahs.iter().find(|c| c["aircall_id"] == 105).unwrap();
    assert_eq!((transcribed["kind"].as_str(), transcribed["transcript"].as_str()), (Some("manual"), Some("Us: Hi Sarah.\nLead: Hi!")));
    let runs: Vec<Value> = sqlx::query_scalar("select input from agent_runs where kind = 'summarize_call'").fetch_all(s.db()).await.unwrap();
    assert_eq!(runs, [json!({ "call_id": transcribed["id"] })]);
    // A call without a transcript in Aircall stays without one.
    assert_eq!(s.event(&token, 1, "transcription.created", json!({ "call_id": 101 })).await.1, json!({ "matched": true }));
    assert_eq!(s.calls_of(s.sarah).await[0]["transcript"], "");

    let (_, row) = s.call("acme", Method::GET, &format!("/projects/{}/aircall", s.project), json!(null)).await;
    assert_eq!((row["calls"].as_i64(), row["error"].as_str(), row["last_event_at"].is_string()), (Some(3), Some(""), true), "{row}");

    // The player asks Aircall for a fresh link to the recording.
    let res = s.http.get(format!("{}/calls/{}/recording", s.base, c["id"].as_str().unwrap())).header("cookie", "splee_session=acme").send().await.unwrap();
    assert_eq!((res.status().as_u16(), res.headers()["location"].to_str().unwrap()), (307, "https://recordings.aircall.example/101.mp3"));
    let res = s.http.get(format!("{}/calls/{}/recording", s.base, c["id"].as_str().unwrap())).header("cookie", "splee_session=beta").send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404, "another organization's call");
}

/// The fixed-offset zone where it's `hour` o'clock now, so calling hours don't depend on when the tests run.
fn zone_at(hour: i64) -> String {
    let mut offset = hour - i64::from(Utc::now().hour());
    if offset > 14 {
        offset -= 24;
    } else if offset < -12 {
        offset += 24;
    }
    // Etc zones have the sign backwards: Etc/GMT-3 is three hours ahead of UTC.
    if offset >= 0 { format!("Etc/GMT-{offset}") } else { format!("Etc/GMT+{}", -offset) }
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_call_starts_in_the_callers_aircall_workspace_after_the_usual_checks(db: PgPool) {
    let s = Seeded::start(db).await;
    // A US number, which keeps several time zones, so the project's decides the calling hours.
    let dan: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, phone, status) values ($1, 'Dan Ross', '+14155550134', 'contacted') returning id")
        .bind(s.project)
        .fetch_one(s.db())
        .await
        .unwrap();
    let call: Uuid = sqlx::query_scalar("insert into calls (project_id, lead_id, kind, step, status, to_number) values ($1, $2, 'sequence', 2, 'open', '+14155550134') returning id")
        .bind(s.project)
        .bind(dan)
        .fetch_one(s.db())
        .await
        .unwrap();
    let zone = |hour: i64| {
        let upsert = "insert into project_settings (project_id, timezone) values ($1, $2) on conflict (project_id) do update set timezone = excluded.timezone";
        sqlx::query(upsert).bind(s.project).bind(zone_at(hour)).execute(s.db())
    };
    zone(12).await.unwrap();
    let path = format!("/calls/{call}/aircall-dial");
    let dial = || s.call("acme", Method::POST, &path, json!({}));

    let (code, body) = dial().await;
    assert_eq!((code, body["error"].as_str()), (409, Some("Connect Aircall on the Integrations page to call from it.")));
    assert_eq!(s.connect().await.0, 200);
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("Nobody in your Aircall has the email acme@acme.example"), "{body}");

    s.fake.users.lock().unwrap().insert("acme@acme.example".into(), json!({ "id": 7, "email": "acme@acme.example", "available": true }));
    assert_eq!(dial().await, (200, json!({ "to": "+14155550134" })));
    assert_eq!(*s.fake.dialed.lock().unwrap(), [(7, json!({ "to": "+14155550134" }))]);
    s.fake.unavailable.lock().unwrap().insert(7);
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("unavailable or on a call"), "{body}");
    s.fake.unavailable.lock().unwrap().clear();

    // The same checks as a call from the browser.
    assert_eq!(s.call("beta", Method::POST, &path, json!({})).await.0, 404);
    zone(22).await.unwrap();
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("Call between 8:00 and 20:00 their time"), "{body}");
    zone(12).await.unwrap();

    // Marked once Aircall took it, so the call it sends back closes this step. See the next test.
    let marked = "select u.email::text, c.aircall_dialed_at is not null from calls c left join users u on u.id = c.aircall_dialed_by where c.id = $1";
    let marked: (Option<String>, bool) = sqlx::query_as(marked).bind(call).fetch_one(s.db()).await.unwrap();
    assert_eq!(marked, (Some("acme@acme.example".into()), true));
    sqlx::query("update calls set status = 'done' where id = $1").bind(call).execute(s.db()).await.unwrap();
    assert_eq!(dial().await.1["error"], "This call was already logged, skipped or cancelled.");
    assert_eq!(s.fake.dialed.lock().unwrap().len(), 1, "nothing else reached Aircall");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_call_closes_the_step_its_caller_put_in_aircall_just_before(db: PgPool) {
    let s = Seeded::start(db).await;
    assert_eq!(s.connect().await.0, 200);
    let token = s.path_token().await;
    // Dan and Dana share a number, and the team spoke to Dana last, so a call nobody started from Splee goes to her.
    let lead = |name: &'static str, contacted: bool| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into leads (project_id, full_name, phone, status, last_contacted_at)
             values ($1, $2, '+14155550134', 'contacted', case when $3 then now() end) returning id",
        )
        .bind(s.project)
        .bind(name)
        .bind(contacted)
        .fetch_one(s.db())
    };
    let (dan, dana) = (lead("Dan Ross", false).await.unwrap(), lead("Dana Ross", true).await.unwrap());
    // An open step due `due` minutes from now, which Jo put in Aircall `dialed` minutes ago.
    let step = |lead: Uuid, due: i32, dialed: Option<i32>| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into calls (project_id, lead_id, kind, status, due_at, aircall_dialed_by, aircall_dialed_at)
             values ($1, $2, 'manual', 'open', now() + make_interval(mins => $3), $4, now() - make_interval(mins => $5)) returning id",
        )
        .bind(s.project)
        .bind(lead)
        .bind(due)
        .bind(dialed.map(|_| s.jo))
        .bind(dialed)
        .fetch_one(s.db())
    };
    let dans_first = step(dan, -1440, None).await.unwrap();
    let dans_second = step(dan, 0, Some(20)).await.unwrap();
    let danas_first = step(dana, -1440, None).await.unwrap();
    let danas_second = step(dana, 0, Some(5)).await.unwrap();
    // Jo is calling Dana's first step from the browser, which marks it the way Twilio's dial does.
    sqlx::query("update calls set called_by = $2, called_at = now(), to_number = '+14155550134', twilio_sid = 'CA1', call_status = 'in-progress' where id = $1")
        .bind(danas_first)
        .bind(s.jo)
        .execute(s.db())
        .await
        .unwrap();
    let ended = |id: i64, ago: i64| aircall_call(id, "+1 415-555-0134", "outbound", false, ago, 30);
    let status = |id: Uuid| sqlx::query_as::<_, (String, Option<i64>)>("select status, aircall_id from calls where id = $1").bind(id).fetch_one(s.db());

    // A call that started 15 minutes ago, arriving late, closes the step put in Aircall before it, not the newer one.
    assert_eq!(s.event(&token, 1, "call.ended", ended(201, 900)).await.1, json!({ "matched": true }));
    assert_eq!(status(dans_second).await.unwrap(), ("done".into(), Some(201)));
    // One that started a minute ago closes Dana's step put in Aircall, not the earlier one Jo is calling through Twilio.
    assert_eq!(s.event(&token, 1, "call.ended", ended(202, 60)).await.1, json!({ "matched": true }));
    assert_eq!(status(danas_second).await.unwrap(), ("done".into(), Some(202)));
    assert_eq!(status(danas_first).await.unwrap(), ("open".into(), None));

    // A step put in Aircall two hours ago and never called doesn't take a call now, which goes to Dana. Her open step is being
    // called through Twilio and stays for Jo to log, so the call is kept on its own.
    sqlx::query("update calls set aircall_dialed_by = $2, aircall_dialed_at = now() - interval '2 hours' where id = $1")
        .bind(dans_first)
        .bind(s.jo)
        .execute(s.db())
        .await
        .unwrap();
    assert_eq!(s.event(&token, 1, "call.ended", ended(203, 60)).await.1, json!({ "matched": true }));
    assert_eq!(status(dans_first).await.unwrap(), ("open".into(), None));
    assert_eq!(status(danas_first).await.unwrap(), ("open".into(), None));
    let kept: (Uuid, String) = sqlx::query_as("select lead_id, kind from calls where aircall_id = 203").fetch_one(s.db()).await.unwrap();
    assert_eq!(kept, (dana, "manual".into()));
}
