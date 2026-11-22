use std::sync::Arc;

use axum::Router;
use chrono::{Timelike, Utc};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::STATES;
use super::fake::{self, Fake};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

struct Seeded {
    base: String,
    http: reqwest::Client,
    fake: Fake,
    fake_url: String,
    db: PgPool,
    project: Uuid,
    /// Sarah, +44 7700 900123, with an open call step. Tom, +33 6 12 34 56 78, without one.
    sarah: Uuid,
    tom: Uuid,
    jo: Uuid,
}

impl Seeded {
    /// Acme and Beta, each with an owner signed in as session "acme" or "beta", and Acme's project. Jo is on Acme's team and
    /// calls from Dialpad as jo@acme.example.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let base_state = crate::autopilot::tests::state(db.clone());
        let cfg = Config { app_secret: "test-secret".into(), dialpad_api_url: fake_url.clone(), public_url: "https://splee.example".into(), ..(*base_state.cfg).clone() };
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
        Self { base, http: reqwest::Client::new(), fake, fake_url, db, project, sarah, tom, jo }
    }

    async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn connect(&self) -> (u16, Value) {
        self.call("acme", Method::PUT, &format!("/projects/{}/dialpad", self.project), json!({ "api_key": format!(" {} ", fake::API_KEY) })).await
    }

    async fn path_token(&self) -> String {
        sqlx::query_scalar("select path_token from dialpad_connections").fetch_one(&self.db).await.unwrap()
    }

    /// The secret Splee gave the nth webhook it made.
    fn secret(&self, n: usize) -> String {
        self.fake.webhooks.lock().unwrap()[n - 1]["secret"].as_str().unwrap().to_string()
    }

    /// Posts `body` to the webhook as it is.
    async fn post(&self, path_token: &str, body: String) -> (u16, Value) {
        let res = self.http.post(format!("{}/dialpad-webhooks/{path_token}", self.base)).body(body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    /// Posts an event signed with the nth webhook's secret.
    async fn event(&self, path_token: &str, n: usize, event: &Value) -> (u16, Value) {
        self.post(path_token, fake::sign(&self.secret(n), event)).await
    }

    async fn calls_of(&self, lead: Uuid) -> Vec<Value> {
        sqlx::query_scalar("select to_jsonb(c) from calls c where lead_id = $1 order by created_at").bind(lead).fetch_all(&self.db).await.unwrap()
    }
}

/// A Dialpad call event between Jo and `number`, `secs` long once answered, starting `ago` seconds ago, in the hangup state.
fn dialpad_call(id: i64, number: &str, direction: &str, answered: bool, ago: i64, secs: i64) -> Value {
    let start = Utc::now().timestamp_millis() - ago * 1000;
    json!({
        "call_id": id, "master_call_id": null, "state": "hangup", "direction": direction, "external_number": number,
        "internal_number": "+15550100200", "date_started": start, "date_connected": if answered { json!(start + 10_000) } else { Value::Null },
        "date_ended": start + 10_000 + secs * 1000, "duration": (secs * 1000) as f64, "custom_data": null,
        "target": { "type": "user", "id": 5111, "name": "Jo", "email": "Jo@Acme.example", "phone": "+15550100200" },
        "contact": { "phone": number, "type": "local", "name": "" },
    })
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn connecting_registers_a_signed_webhook_and_disconnecting_deletes_it(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/dialpad", s.project);
    assert_eq!(s.call("acme", Method::GET, &path, json!(null)).await, (200, Value::Null));
    let (code, body) = s.call("acme", Method::PUT, &path, json!({ "api_key": "wrong" })).await;
    assert_eq!((code, body["error"].as_str().unwrap_or_default().contains("didn't accept")), (400, true), "{body}");
    assert!(s.fake.webhooks.lock().unwrap().is_empty());
    assert_eq!(s.call("acme", Method::PUT, &path, json!({ "api_key": " " })).await.0, 400);

    let (code, row) = s.connect().await;
    assert_eq!((code, row["calls"].as_i64(), row["error"].as_str()), (200, Some(0), Some("")), "{row}");
    let token = s.path_token().await;
    let first = s.fake.webhooks.lock().unwrap()[0].clone();
    assert_eq!(first["hook_url"], format!("https://splee.example/api/dialpad-webhooks/{token}"));
    let secret = s.secret(1);
    assert!(secret.len() >= 32, "{secret}");
    assert_eq!(s.fake.subscriptions.lock().unwrap()[0], json!({ "endpoint_id": 101, "call_states": STATES, "enabled": true }));
    let stored: (String, String, String, String) =
        sqlx::query_as("select webhook_id, subscription_id, secret_enc, api_key_enc from dialpad_connections").fetch_one(&s.db).await.unwrap();
    assert_eq!((stored.0.as_str(), stored.1.as_str()), ("101", "201"));
    assert!(!stored.2.contains(&secret) && !stored.3.contains(fake::API_KEY), "neither the secret nor the API key is stored as it is");
    assert_eq!(s.call("beta", Method::GET, &path, json!(null)).await.0, 404, "another organization's project");

    // Only an event signed with the webhook's secret is read.
    assert_eq!(s.post(&token, json!({ "state": "hangup" }).to_string()).await.0, 401);
    assert_eq!(s.post(&token, fake::sign("not-the-secret", &json!({ "state": "hangup" }))).await.0, 401);
    let none = crate::crypto::hs256_jwt(secret.as_bytes(), &json!({ "alg": "none" }), &json!({ "state": "hangup" }));
    assert_eq!(s.post(&token, none).await.0, 401, "only HS256");
    assert_eq!(s.event("nope", 1, &json!({})).await.0, 404);

    // Connecting again keeps the URL, registers a new webhook with a new secret and deletes the old one.
    assert_eq!(s.connect().await.0, 200);
    assert_eq!(s.path_token().await, token);
    assert_eq!(s.fake.webhooks.lock().unwrap()[1]["hook_url"], first["hook_url"]);
    assert_ne!(s.secret(2), secret);
    assert_eq!(*s.fake.deleted.lock().unwrap(), ["subscriptions/call/201", "webhooks/101"]);
    assert_eq!(s.event(&token, 1, &json!({ "state": "hangup" })).await.0, 401, "the old secret is no good any more");
    assert_eq!(s.event(&token, 2, &json!({ "state": "ringing" })).await, (200, json!({ "matched": false })));

    assert_eq!(s.call("acme", Method::DELETE, &path, json!(null)).await, (200, Value::Null));
    assert_eq!(*s.fake.deleted.lock().unwrap(), ["subscriptions/call/201", "webhooks/101", "subscriptions/call/202", "webhooks/102"]);
    assert_eq!(s.event(&token, 2, &json!({})).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn calls_with_leads_are_kept_once_with_their_recordings_and_transcripts(db: PgPool) {
    let s = Seeded::start(db).await;
    assert_eq!(s.connect().await.0, 200);
    let token = s.path_token().await;

    // Jo calls Sarah, which closes her open call step with what happened.
    let to_sarah = dialpad_call(6001, "+44 7700 900123", "outbound", true, 600, 120);
    assert_eq!(s.event(&token, 1, &to_sarah).await, (200, json!({ "matched": true })));
    assert_eq!(s.event(&token, 1, &to_sarah).await, (200, json!({ "matched": true })), "a repeated event");
    let sarahs = s.calls_of(s.sarah).await;
    assert_eq!(sarahs.len(), 1, "{sarahs:?}");
    let c = &sarahs[0];
    assert_eq!(
        (c["kind"].as_str(), c["status"].as_str(), c["outcome"].as_str(), c["dialpad_id"].as_i64(), c["duration_secs"].as_i64(), c["call_status"].as_str()),
        (Some("sequence"), Some("done"), Some("connected"), Some(6001), Some(120), Some("completed"))
    );
    assert_eq!(c["called_by"].as_str(), Some(s.jo.to_string().as_str()));
    // A leg of the same call after a transfer is the same call.
    let mut leg = dialpad_call(6009, "+447700900123", "outbound", true, 300, 60);
    leg["master_call_id"] = json!("6001");
    assert_eq!(s.event(&token, 1, &leg).await.1, json!({ "matched": true }));
    assert_eq!(s.calls_of(s.sarah).await.len(), 1);

    // The recording comes in its own event, and the player gets it from Dialpad with the API key.
    let mut recorded = to_sarah.clone();
    recorded["state"] = json!("recording");
    let url = format!("{}/dialpad/r/7001", s.fake_url);
    recorded["recording_details"] = json!([{ "id": 7001, "url": url, "duration": 120_000, "recording_type": "callrecording" }]);
    assert_eq!(s.event(&token, 1, &recorded).await.1, json!({ "matched": true }));
    let id = c["id"].as_str().unwrap();
    let res = s.http.get(format!("{}/calls/{id}/recording", s.base)).header("cookie", "splee_session=acme").send().await.unwrap();
    assert_eq!((res.status().as_u16(), res.text().await.unwrap()), (200, "mp3 of 7001".into()));
    let res = s.http.get(format!("{}/calls/{id}/recording", s.base)).header("cookie", "splee_session=beta").send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404, "another organization's call");
    // The API key only goes to Dialpad.
    sqlx::query("update calls set recording_url = 'https://elsewhere.example/r/7001' where id = $1::uuid").bind(id).execute(&s.db).await.unwrap();
    let res = s.http.get(format!("{}/calls/{id}/recording", s.base)).header("cookie", "splee_session=acme").send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404);

    // The transcript's said lines go on the call in order, and the agent is asked for a summary, once.
    let lines = json!([{ "type": "transcript", "name": "(44) 7700", "contact_id": "c1", "content": "Hi!", "time": "2026-11-22T10:00:03.000000" },
                       { "type": "transcript", "name": "Jo", "user_id": "5111", "content": "Hi Sarah.", "time": "2026-11-22T10:00:01.000000" },
                       { "type": "moment", "name": "Jo", "user_id": "5111", "content": "price_inquiry", "time": "2026-11-22T10:00:02.000000" }]);
    s.fake.transcripts.lock().unwrap().insert(6001, lines);
    let mut transcribed = to_sarah.clone();
    transcribed["state"] = json!("call_transcription");
    for _ in 0..2 {
        assert_eq!(s.event(&token, 1, &transcribed).await.1, json!({ "matched": true }));
    }
    assert_eq!(s.calls_of(s.sarah).await[0]["transcript"], "Us: Hi Sarah.\nLead: Hi!");
    let runs: Vec<Value> = sqlx::query_scalar("select input from agent_runs where kind = 'summarize_call'").fetch_all(&s.db).await.unwrap();
    assert_eq!(runs, [json!({ "call_id": c["id"] })]);

    // Numbers that aren't a lead's, hidden ones and states Splee doesn't read aren't kept.
    assert_eq!(s.event(&token, 1, &dialpad_call(6002, "+1 415 555 0100", "outbound", true, 60, 30)).await.1, json!({ "matched": false }));
    assert_eq!(s.event(&token, 1, &dialpad_call(6003, "", "inbound", true, 60, 30)).await.1, json!({ "matched": false }));
    let mut ringing = dialpad_call(6004, "+33612345678", "inbound", false, 5, 0);
    ringing["state"] = json!("ringing");
    assert_eq!(s.event(&token, 1, &ringing).await.1, json!({ "matched": false }));

    // Tom calls and nobody answers.
    assert_eq!(s.event(&token, 1, &dialpad_call(6005, "+33 6 12 34 56 78", "inbound", false, 300, 20)).await.1, json!({ "matched": true }));
    let toms = s.calls_of(s.tom).await;
    assert_eq!(
        (toms.len(), toms[0]["kind"].as_str(), toms[0]["outcome"].as_str(), toms[0]["notes"].as_str(), toms[0]["dialpad_id"].as_i64()),
        (1, Some("manual"), Some("no_answer"), Some("They called and nobody answered."), Some(6005))
    );

    let (_, row) = s.call("acme", Method::GET, &format!("/projects/{}/dialpad", s.project), json!(null)).await;
    assert_eq!((row["calls"].as_i64(), row["error"].as_str(), row["last_event_at"].is_string()), (Some(2), Some(""), true), "{row}");
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
async fn a_call_started_in_dialpad_from_the_calls_page_closes_that_step(db: PgPool) {
    let s = Seeded::start(db).await;
    // Dan and Dana share a US number, and the team spoke to Dana last, so a call nobody started from Splee goes to her. A US
    // number keeps several time zones, so the project's decides the calling hours.
    let lead = |name: &'static str, contacted: bool| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into leads (project_id, full_name, phone, status, last_contacted_at)
             values ($1, $2, '+14155550134', 'contacted', case when $3 then now() end) returning id",
        )
        .bind(s.project)
        .bind(name)
        .bind(contacted)
        .fetch_one(&s.db)
    };
    let (dan, dana) = (lead("Dan Ross", false).await.unwrap(), lead("Dana Ross", true).await.unwrap());
    let step = |n: i32, due: i32| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into calls (project_id, lead_id, kind, step, status, to_number, due_at)
             values ($1, $2, 'sequence', $3, 'open', '+14155550134', now() + make_interval(days => $4)) returning id",
        )
        .bind(s.project)
        .bind(dan)
        .bind(n)
        .bind(due)
        .fetch_one(&s.db)
    };
    let (earlier, call) = (step(1, -1).await.unwrap(), step(2, 0).await.unwrap());
    let zone = |hour: i64| {
        let upsert = "insert into project_settings (project_id, timezone) values ($1, $2) on conflict (project_id) do update set timezone = excluded.timezone";
        sqlx::query(upsert).bind(s.project).bind(zone_at(hour)).execute(&s.db)
    };
    zone(12).await.unwrap();
    let path = format!("/calls/{call}/dialpad-dial");
    let dial = || s.call("acme", Method::POST, &path, json!({}));

    let (code, body) = dial().await;
    assert_eq!((code, body["error"].as_str()), (409, Some("Connect Dialpad on the Integrations page to call from it.")));
    assert_eq!(s.connect().await.0, 200);
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("Nobody in your Dialpad has the email acme@acme.example"), "{body}");

    let user = 5_066_549_580_791_808_i64;
    s.fake.users.lock().unwrap().insert("acme@acme.example".into(), json!({ "id": user.to_string(), "emails": ["acme@acme.example"] }));
    assert_eq!(dial().await, (200, json!({ "to": "+14155550134" })));
    assert_eq!(*s.fake.dialed.lock().unwrap(), [(user, json!({ "phone_number": "+14155550134", "custom_data": call.to_string() }))]);
    s.fake.no_app.lock().unwrap().insert(user);
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("Open Dialpad on your computer or in your browser"), "{body}");
    s.fake.no_app.lock().unwrap().clear();

    // The same checks as a call from the browser.
    assert_eq!(s.call("beta", Method::POST, &path, json!({})).await.0, 404);
    zone(22).await.unwrap();
    let (code, body) = dial().await;
    assert!(code == 409 && body["error"].as_str().unwrap().contains("Call between 8:00 and 20:00 their time"), "{body}");
    zone(12).await.unwrap();

    // The call comes back with the step in its custom_data and closes that one, not Dan's earlier step or Dana's.
    let token = s.path_token().await;
    let mut ended = dialpad_call(8001, "+1 415-555-0134", "outbound", true, 120, 60);
    ended["custom_data"] = json!(call.to_string());
    ended["target"]["email"] = json!("acme@acme.example");
    assert_eq!(s.event(&token, 1, &ended).await.1, json!({ "matched": true }));
    let status = |id: Uuid| sqlx::query_as::<_, (String, Option<i64>)>("select status, dialpad_id from calls where id = $1").bind(id).fetch_one(&s.db);
    assert_eq!(status(call).await.unwrap(), ("done".into(), Some(8001)));
    assert_eq!(status(earlier).await.unwrap(), ("open".into(), None));
    assert!(s.calls_of(dana).await.is_empty());
    assert_eq!(dial().await.1["error"], "This call was already logged, skipped or cancelled.");

    // custom_data another app put there, or a step that's closed, is no step: the call goes to Dana, on its own.
    for (id, data) in [(8002, json!("ticket=12")), (8003, json!(call.to_string()))] {
        let mut other = dialpad_call(id, "+14155550134", "outbound", false, 60, 0);
        other["custom_data"] = data;
        assert_eq!(s.event(&token, 1, &other).await.1, json!({ "matched": true }));
    }
    let danas: Vec<Option<i64>> = s.calls_of(dana).await.iter().map(|c| c["dialpad_id"].as_i64()).collect();
    assert_eq!(danas, [Some(8002), Some(8003)]);
    assert_eq!(status(earlier).await.unwrap(), ("open".into(), None), "Dana was spoken to last, so it's her call");
    assert_eq!(s.fake.dialed.lock().unwrap().len(), 1, "nothing else reached Dialpad");
}
