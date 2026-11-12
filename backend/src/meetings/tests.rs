use std::sync::Arc;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::Method;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use super::fake::{self, Fake};
use super::{Attendee, Provider, TRANSCRIPT_CHARS, cap, fathom, fireflies, gong, match_lead, zoom};
use crate::agent::meetings::tests::{answer, run_notes};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

const FIREFLIES_SECRET: &str = "ff-webhook-secret";
/// Fathom's secrets are "whsec_" and a base64 key.
const FATHOM_RAW: &[u8] = b"fathom-signing-key-0123456789";
const ZOOM_SECRET_TOKEN: &str = "zoom-secret-token";
const KEY: &str = "splee_test_meetings";

fn hmac_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).unwrap();
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

fn fathom_secret() -> String {
    format!("whsec_{}", STANDARD.encode(FATHOM_RAW))
}

fn fathom_signature(id: &str, ts: i64, body: &[u8]) -> String {
    format!("v1,{}", STANDARD.encode(hmac_bytes(FATHOM_RAW, &[id.as_bytes(), b".", ts.to_string().as_bytes(), b".", body].concat())))
}

fn zoom_signature(secret: &str, ts: i64, body: &[u8]) -> String {
    format!("v0={}", hex::encode(hmac_bytes(secret.as_bytes(), &[b"v0:", ts.to_string().as_bytes(), b":", body].concat())))
}

#[test]
fn signatures_are_checked_for_each_provider() {
    let now = 1_790_000_000;
    let body = br#"{"meetingId":"m1","eventType":"Transcription completed"}"#;

    let good = hex::encode(hmac_bytes(FIREFLIES_SECRET.as_bytes(), body));
    assert!(fireflies::verify(FIREFLIES_SECRET, &good, body));
    assert!(fireflies::verify(FIREFLIES_SECRET, &format!("sha256={good}"), body));
    assert!(!fireflies::verify("other-secret", &good, body), "the wrong secret");
    assert!(!fireflies::verify(FIREFLIES_SECRET, &good, b"{}"), "a changed body");
    assert!(!fireflies::verify(FIREFLIES_SECRET, "not hex", body));

    let good = fathom_signature("msg_1", now, body);
    assert!(fathom::verify(&fathom_secret(), "msg_1", &now.to_string(), &good, body, now + 299));
    assert!(!fathom::verify(&format!("whsec_{}", STANDARD.encode(b"other")), "msg_1", &now.to_string(), &good, body, now), "the wrong secret");
    assert!(!fathom::verify(&fathom_secret(), "msg_2", &now.to_string(), &good, body, now), "another message's id");
    assert!(!fathom::verify(&fathom_secret(), "msg_1", &now.to_string(), &good, body, now + 301), "stale");
    assert!(!fathom::verify(&fathom_secret(), "", &now.to_string(), &good, body, now), "no id");
    // While a secret is rolled, the header has a signature for each.
    let rolled = format!("v1,{} {good}", STANDARD.encode([7u8; 32]));
    assert!(fathom::verify(&fathom_secret(), "msg_1", &now.to_string(), &rolled, body, now));

    let good = zoom_signature(ZOOM_SECRET_TOKEN, now, body);
    assert!(zoom::verify(ZOOM_SECRET_TOKEN, &now.to_string(), &good, body, now + 60));
    assert!(!zoom::verify("other", &now.to_string(), &good, body, now), "the wrong secret");
    assert!(!zoom::verify(ZOOM_SECRET_TOKEN, &now.to_string(), &good, b"{}", now), "a changed body");
    assert!(!zoom::verify(ZOOM_SECRET_TOKEN, &now.to_string(), &good, body, now - 301), "from the future");
    assert!(!zoom::verify(ZOOM_SECRET_TOKEN, &now.to_string(), &good.replace("v0=", ""), body, now), "no version");
    let answer = zoom::validation(ZOOM_SECRET_TOKEN, "plain-123");
    assert_eq!(answer["plainToken"], "plain-123");
    assert_eq!(answer["encryptedToken"].as_str().unwrap(), hex::encode(hmac_bytes(ZOOM_SECRET_TOKEN.as_bytes(), b"plain-123")));

    // Anyone could sign with an empty secret, and Gong has no webhook.
    let mut headers = HeaderMap::new();
    headers.insert("x-hub-signature", HeaderValue::from_str(&hex::encode(hmac_bytes(b"", body))).unwrap());
    assert!(!Provider::Fireflies.verify("", &headers, body, now));
    assert!(!Provider::Gong.verify("anything", &headers, body, now));
}

#[test]
fn transcripts_are_read_and_capped() {
    let vtt = "WEBVTT\n\n1\n00:00:01.000 --> 00:00:04.000\nSam Lee: Thanks for joining.\n\n2\n00:00:05.000 --> 00:00:07.000\nTom: Happy to.\n";
    assert_eq!(zoom::vtt_lines(vtt), "Sam Lee: Thanks for joining.\nTom: Happy to.");
    assert_eq!(zoom::meeting_path("aBc+d=="), "aBc%2Bd%3D%3D");
    assert_eq!(zoom::meeting_path("/ab//c"), "%252Fab%252F%252Fc", "encoded twice");

    // Characters, not bytes, so a transcript in another script isn't cut mid-letter.
    let long = "é".repeat(TRANSCRIPT_CHARS + 5);
    let capped = cap(&long);
    assert_eq!(capped.chars().count(), TRANSCRIPT_CHARS);
    assert_eq!(cap("short"), "short");

    let t = json!({
        "id": "ff1", "title": "Northwind intro", "date": 1_790_000_000_000i64, "duration": 31.5, "transcript_url": "https://app.fireflies.ai/view/ff1",
        "organizer_email": "Jo@Acme.example", "participants": ["jo@acme.example", "sarah@northwind.example"],
        "meeting_attendees": [{ "displayName": "Sarah Kim", "email": "sarah@northwind.example" }],
        "sentences": [{ "speaker_name": "Jo", "text": "Hi Sarah" }, { "speaker_name": "Sarah Kim", "text": "Hi!" }],
        "summary": { "overview": "Intro call.", "action_items": "Send pricing" },
    });
    let f = fireflies::parse(&t).unwrap();
    assert_eq!((f.external_id.as_str(), f.duration_secs, f.started_at.timestamp()), ("ff1", Some(1890), 1_790_000_000));
    assert_eq!(f.transcript, "Jo: Hi Sarah\nSarah Kim: Hi!");
    assert_eq!(f.summary, "Intro call.\n\nAction items:\nSend pricing");
    assert_eq!(
        f.attendees,
        vec![
            Attendee { name: "Sarah Kim".into(), email: "sarah@northwind.example".into(), internal: false },
            Attendee { name: String::new(), email: "jo@acme.example".into(), internal: true },
        ]
    );
}

struct Seeded {
    state: AppState,
    base: String,
    http: reqwest::Client,
    fake: Fake,
    acme: Uuid,
    project: Uuid,
    /// Sarah at northwind.example, with an open deal from a hot reply and a first email from Acme's mailbox. Tom at
    /// contoso.example, without a deal. Gina at gmail.com. Beta's project has a lead with Sarah's address too.
    sarah: Uuid,
    tom: Uuid,
    beta_sarah: Uuid,
    mailbox: Uuid,
}

impl Seeded {
    /// Acme and Beta, each with an owner signed in as session "acme" or "beta" and a project. Jo is on Acme's team and
    /// runs its meetings from jo@acme.example.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let base_state = crate::autopilot::tests::state(db.clone());
        let cfg = Config { app_secret: "test-secret".into(), meetings_api_url: fake_url, ..(*base_state.cfg).clone() };
        let state = AppState { cfg: Arc::new(cfg), ..base_state };
        let mut ids = vec![];
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
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, $2, '{\"name\": \"Fieldnote\"}') returning id")
                .bind(org)
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            ids.push((org, project));
        }
        let ((acme, project), (beta, beta_project)) = (ids[0], ids[1]);
        let jo: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('jo@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(acme).bind(jo).execute(&db).await.unwrap();
        let mailbox: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'jo@fieldnote.example', 'smtp.test', 'jo', 'x')
             returning id",
        )
        .bind(acme)
        .fetch_one(&db)
        .await
        .unwrap();
        let lead = |project: Uuid, name: &'static str, email: &'static str, domain: &'static str| {
            sqlx::query_as::<_, crate::models::Lead>(
                "insert into leads (project_id, full_name, first_name, email, company_name, company_domain, status)
                 values ($1, $2, split_part($2, ' ', 1), $3, $2, $4, 'hot') returning *",
            )
            .bind(project)
            .bind(name)
            .bind(email)
            .bind(domain)
        };
        let sarah = lead(project, "Sarah Kim", "sarah@northwind.example", "northwind.example").fetch_one(&db).await.unwrap();
        let tom = lead(project, "Tom Ruiz", "tom@contoso.example", "www.contoso.example").fetch_one(&db).await.unwrap();
        lead(project, "Gina Moss", "gina@gmail.com", "gmail.com").fetch_one(&db).await.unwrap();
        let beta_sarah = lead(beta_project, "Sarah Kim", "sarah@northwind.example", "northwind.example").fetch_one(&db).await.unwrap();
        crate::deals::hot_reply(&state, &sarah).await;
        sqlx::query(
            "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, subject, body, sent_at, message_id)
             values ($1, $2, $3, 'outbound', 'initial', 'sent', 'Quick idea', 'Hello', now() - interval '5 days', '<first@fieldnote.example>')",
        )
        .bind(project)
        .bind(sarah.id)
        .bind(mailbox)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'crm', 'splee_test_', $2)").bind(acme).bind(sha256_hex(KEY)).execute(&db).await.unwrap();
        let _ = beta;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { state, base, http: reqwest::Client::new(), fake, acme, project, sarah: sarah.id, tom: tom.id, beta_sarah: beta_sarah.id, mailbox }
    }

    fn db(&self) -> &PgPool {
        &self.state.db
    }

    async fn call(&self, session: &str, method: Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn api(&self, path: &str) -> (u16, Value) {
        let res = self.http.get(format!("{}{path}", self.base)).bearer_auth(KEY).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    /// Connects the provider to Acme's project and returns its row.
    async fn connect(&self, provider: &str, body: Value) -> Value {
        let (code, list) = self.call("acme", Method::PUT, &format!("/projects/{}/meeting-connections/{provider}", self.project), body).await;
        assert_eq!(code, 200, "{list}");
        list.as_array().unwrap().iter().find(|c| c["provider"] == provider).unwrap().clone()
    }

    async fn save_secret(&self, provider: &str, secret: &str) {
        let path = format!("/projects/{}/meeting-connections/{provider}/secret", self.project);
        let (code, list) = self.call("acme", Method::PUT, &path, json!({ "secret": secret })).await;
        assert_eq!(code, 200, "{list}");
    }

    async fn post(&self, provider: &str, token: &str, headers: &[(&str, String)], body: &[u8]) -> (u16, Value) {
        let mut req = self.http.post(format!("{}/meeting-webhooks/{provider}/{token}", self.base)).header("content-type", "application/json").body(body.to_vec());
        for (name, value) in headers {
            req = req.header(*name, value);
        }
        let res = req.send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn fireflies(&self, token: &str, event: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(event).unwrap();
        let sig = hex::encode(hmac_bytes(FIREFLIES_SECRET.as_bytes(), &body));
        self.post("fireflies", token, &[("x-hub-signature", sig)], &body).await
    }

    async fn fathom(&self, token: &str, id: &str, ts: i64, meeting: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(meeting).unwrap();
        let headers = [("webhook-id", id.to_string()), ("webhook-timestamp", ts.to_string()), ("webhook-signature", fathom_signature(id, ts, &body))];
        self.post("fathom", token, &headers, &body).await
    }

    async fn zoom(&self, token: &str, secret: &str, event: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(event).unwrap();
        let ts = Utc::now().timestamp();
        let headers = [("x-zm-request-timestamp", ts.to_string()), ("x-zm-signature", zoom_signature(secret, ts, &body))];
        self.post("zoom", token, &headers, &body).await
    }

    async fn meetings_of(&self, lead: Uuid) -> Vec<Value> {
        sqlx::query_scalar("select to_jsonb(m) - 'transcript' || jsonb_build_object('transcript_chars', length(m.transcript)) from meetings m where lead_id = $1 order by started_at")
            .bind(lead)
            .fetch_all(self.db())
            .await
            .unwrap()
    }

    async fn runs(&self) -> Vec<Value> {
        sqlx::query_scalar("select input from agent_runs where kind = 'meeting_notes' order by created_at").fetch_all(self.db()).await.unwrap()
    }

    /// Fireflies with its webhook secret, and a transcript of Jo's call with Sarah.
    async fn with_fireflies(&self) -> String {
        let row = self.connect("fireflies", json!({ "api_key": fake::FIREFLIES_KEY })).await;
        self.save_secret("fireflies", FIREFLIES_SECRET).await;
        self.fake.fireflies.lock().unwrap().insert("ff1".into(), fireflies_transcript("ff1", "sarah@northwind.example", Utc::now() - Duration::hours(2)));
        row["webhook_token"].as_str().unwrap().to_string()
    }
}

fn fireflies_transcript(id: &str, guest: &str, at: DateTime<Utc>) -> Value {
    json!({
        "id": id, "title": "Northwind and Fieldnote", "date": at.timestamp_millis(), "duration": 30, "transcript_url": format!("https://app.fireflies.ai/view/{id}"),
        "organizer_email": "jo@acme.example", "participants": ["jo@acme.example", guest],
        "meeting_attendees": [{ "displayName": "Jo", "email": "jo@acme.example" }, { "displayName": "Guest", "email": guest }],
        "sentences": [{ "speaker_name": "Jo", "text": "What slows your team down?" }, { "speaker_name": "Guest", "text": "Reports. Send me pricing for 40 seats." }],
        "summary": { "overview": "They want pricing.", "action_items": "Send pricing" },
    })
}

fn transcribed(id: &str) -> Value {
    json!({ "meetingId": id, "eventType": "Transcription completed" })
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_fireflies_meeting_is_kept_once_for_the_lead_it_was_with(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/meeting-connections/fireflies", s.project);
    let (code, err) = s.call("acme", Method::PUT, &path, json!({ "api_key": "wrong" })).await;
    assert!(code == 400 && err["error"].as_str().unwrap().contains("didn't accept"), "{err}");
    assert_eq!(s.call("beta", Method::PUT, &path, json!({ "api_key": fake::FIREFLIES_KEY })).await.0, 404, "Beta can't connect Acme's project");
    let token = s.with_fireflies().await;
    let stored: String = sqlx::query_scalar("select credentials_enc from meeting_connections").fetch_one(s.db()).await.unwrap();
    assert!(!stored.contains(fake::FIREFLIES_KEY), "the key is encrypted");

    // Unsigned, signed with something else, or for another token.
    let body = serde_json::to_vec(&transcribed("ff1")).unwrap();
    assert_eq!(s.post("fireflies", &token, &[], &body).await.0, 401);
    let wrong = hex::encode(hmac_bytes(b"not-the-secret", &body));
    assert_eq!(s.post("fireflies", &token, &[("x-hub-signature", wrong)], &body).await.0, 401);
    assert_eq!(s.fireflies("not-a-token", &transcribed("ff1")).await.0, 404);
    assert!(s.meetings_of(s.sarah).await.is_empty());

    let (code, res) = s.fireflies(&token, &transcribed("ff1")).await;
    assert_eq!((code, res["matched"].as_bool()), (200, Some(true)), "{res}");
    let (_, again) = s.fireflies(&token, &transcribed("ff1")).await;
    assert_eq!(again, json!({ "duplicate": true }));
    let kept = s.meetings_of(s.sarah).await;
    assert_eq!(kept.len(), 1);
    let m = &kept[0];
    assert_eq!((m["provider"].as_str(), m["external_id"].as_str(), m["duration_secs"].as_i64()), (Some("fireflies"), Some("ff1"), Some(1800)));
    assert_eq!(m["attendees"], json!([{ "name": "Guest", "email": "sarah@northwind.example" }]), "the team is left out");
    assert_eq!((m["recording_url"].as_str(), m["provider_summary"].as_str()), (Some("https://app.fireflies.ai/view/ff1"), Some("They want pricing.\n\nAction items:\nSend pricing")));
    assert!(m["transcript_chars"].as_i64().unwrap() > 0);
    assert!(s.meetings_of(s.beta_sarah).await.is_empty(), "Beta's lead with the same address is another organization's");
    assert_eq!(s.runs().await, vec![json!({ "meeting_id": m["id"] })], "one run for the meeting");

    // A new delivery of the same meeting, under another event id, still keeps it once.
    let (_, res) = s.fireflies(&token, &json!({ "meeting_id": "ff1", "event": "meeting.transcribed" })).await;
    assert_eq!(res["matched"], true);
    assert_eq!((s.meetings_of(s.sarah).await.len(), s.runs().await.len()), (1, 1));
    // Other events, and meetings Fireflies doesn't have, are answered without a meeting.
    assert_eq!(s.fireflies(&token, &json!({ "meetingId": "ff1", "eventType": "Meeting started" })).await.1["matched"], false);
    assert_eq!(s.fireflies(&token, &transcribed("gone")).await.1["matched"], false);

    // The lead's timeline has it, and only Acme sees it.
    let (_, detail) = s.call("acme", Method::GET, &format!("/leads/{}", s.sarah), json!({})).await;
    assert_eq!(detail["meetings"][0]["title"], "Northwind and Fieldnote");
    assert!(detail["meetings"][0].get("transcript").is_none(), "transcripts stay out of the timeline");
    assert_eq!(s.call("beta", Method::GET, &format!("/leads/{}/meetings", s.sarah), json!({})).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn meetings_match_by_address_then_company_domain_but_never_free_mail(db: PgPool) {
    let s = Seeded::start(db).await;
    let guest = |email: &str| vec![Attendee { name: "Jo".into(), email: "jo@acme.example".into(), internal: true }, Attendee { name: String::new(), email: email.into(), internal: false }];
    let matched = |a: Vec<Attendee>| {
        let db = s.db().clone();
        let (org, project) = (s.acme, s.project);
        async move { match_lead(&db, org, project, &a).await.unwrap() }
    };
    assert_eq!(matched(guest("Sarah@Northwind.example")).await, Some(s.sarah), "by address, whatever the case");
    assert_eq!(matched(guest("ceo@contoso.example")).await, Some(s.tom), "by domain, without the www.");
    assert_eq!(matched(guest("someone@gmail.com")).await, None, "a free mail domain is a person, not a company");
    assert!(matched(guest("gina@gmail.com")).await.is_some(), "but the exact address still matches");
    assert_eq!(matched(guest("x@elsewhere.example")).await, None);
    // The team never counts: a member's address, or a guest at the team's own domain, even marked external.
    assert_eq!(matched(vec![Attendee { name: String::new(), email: "jo@acme.example".into(), internal: false }]).await, None);
    sqlx::query("update leads set company_domain = 'fieldnote.example' where id = $1").bind(s.tom).execute(s.db()).await.unwrap();
    assert_eq!(matched(guest("intern@fieldnote.example")).await, None, "the mailbox's domain is the team's");

    // Fathom's webhook, whose meeting is with someone at Contoso. Its transcript comes from the recording.
    let row = s.connect("fathom", json!({ "api_key": fake::FATHOM_KEY })).await;
    let token = row["webhook_token"].as_str().unwrap().to_string();
    s.save_secret("fathom", &fathom_secret()).await;
    sqlx::query("update leads set company_domain = 'contoso.example' where id = $1").bind(s.tom).execute(s.db()).await.unwrap();
    s.fake.fathom_transcripts.lock().unwrap().insert("777".into(), json!([{ "speaker": { "display_name": "Ana" }, "text": "Can we start in May?" }]));
    let start = Utc::now() - Duration::hours(3);
    let meeting = |id: i64, email: &str| {
        json!({
            "recording_id": id, "meeting_title": "Contoso demo", "url": format!("https://fathom.video/calls/{id}"), "share_url": format!("https://fathom.video/share/{id}"),
            "recording_start_time": start.to_rfc3339(), "recording_end_time": (start + Duration::minutes(25)).to_rfc3339(),
            "calendar_invitees": [{ "name": "Ana", "email": email, "is_external": true }, { "name": "Jo", "email": "jo@acme.example", "is_external": false }],
            "recorded_by": { "name": "Jo", "email": "jo@acme.example" },
            "default_summary": { "markdown_formatted": "## Demo\nThey want to start in May." },
        })
    };
    let now = Utc::now().timestamp();
    assert_eq!(s.fathom(&token, "msg_1", now - 600, &meeting(777, "ana@contoso.example")).await.0, 401, "stale");
    let (code, res) = s.fathom(&token, "msg_1", now, &meeting(777, "ana@contoso.example")).await;
    assert_eq!((code, res["matched"].as_bool()), (200, Some(true)), "{res}");
    let m = &s.meetings_of(s.tom).await[0];
    assert_eq!((m["duration_secs"].as_i64(), m["recording_url"].as_str()), (Some(1500), Some("https://fathom.video/share/777")));
    let transcript: String = sqlx::query_scalar("select transcript from meetings where id = ($1::text)::uuid").bind(m["id"].as_str()).fetch_one(s.db()).await.unwrap();
    assert_eq!(transcript, "Ana: Can we start in May?");
    assert_eq!(s.fathom(&token, "msg_1", now, &meeting(777, "ana@contoso.example")).await.1, json!({ "duplicate": true }));
    let (_, res) = s.fathom(&token, "msg_2", now, &meeting(778, "friend@gmail.com")).await;
    assert_eq!(res["matched"], false);
    assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from meetings").fetch_one(s.db()).await.unwrap(), 1, "unmatched meetings aren't kept");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn zoom_answers_its_url_check_and_links_the_meeting_to_its_booking(db: PgPool) {
    let s = Seeded::start(db).await;
    let zoom_body = |token: &str| json!({ "account_id": fake::ZOOM_ACCOUNT, "client_id": fake::ZOOM_CLIENT, "client_secret": fake::ZOOM_SECRET, "secret_token": token });
    let path = format!("/projects/{}/meeting-connections/zoom", s.project);
    let mut wrong = zoom_body(ZOOM_SECRET_TOKEN);
    wrong["client_secret"] = json!("nope");
    assert_eq!(s.call("acme", Method::PUT, &path, wrong).await.0, 400);
    assert_eq!(s.call("acme", Method::PUT, &path, zoom_body("")).await.0, 400, "the secret token comes with the app");
    let row = s.connect("zoom", zoom_body(ZOOM_SECRET_TOKEN)).await;
    assert_eq!(row["has_secret"], true);
    let token = row["webhook_token"].as_str().unwrap().to_string();

    let challenge = json!({ "event": "endpoint.url_validation", "payload": { "plainToken": "qgg8vlvZRS6UYooatFL8Aw" }, "event_ts": 1 });
    let (code, answer) = s.zoom(&token, ZOOM_SECRET_TOKEN, &challenge).await;
    assert_eq!(code, 200);
    assert_eq!(answer, zoom::validation(ZOOM_SECRET_TOKEN, "qgg8vlvZRS6UYooatFL8Aw"));
    assert_eq!(s.zoom(&token, "not-the-token", &challenge).await.0, 401, "a challenge is only answered when it's signed");

    // Sarah booked through Jo's booking page 10 minutes before the call started.
    let start = Utc::now() - Duration::hours(1);
    let page: Uuid = sqlx::query_scalar("insert into booking_pages (org_id, mailbox_id, slug) values ($1, $2, 'jo-intro') returning id")
        .bind(s.acme)
        .bind(s.mailbox)
        .fetch_one(s.db())
        .await
        .unwrap();
    let booking: Uuid = sqlx::query_scalar(
        "insert into bookings (page_id, starts_at, ends_at, name, email, token_hash) values ($1, $2, $2 + interval '30 minutes', 'Sarah', 'SARAH@northwind.example', 'h1')
         returning id",
    )
    .bind(page)
    .bind(start - Duration::minutes(10))
    .fetch_one(s.db())
    .await
    .unwrap();
    // Tom joined too, and his own booking starts closer to the call. It's his, not Sarah's.
    sqlx::query(
        "insert into bookings (page_id, starts_at, ends_at, name, email, token_hash) values ($1, $2, $2 + interval '30 minutes', 'Tom', 'tom@contoso.example', 'h2')",
    )
    .bind(page)
    .bind(start)
    .execute(s.db())
    .await
    .unwrap();
    let uuid = "4444AAAiAAAAAiAiAiiAii==";
    s.fake.zoom_participants.lock().unwrap().insert(
        uuid.into(),
        json!({ "participants": [{ "name": "Jo", "user_email": "jo@acme.example" }, { "name": "Sarah Kim", "user_email": "sarah@northwind.example" },
                             { "name": "Tom Ruiz", "user_email": "tom@contoso.example" }] }),
    );
    // Longer than what's kept.
    let line = "Sarah Kim: We need this live before the audit in March.\n";
    *s.fake.zoom_vtt.lock().unwrap() = format!("WEBVTT\n\n{}", line.repeat(TRANSCRIPT_CHARS / line.len() + 100));
    let completed = |download: String| {
        json!({
            "event": "recording.transcript_completed", "event_ts": 1_790_000_000_000i64, "download_token": fake::ZOOM_DOWNLOAD_TOKEN,
            "payload": { "account_id": fake::ZOOM_ACCOUNT, "object": {
                "uuid": uuid, "id": 123, "host_email": "jo@acme.example", "topic": "Fieldnote x Northwind", "start_time": start.to_rfc3339(), "duration": 45,
                "share_url": "https://zoom.us/rec/share/abc",
                "recording_files": [{ "file_type": "MP4", "download_url": "https://zoom.us/rec/download/video" }, { "file_type": "TRANSCRIPT", "download_url": download }],
            } },
        })
    };
    let fake_base = s.state.cfg.meetings_api_url.clone();
    assert_eq!(s.zoom(&token, "not-the-token", &completed(format!("{fake_base}/zoom/download/t.vtt"))).await.0, 401);
    let (code, res) = s.zoom(&token, ZOOM_SECRET_TOKEN, &completed(format!("{fake_base}/zoom/download/t.vtt"))).await;
    assert_eq!((code, res["matched"].as_bool()), (200, Some(true)), "{res}");
    let m = &s.meetings_of(s.sarah).await[0];
    assert_eq!(m["booking_id"].as_str(), Some(booking.to_string().as_str()), "linked to the booking it was");
    assert_eq!((m["title"].as_str(), m["duration_secs"].as_i64()), (Some("Fieldnote x Northwind"), Some(2700)));
    assert_eq!(m["transcript_chars"].as_u64(), Some(TRANSCRIPT_CHARS as u64), "the transcript is capped");
    assert_eq!(s.zoom(&token, ZOOM_SECRET_TOKEN, &completed(String::new())).await.1, json!({ "duplicate": true }));

    // A transcript URL that isn't Zoom's is never fetched with the token.
    let mut elsewhere = completed("https://attacker.example/steal".into());
    elsewhere["payload"]["object"]["uuid"] = json!("other-meeting");
    let before = s.fake.hits("/zoom/download");
    assert_eq!(s.zoom(&token, ZOOM_SECRET_TOKEN, &elsewhere).await.0, 200);
    assert_eq!(s.fake.hits("/zoom/download"), before);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn gong_is_read_a_page_at_a_time_from_where_the_last_read_ended(db: PgPool) {
    let s = Seeded::start(db).await;
    let path = format!("/projects/{}/meeting-connections/gong", s.project);
    assert_eq!(s.call("acme", Method::PUT, &path, json!({ "access_key": fake::GONG_KEY, "access_secret": "wrong" })).await.0, 400);
    let row = s.connect("gong", json!({ "access_key": fake::GONG_KEY, "access_secret": fake::GONG_SECRET })).await;
    assert_eq!(row["synced_at"], Value::Null);
    let call = |id: &str, email: &str, hours_ago: i64| {
        json!({
            "metaData": { "id": id, "title": format!("Call {id}"), "started": (Utc::now() - Duration::hours(hours_ago)).to_rfc3339(), "duration": 1800,
                          "url": format!("https://app.gong.io/call?id={id}") },
            "parties": [{ "speakerId": "s1", "name": "Jo", "emailAddress": "jo@acme.example", "affiliation": "Internal" },
                        { "speakerId": "s2", "name": "Guest", "emailAddress": email, "affiliation": "External" }],
            "content": { "brief": format!("Brief of {id}"), "keyPoints": [{ "text": "Budget is set" }] },
        })
    };
    {
        let mut calls = s.fake.gong_calls.lock().unwrap();
        calls.push(call("g1", "sarah@northwind.example", 30));
        calls.push(call("g2", "nobody@elsewhere.example", 20));
        calls.push(call("g3", "pat@contoso.example", 5));
        let mut transcripts = s.fake.gong_transcripts.lock().unwrap();
        for id in ["g1", "g2", "g3"] {
            transcripts.insert(id.into(), json!([{ "speakerId": "s1", "sentences": [{ "text": "Hi." }, { "text": "Ready?" }] }, { "speakerId": "s2", "sentences": [{ "text": "Yes." }] }]));
        }
    }

    crate::meetings::poll_due(&s.state).await.unwrap();
    let requests = s.fake.gong_requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2, "three calls are two pages");
    assert_eq!((requests[0].get("cursor"), requests[1]["cursor"].as_str()), (None, Some("page-2")));
    let from = DateTime::parse_from_rfc3339(requests[0]["filter"]["fromDateTime"].as_str().unwrap()).unwrap();
    assert!((Utc::now() - from.with_timezone(&Utc) - gong::FIRST_LOOK_BACK).num_seconds().abs() < 60, "the first read goes back a week");
    assert_eq!(s.fake.hits("/gong/v2/calls/transcript"), 2, "only the calls with a lead get their transcript");
    let sarahs = s.meetings_of(s.sarah).await;
    assert_eq!((sarahs.len(), sarahs[0]["provider_summary"].as_str()), (1, Some("Brief of g1\n\nKey points:\n- Budget is set")));
    let transcript: String = sqlx::query_scalar("select transcript from meetings where external_id = 'g1'").fetch_one(s.db()).await.unwrap();
    assert_eq!(transcript, "Jo: Hi. Ready?\nGuest: Yes.");
    assert_eq!(s.meetings_of(s.tom).await.len(), 1, "Pat at Contoso is Tom's company");
    let (cursor, synced): (Option<DateTime<Utc>>, Option<DateTime<Utc>>) =
        sqlx::query_as("select sync_cursor, synced_at from meeting_connections").fetch_one(s.db()).await.unwrap();
    let cursor = cursor.unwrap();
    assert!(synced.is_some());

    // Not due again for 15 minutes.
    crate::meetings::poll_due(&s.state).await.unwrap();
    assert_eq!(s.fake.gong_requests.lock().unwrap().len(), 2);
    // Then it reads from 6 hours before the cursor, which takes in g3 again, and what it already kept isn't kept or
    // summarized twice.
    sqlx::query("update meeting_connections set synced_at = now() - interval '16 minutes'").execute(s.db()).await.unwrap();
    s.fake.gong_calls.lock().unwrap().push(call("g4", "sarah@northwind.example", 1));
    sqlx::query("update meeting_connections set sync_cursor = now() - interval '2 hours'").execute(s.db()).await.unwrap();
    crate::meetings::poll_due(&s.state).await.unwrap();
    let last = s.fake.gong_requests.lock().unwrap().last().cloned().unwrap();
    let from = DateTime::parse_from_rfc3339(last["filter"]["fromDateTime"].as_str().unwrap()).unwrap().with_timezone(&Utc);
    assert!((Utc::now() - Duration::hours(8) - from).num_seconds().abs() < 60, "{from} is 6 hours before the cursor");
    assert!(cursor > from);
    assert_eq!(s.fake.hits("/gong/v2/calls/transcript"), 3, "g3 was kept already, so only g4 is fetched");
    assert_eq!((s.meetings_of(s.sarah).await.len(), s.meetings_of(s.tom).await.len()), (2, 1));
    assert_eq!(s.runs().await.len(), 3, "a run per meeting");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_notes_draft_a_follow_up_and_the_stage_moves_only_on_a_click(db: PgPool) {
    let s = Seeded::start(db).await;
    let token = s.with_fireflies().await;
    s.fireflies(&token, &transcribed("ff1")).await;
    let meeting: Uuid = sqlx::query_scalar("select id from meetings where lead_id = $1").bind(s.sarah).fetch_one(s.db()).await.unwrap();
    let deal_stage = || sqlx::query_scalar::<_, String>("select s.name from deals d join deal_stages s on s.id = d.stage_id where d.lead_id = $1").bind(s.sarah).fetch_one(s.db());
    assert_eq!(deal_stage().await.unwrap(), "Interested");

    let (outcome, prompt) = run_notes(&s.state, meeting, answer("Proposal")).await;
    assert_eq!(outcome.message, "Wrote the meeting notes, drafted the follow-up and suggested Proposal");
    assert!(prompt.contains("Guest: Reports. Send me pricing for 40 seats.") && prompt.contains("The deal is in Interested"), "{prompt}");
    assert!(prompt.contains("Interested, Meeting booked, Proposal, Negotiation, Won, Lost") && prompt.contains("They want pricing."), "{prompt}");
    let cost: f64 = sqlx::query_scalar("select cost_usd from agent_runs where kind = 'meeting_notes'").fetch_one(s.db()).await.unwrap();
    assert!((cost - 0.02).abs() < 1e-9, "the run carries its cost: {cost}");

    let (_, rows) = s.call("acme", Method::GET, &format!("/leads/{}/meetings", s.sarah), json!({})).await;
    let row = &rows[0];
    assert_eq!(row["summary"], "Sarah's team loses a day a week to manual reports. They liked the demo and want pricing for 40 seats.");
    assert_eq!(row["next_steps"], json!(["Send pricing for 40 seats", "Book a call with their CFO"]));
    assert_eq!((row["suggested_stage"].as_str(), row["stage_status"].as_str(), row["follow_up_status"].as_str()), (Some("Proposal"), Some("pending"), Some("draft")));
    assert_eq!(deal_stage().await.unwrap(), "Interested", "a suggestion never moves the deal by itself");

    // The draft waits with the other drafts, from the mailbox that emailed Sarah, threaded under that email.
    let (_, drafts) = s.call("acme", Method::GET, &format!("/emails?project_id={}&status=draft", s.project), json!({})).await;
    let draft = &drafts["items"][0];
    assert_eq!((draft["kind"].as_str(), draft["subject"].as_str()), (Some("meeting_follow_up"), Some("Re: Quick idea")), "{drafts}");
    assert_eq!(draft["mailbox_id"].as_str(), Some(s.mailbox.to_string().as_str()));
    assert!(draft["body"].as_str().unwrap().starts_with("Hi Sarah, thanks for your time today."));
    assert_eq!(drafts["total"], 1);

    // A second run for the meeting writes nothing more.
    let (again, _) = run_notes(&s.state, meeting, answer("Negotiation")).await;
    assert_eq!(again.message, "The meeting already has notes");
    assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from email_messages where kind = 'meeting_follow_up'").fetch_one(s.db()).await.unwrap(), 1);

    // Beta can't decide it, Acme accepts it with one click, and a second click finds it settled.
    let stage = format!("/meetings/{meeting}/stage");
    assert_eq!(s.call("beta", Method::POST, &stage, json!({ "accept": true })).await.0, 404);
    let (code, accepted) = s.call("acme", Method::POST, &stage, json!({ "accept": true })).await;
    assert_eq!((code, accepted["stage_status"].as_str()), (200, Some("accepted")), "{accepted}");
    assert_eq!(deal_stage().await.unwrap(), "Proposal");
    assert_eq!(s.call("acme", Method::POST, &stage, json!({ "accept": false })).await.0, 409);

    // The deal card shows the latest meeting and its summary.
    let (_, board) = s.call("acme", Method::GET, &format!("/pipeline?project_id={}", s.project), json!({})).await;
    let card = board["deals"].as_array().unwrap().iter().find(|d| d["lead_id"] == s.sarah.to_string()).unwrap();
    assert_eq!((card["meeting_title"].as_str(), card["meeting_summary"].as_str()), (Some("Northwind and Fieldnote"), row["summary"].as_str()));

    // A stage the deal is already in is no suggestion, and a dismissed one leaves the deal alone.
    s.fake.fireflies.lock().unwrap().insert("ff2".into(), fireflies_transcript("ff2", "sarah@northwind.example", Utc::now() - Duration::hours(1)));
    s.fireflies(&token, &transcribed("ff2")).await;
    let second: Uuid = sqlx::query_scalar("select id from meetings where external_id = 'ff2'").fetch_one(s.db()).await.unwrap();
    let (outcome, _) = run_notes(&s.state, second, answer("proposal")).await;
    assert_eq!(outcome.message, "Wrote the meeting notes and drafted the follow-up");
    assert_eq!(s.call("acme", Method::POST, &format!("/meetings/{second}/stage"), json!({ "accept": true })).await.0, 409, "nothing to accept");
    s.fake.fireflies.lock().unwrap().insert("ff3".into(), fireflies_transcript("ff3", "sarah@northwind.example", Utc::now()));
    s.fireflies(&token, &transcribed("ff3")).await;
    let third: Uuid = sqlx::query_scalar("select id from meetings where external_id = 'ff3'").fetch_one(s.db()).await.unwrap();
    run_notes(&s.state, third, answer("Won")).await;
    let (_, dismissed) = s.call("acme", Method::POST, &format!("/meetings/{third}/stage"), json!({ "accept": false })).await;
    assert_eq!(dismissed["stage_status"], "dismissed");
    assert_eq!(deal_stage().await.unwrap(), "Proposal");

    // Tom has no deal, so accepting opens one in the suggested stage. No email goes to a lead who opted out.
    sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(s.tom).execute(s.db()).await.unwrap();
    s.fake.fireflies.lock().unwrap().insert("ff4".into(), fireflies_transcript("ff4", "tom@contoso.example", Utc::now()));
    s.fireflies(&token, &transcribed("ff4")).await;
    let toms: Uuid = sqlx::query_scalar("select id from meetings where external_id = 'ff4'").fetch_one(s.db()).await.unwrap();
    let (outcome, _) = run_notes(&s.state, toms, answer("Meeting booked")).await;
    assert!(outcome.message.contains("No follow-up"), "{}", outcome.message);
    assert_eq!(s.call("acme", Method::POST, &format!("/meetings/{toms}/stage"), json!({ "accept": true })).await.0, 200);
    let tom_stage: String = sqlx::query_scalar("select s.name from deals d join deal_stages s on s.id = d.stage_id where d.lead_id = $1").bind(s.tom).fetch_one(s.db()).await.unwrap();
    assert_eq!(tom_stage, "Meeting booked");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_api_and_mcp_list_a_leads_meetings_for_the_keys_organization(db: PgPool) {
    let s = Seeded::start(db).await;
    let token = s.with_fireflies().await;
    s.fireflies(&token, &transcribed("ff1")).await;
    let meeting: Uuid = sqlx::query_scalar("select id from meetings").fetch_one(s.db()).await.unwrap();
    run_notes(&s.state, meeting, answer("Proposal")).await;

    let (code, list) = s.api(&format!("/v1/leads/{}/meetings", s.sarah)).await;
    assert_eq!((code, list[0]["suggested_stage"].as_str()), (200, Some("Proposal")), "{list}");
    assert!(list[0].get("transcript").is_none());
    assert_eq!(s.api(&format!("/v1/leads/{}/meetings", s.beta_sarah)).await.0, 404);

    let mcp = |arguments: Value| {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": { "name": "list_meetings", "arguments": arguments } });
        let req = s.http.post(format!("{}/mcp", s.base)).bearer_auth(KEY).header("accept", "application/json, text/event-stream").json(&body);
        async move {
            let res: Value = req.send().await.unwrap().json().await.unwrap();
            (res["result"]["isError"].as_bool().unwrap(), res["result"]["content"][0]["text"].as_str().unwrap().to_string())
        }
    };
    let (error, text) = mcp(json!({ "lead_id": s.sarah })).await;
    assert!(!error && text.contains("Send pricing for 40 seats") && text.contains("\"stage_status\":\"pending\"") && !text.contains("external_id"), "{text}");
    let (error, _) = mcp(json!({ "lead_id": s.beta_sarah })).await;
    assert!(error, "another organization's lead");
}
