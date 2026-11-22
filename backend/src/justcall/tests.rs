use std::sync::Arc;

use axum::Router;
use chrono::{Duration, Utc};
use reqwest::Method;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::fake::{self, Fake};
use super::{EVENTS, parse, signed, transcript};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

const AT: &str = "2026-11-22 10:00:00";

#[test]
fn signatures_match_the_docs_example() {
    let event = json!({ "webhook_url": "https://webhook.site/3bcea770-370a-4b09-8b66-426f687e08a4", "type": "call.completed", "data": {} });
    let (secret, signature) = ("ea39089c40790e9dc7a080ec95e849b8fa0fa5fb", "56761bae5b27a784a3ddd2af828bc5def7176bc0a8650199b04c737bd39bbecf");
    assert!(signed(secret, &event, "2024-03-21 17:08:22", signature));
    assert_eq!(fake::sign(secret, &event, "2024-03-21 17:08:22"), signature);
    assert!(!signed(secret, &event, "2024-03-21 17:08:23", signature), "another time");
    assert!(!signed("another-secret", &event, "2024-03-21 17:08:22", signature));
    let mut other = event.clone();
    other["type"] = json!("call.missed");
    assert!(!signed(secret, &other, "2024-03-21 17:08:22", signature), "another event");
    assert!(!signed(secret, &event, "2024-03-21 17:08:22", "not hex"));
}

#[test]
fn calls_are_read_from_the_docs_payload() {
    let mut call = justcall_call(178_632, "16813810100", "Incoming", "answered", 600);
    call["call_date"] = json!("2024-01-18");
    call["call_time"] = json!("14:34:13");
    let e = parse(&call).unwrap();
    let started = chrono::NaiveDate::from_ymd_opt(2024, 1, 18).unwrap().and_hms_opt(14, 34, 13).unwrap().and_utc();
    assert_eq!((e.id, e.number.as_str(), e.inbound, e.started_at), (178_632, "+16813810100", true, started));
    assert_eq!((e.answered_at, e.ended_at), (Some(started + Duration::seconds(30)), Some(started + Duration::seconds(630))));
    assert_eq!((e.user_email.as_str(), e.asset.is_empty()), ("jo@acme.example", false));
    let missed = parse(&justcall_call(2, "+33612345678", "Incoming", "Missed", 0)).unwrap();
    assert_eq!((missed.answered_at, missed.number.as_str()), (None, "+33612345678"));
    assert_eq!(parse(&justcall_call(3, "", "Outgoing", "Unanswered", 0)), None, "a hidden number");

    // Lines in the order they were said, named when JustCall knows who said them.
    call["justcall_ai"]["call_transcription"] = json!([
        { "speaker_id": 1, "sentence": "Hi.", "timestamp": { "starttime": 7, "endtime": 8 } },
        { "speaker_id": 0, "speaker_name": "Jo", "sentence": "Hello! Is this Robert?", "timestamp": { "starttime": 5, "endtime": 6 } },
        { "speaker_id": 1, "sentence": " ", "timestamp": { "starttime": 9, "endtime": 9 } },
    ]);
    assert_eq!(transcript(&call), "Jo: Hello! Is this Robert?\nSpeaker 1: Hi.");
}

/// A JustCall call between Jo and `number`, `secs` long once answered, as its events carry it.
fn justcall_call(id: i64, number: &str, direction: &str, kind: &str, secs: i64) -> Value {
    let start = Utc::now() - Duration::minutes(10);
    json!({
        "id": id, "call_sid": format!("CA{id}"), "contact_number": number, "contact_name": "", "contact_email": "",
        "justcall_number": "12135660100", "agent_id": 2787, "agent_name": "Jo", "agent_email": "Jo@Acme.example",
        "call_date": start.format("%Y-%m-%d").to_string(), "call_time": start.format("%H:%M:%S").to_string(),
        "call_info": { "direction": direction, "type": kind, "recording": if secs > 0 { json!(format!("https://callingservice.justcall.io/r?recsid=RE{id}")) } else { json!("") },
                       "voicemail_transcription": "", "notes": "", "disposition": "" },
        "call_duration": { "friendly_duration": "00:10:33", "ring_time": 30, "hold_time": 0, "total_duration": secs, "conversation_time": secs },
        "justcall_ai": { "call_transcription": [], "call_summary": "" },
    })
}

struct Seeded {
    base: String,
    http: reqwest::Client,
    fake: Fake,
    db: PgPool,
    project: Uuid,
    /// Sarah, +44 7700 900123, with an open call step. Tom, +33 6 12 34 56 78, without one.
    sarah: Uuid,
    tom: Uuid,
    jo: Uuid,
}

impl Seeded {
    /// Acme and Beta, each with an owner signed in as session "acme" or "beta", and Acme's project. Jo is on Acme's team and
    /// calls from JustCall as jo@acme.example.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let base_state = crate::autopilot::tests::state(db.clone());
        let public_url = "https://splee.example".into();
        let cfg = Config { app_secret: "test-secret".into(), justcall_api_url: fake_url, public_url, ..(*base_state.cfg).clone() };
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
        Self { base, http: reqwest::Client::new(), fake, db, project, sarah, tom, jo }
    }

    async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn connect(&self, key: &str, secret: &str) -> (u16, Value) {
        self.call("acme", Method::PUT, &format!("/projects/{}/justcall", self.project), json!({ "api_key": format!(" {key} "), "api_secret": secret })).await
    }

    async fn path_token(&self) -> String {
        sqlx::query_scalar("select path_token from justcall_connections").fetch_one(&self.db).await.unwrap()
    }

    /// Posts `event` of type `kind` to the webhook with `token`, as JustCall would to the URL it has, signed with `secret`.
    async fn event_with(&self, token: &str, secret: &str, kind: &str, data: &Value) -> (u16, Value) {
        let url = format!("https://splee.example/api/justcall-webhooks/{token}");
        let event = json!({ "request_id": "01HSGZRD0PHRCCHM46JRCRYMC4", "webhook_url": url, "url_id": "u1", "type": kind, "data": data });
        let req = self.http.post(format!("{}/justcall-webhooks/{token}", self.base)).header("content-type", "application/json");
        let req = req.header("x-justcall-signature", fake::sign(secret, &event, AT)).header("x-justcall-request-timestamp", AT).header("x-justcall-signature-version", "v1");
        let res = req.body(event.to_string()).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn event(&self, token: &str, kind: &str, data: &Value) -> (u16, Value) {
        self.event_with(token, fake::API_SECRET, kind, data).await
    }

    async fn calls_of(&self, lead: Uuid) -> Vec<Value> {
        sqlx::query_scalar("select to_jsonb(c) from calls c where lead_id = $1 order by created_at").bind(lead).fetch_all(&self.db).await.unwrap()
    }
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn connecting_adds_the_webhook_url_to_each_call_event_and_disconnecting_removes_it(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/justcall", s.project);
    assert_eq!(s.call("acme", Method::GET, &path, json!(null)).await, (200, Value::Null));
    let (code, body) = s.connect(fake::API_KEY, "wrong").await;
    assert_eq!((code, body["error"].as_str().unwrap_or_default().contains("didn't accept")), (400, true), "{body}");
    assert_eq!(s.connect(fake::API_KEY, " ").await.0, 400);
    assert!(s.fake.added.lock().unwrap().is_empty());

    // An event JustCall won't add the URL to takes it off the ones it was added to, and nothing is saved.
    *s.fake.refuse.lock().unwrap() = Some("jc.call_ai_generated".into());
    let (code, body) = s.connect(fake::API_KEY, fake::API_SECRET).await;
    assert!(code == 400 && body["error"].as_str().unwrap().contains("Team plan"), "{body}");
    assert_eq!(*s.fake.removed.lock().unwrap(), ["u1", "u2", "u3"]);
    assert_eq!(s.call("acme", Method::GET, &path, json!(null)).await, (200, Value::Null));
    *s.fake.refuse.lock().unwrap() = None;
    s.fake.added.lock().unwrap().clear();
    s.fake.removed.lock().unwrap().clear();
    s.fake.removed_with.lock().unwrap().clear();

    let (code, row) = s.connect(fake::API_KEY, fake::API_SECRET).await;
    assert_eq!((code, row["calls"].as_i64(), row["error"].as_str()), (200, Some(0), Some("")), "{row}");
    let token = s.path_token().await;
    let url = format!("https://splee.example/api/justcall-webhooks/{token}");
    let added: Vec<Value> = s.fake.added.lock().unwrap().clone();
    assert_eq!(added, EVENTS.map(|e| json!({ "type": e, "webhook_url": url })));
    let stored: (Vec<String>, String, String) =
        sqlx::query_as("select url_ids, api_key_enc, api_secret_enc from justcall_connections").fetch_one(&s.db).await.unwrap();
    assert_eq!(stored.0, ["u1", "u2", "u3", "u4"], "only Splee's URL, not the others on the event");
    assert!(!stored.1.contains(fake::API_KEY) && !stored.2.contains(fake::API_SECRET), "neither the key nor the secret is stored as it is");
    assert_eq!(s.call("beta", Method::GET, &path, json!(null)).await.0, 404, "another organization's project");

    // Only an event signed with the account's secret is read. The post JustCall checks a URL with has no call, and is answered
    // even before the URL's token is saved.
    let ringing = json!({ "id": 1 });
    assert_eq!(s.event_with(&token, "not-the-secret", "call.completed", &ringing).await.0, 401);
    let res = s.http.post(format!("{}/justcall-webhooks/{token}", s.base)).body(json!({ "type": "call.completed", "data": {} }).to_string()).send().await.unwrap();
    assert_eq!(res.status().as_u16(), 401, "unsigned");
    let check = json!({ "webhook_url": "https://splee.example/api/justcall-webhooks/new", "type": "call.completed" }).to_string();
    let res = s.http.post(format!("{}/justcall-webhooks/new", s.base)).body(check).send().await.unwrap();
    assert_eq!((res.status().as_u16(), res.json::<Value>().await.unwrap()), (200, json!({ "matched": false })));
    assert_eq!(s.event("nope", "call.completed", &ringing).await.0, 404);
    assert_eq!(s.event(&token, "sms.received", &ringing).await, (200, json!({ "matched": false })));

    // Connecting again, here to another account, adds a new URL and removes the old one with the old key, the only one that
    // reaches it.
    assert_eq!(s.connect(fake::OTHER_KEY, fake::OTHER_SECRET).await.0, 200);
    let new_token = s.path_token().await;
    assert_ne!(new_token, token);
    assert_eq!(s.fake.added.lock().unwrap()[4]["webhook_url"], format!("https://splee.example/api/justcall-webhooks/{new_token}"));
    assert_eq!(*s.fake.removed.lock().unwrap(), ["u1", "u2", "u3", "u4"]);
    let old_pair = format!("{}:{}", fake::API_KEY, fake::API_SECRET);
    assert_eq!(*s.fake.removed_with.lock().unwrap(), [old_pair.clone(), old_pair.clone(), old_pair.clone(), old_pair]);
    assert_eq!(s.event(&token, "call.completed", &ringing).await.0, 404, "the old URL is gone");
    assert_eq!(s.event_with(&new_token, fake::API_SECRET, "call.completed", &ringing).await.0, 401, "the old secret is no good any more");
    assert_eq!(s.event_with(&new_token, fake::OTHER_SECRET, "call.completed", &ringing).await, (200, json!({ "matched": false })));

    assert_eq!(s.call("acme", Method::DELETE, &path, json!(null)).await, (200, Value::Null));
    assert_eq!(s.fake.removed.lock().unwrap()[4..], ["u5", "u6", "u7", "u8"]);
    assert_eq!(s.fake.removed_with.lock().unwrap()[4], format!("{}:{}", fake::OTHER_KEY, fake::OTHER_SECRET));
    assert_eq!(s.event_with(&new_token, fake::OTHER_SECRET, "call.completed", &ringing).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn calls_with_leads_are_kept_once_with_their_recordings_and_transcripts(db: PgPool) {
    let s = Seeded::start(db).await;
    assert_eq!(s.connect(fake::API_KEY, fake::API_SECRET).await.0, 200);
    let token = s.path_token().await;

    // Jo calls Sarah, which closes her open call step with what happened.
    let to_sarah = justcall_call(6001, "447700900123", "Outgoing", "Answered", 120);
    assert_eq!(s.event(&token, "call.completed", &to_sarah).await, (200, json!({ "matched": true })));
    assert_eq!(s.event(&token, "call.completed", &to_sarah).await, (200, json!({ "matched": true })), "a repeated event");
    let sarahs = s.calls_of(s.sarah).await;
    assert_eq!(sarahs.len(), 1, "{sarahs:?}");
    let c = &sarahs[0];
    assert_eq!(
        (c["kind"].as_str(), c["status"].as_str(), c["outcome"].as_str(), c["justcall_id"].as_i64(), c["duration_secs"].as_i64(), c["call_status"].as_str()),
        (Some("sequence"), Some("done"), Some("connected"), Some(6001), Some(120), Some("completed"))
    );
    assert_eq!((c["called_by"].as_str(), c["inbound"].as_bool()), (Some(s.jo.to_string().as_str()), Some(false)));

    // The player gets the recording from JustCall's API with the key.
    let id = c["id"].as_str().unwrap();
    let res = s.http.get(format!("{}/calls/{id}/recording", s.base)).header("cookie", "splee_session=acme").send().await.unwrap();
    assert_eq!(res.headers()["content-type"], "audio/mpeg");
    assert_eq!((res.status().as_u16(), res.text().await.unwrap()), (200, "mp3 of 6001".into()));
    let res = s.http.get(format!("{}/calls/{id}/recording", s.base)).header("cookie", "splee_session=beta").send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404, "another organization's call");

    // The AI report's transcript goes on the call, and the agent is asked for a summary, once.
    let mut report = to_sarah.clone();
    report["justcall_ai"]["call_transcription"] = json!([
        { "speaker_id": 1, "sentence": "Hi!", "timestamp": { "starttime": 3, "endtime": 4 } },
        { "speaker_id": 0, "sentence": "Hi Sarah, it's Jo.", "timestamp": { "starttime": 1, "endtime": 2 } },
    ]);
    for _ in 0..2 {
        assert_eq!(s.event(&token, "jc.call_ai_generated", &report).await.1, json!({ "matched": true }));
    }
    assert_eq!(s.calls_of(s.sarah).await[0]["transcript"], "Speaker 0: Hi Sarah, it's Jo.\nSpeaker 1: Hi!");
    let runs: Vec<Value> = sqlx::query_scalar("select input from agent_runs where kind = 'summarize_call'").fetch_all(&s.db).await.unwrap();
    assert_eq!(runs, [json!({ "call_id": c["id"] })]);

    // Numbers that aren't a lead's, and hidden ones, aren't kept.
    assert_eq!(s.event(&token, "call.completed", &justcall_call(6002, "14155550100", "Outgoing", "Answered", 30)).await.1, json!({ "matched": false }));
    assert_eq!(s.event(&token, "call.completed", &justcall_call(6003, "", "Incoming", "Answered", 30)).await.1, json!({ "matched": false }));

    // Tom calls and nobody answers. Then he calls again and leaves a voicemail, which plays like a recording.
    let mut missed = justcall_call(6005, "33612345678", "Incoming", "Missed", 0);
    missed["call_info"]["recording"] = json!("https://callingservice.justcall.io/r?recsid=RE6005");
    assert_eq!(s.event(&token, "call.missed", &missed).await.1, json!({ "matched": true }));
    let mut voicemail = justcall_call(6006, "+33612345678", "Incoming", "Voicemail", 0);
    voicemail["call_info"]["recording"] = json!("https://callingservice.justcall.io/r?recsid=RE6006");
    assert_eq!(s.event(&token, "call.voicemail", &voicemail).await.1, json!({ "matched": true }));
    let toms = s.calls_of(s.tom).await;
    let notes: Vec<(&str, &str, i64)> =
        toms.iter().map(|c| (c["outcome"].as_str().unwrap(), c["notes"].as_str().unwrap(), c["justcall_id"].as_i64().unwrap())).collect();
    assert_eq!(notes, [("no_answer", "They called and nobody answered.", 6005), ("no_answer", "They called and left a voicemail.", 6006)]);
    assert!(toms.iter().all(|c| c["kind"] == "manual" && c["inbound"] == true));
    let res = s.http.get(format!("{}/calls/{}/recording", s.base, toms[0]["id"].as_str().unwrap())).header("cookie", "splee_session=acme").send().await.unwrap();
    assert_eq!(res.status().as_u16(), 404, "a missed call has no recording");

    let (_, row) = s.call("acme", Method::GET, &format!("/projects/{}/justcall", s.project), json!(null)).await;
    assert_eq!((row["calls"].as_i64(), row["error"].as_str(), row["last_event_at"].is_string()), (Some(3), Some(""), true), "{row}");
}
