use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use ring::agreement::{ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey, agree_ephemeral};
use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey as SignatureKey};
use serde_json::Value;
use sqlx::PgPool;

use super::*;
use crate::crypto::sha256_hex;

fn d(text: &str) -> Vec<u8> {
    b64(text).unwrap()
}

/// What a browser does with a message: the reverse of `encrypt`, from the header's key and salt and its own private key.
/// None when the key doesn't open it.
fn decrypt(ua_private: EphemeralPrivateKey, ua_public: &[u8], auth: &[u8], body: &[u8]) -> Option<Vec<u8>> {
    let (salt, rest) = body.split_at(16);
    assert_eq!(rest[..4], RECORD_SIZE.to_be_bytes());
    let key_length = rest[4] as usize;
    let (as_public, ciphertext) = rest[5..].split_at(key_length);
    let ecdh = agree_ephemeral(ua_private, &UnparsedPublicKey::new(&ECDH_P256, as_public), |s| s.to_vec()).unwrap();
    let key_info = [b"WebPush: info\0".as_slice(), ua_public, as_public].concat();
    let ikm = hkdf_sha256(auth, &ecdh, &key_info, 32);
    let cek = hkdf_sha256(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf_sha256(salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).unwrap());
    let mut record = ciphertext.to_vec();
    let plain = key.open_in_place(aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(), aead::Aad::empty(), &mut record).ok()?;
    assert_eq!(plain.last(), Some(&2), "the last record ends in its delimiter");
    Some(plain[..plain.len() - 1].to_vec())
}

/// A browser's subscription keys, with the private key that reads what's sent to it.
struct Browser {
    private: EphemeralPrivateKey,
    p256dh: String,
    auth: String,
}

fn browser() -> Browser {
    let rng = SystemRandom::new();
    let private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
    let p256dh = URL_SAFE_NO_PAD.encode(private.compute_public_key().unwrap().as_ref());
    let mut auth = [0u8; 16];
    rng.fill(&mut auth).unwrap();
    Browser { private, p256dh, auth: URL_SAFE_NO_PAD.encode(auth) }
}

#[test]
fn the_rfc_8291_example_encrypts_byte_for_byte() {
    // Section 5 and Appendix A. The ECDH secret is the RFC's, since ring doesn't take a fixed private key.
    let body = seal(
        &d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"),
        &d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"),
        &d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4"),
        &d("BTBZMqHH6r4Tts7J_aSIgg"),
        &d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap(),
        b"When I grow up, I want to be a watermelon",
    )
    .unwrap();
    let expected = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_\
                    yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    assert_eq!(URL_SAFE_NO_PAD.encode(body), expected);
}

#[test]
fn a_message_decrypts_with_the_browsers_key_and_nothing_else() {
    let (b, other) = (browser(), browser());
    let message = br#"{"title":"Hot lead for Fieldnote","body":"Sarah Chen (Acme): Tuesday works"}"#;
    let body = encrypt(&d(&b.p256dh), &d(&b.auth), message).unwrap();
    assert_eq!(decrypt(b.private, &d(&b.p256dh), &d(&b.auth), &body).unwrap(), message);
    assert_eq!(decrypt(other.private, &d(&other.p256dh), &d(&other.auth), &body), None, "another browser's key doesn't open it");
    assert!(encrypt(&d(&browser().p256dh), &d(&browser().auth), &vec![b'x'; MAX_PLAINTEXT + 1]).is_err(), "too long for a push service");
}

#[test]
fn the_vapid_token_verifies_with_the_public_key_and_names_the_push_service() {
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(ALG, &rng).unwrap();
    let vapid = Vapid::new(EcdsaKeyPair::from_pkcs8(ALG, pkcs8.as_ref(), &rng).unwrap());
    let endpoint = Url::parse("https://fcm.googleapis.com:443/fcm/send/abc123").unwrap();
    let header = vapid.authorization(&endpoint, "mailto:ops@splee.test").unwrap();
    let (token, key) = header.strip_prefix("vapid t=").unwrap().split_once(", k=").unwrap();
    assert_eq!(key, vapid.public);
    assert_eq!(d(key).len(), 65);

    let (input, signature) = token.rsplit_once('.').unwrap();
    SignatureKey::new(&ECDSA_P256_SHA256_FIXED, d(key)).verify(input.as_bytes(), &d(signature)).expect("signed by the key it names");
    let other = EcdsaKeyPair::generate_pkcs8(ALG, &rng).unwrap();
    let other = EcdsaKeyPair::from_pkcs8(ALG, other.as_ref(), &rng).unwrap();
    assert!(SignatureKey::new(&ECDSA_P256_SHA256_FIXED, other.public_key().as_ref()).verify(input.as_bytes(), &d(signature)).is_err());

    let (head, claims) = input.split_once('.').unwrap();
    let head: Value = serde_json::from_slice(&d(head)).unwrap();
    assert_eq!((head["alg"].as_str(), head["typ"].as_str()), (Some("ES256"), Some("JWT")));
    let claims: Value = serde_json::from_slice(&d(claims)).unwrap();
    assert_eq!(claims["aud"], "https://fcm.googleapis.com", "the origin, without the default port or path");
    assert_eq!(claims["sub"], "mailto:ops@splee.test");
    let left = claims["exp"].as_i64().unwrap() - chrono::Utc::now().timestamp();
    assert!((11 * 3600..=24 * 3600).contains(&left), "RFC 8292 allows at most 24 hours: {left}");
}

#[test]
fn only_a_browsers_real_subscription_is_saved() {
    let b = browser();
    assert_eq!(invalid("https://fcm.googleapis.com/fcm/send/abc", &b.p256dh, &b.auth), None);
    assert_eq!(invalid("https://web.push.apple.com/abc", &format!("{}=", b.p256dh), &format!("{}==", b.auth)), None, "padding is fine");
    assert!(invalid("http://push.example.com/abc", &b.p256dh, &b.auth).is_some(), "plain http");
    assert!(invalid("not a url", &b.p256dh, &b.auth).is_some());
    let mut off_curve = d(&b.p256dh);
    off_curve[64] ^= 1;
    assert!(invalid("https://fcm.googleapis.com/x", &URL_SAFE_NO_PAD.encode(off_curve), &b.auth).is_some(), "not a point on P-256");
    assert!(invalid("https://fcm.googleapis.com/x", &b.p256dh, "c2hvcnQ").is_some(), "auth too short");
}

#[test]
fn devices_are_named_by_browser_and_system() {
    let mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
    assert_eq!(device_label(mac), "Chrome on macOS");
    let edge = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36 Edg/141.0.0.0";
    assert_eq!(device_label(edge), "Edge on Windows");
    let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.5 Mobile/15E148 Safari/604.1";
    assert_eq!(device_label(iphone), "Safari on iPhone");
    assert_eq!(device_label("Mozilla/5.0 (Android 15; Mobile; rv:140.0) Gecko/140.0 Firefox/140.0"), "Firefox on Android");
    assert_eq!(device_label(""), "A browser");
}

#[test]
fn long_text_is_cut_on_one_line() {
    assert_eq!(cut("Sounds good.\n\nTuesday?", 100), "Sounds good. Tuesday?");
    assert_eq!(cut("abcdef", 4), "abc…");
    assert_eq!(cut("abcd", 4), "abcd");
}

/// A request the fake push service got: its path, headers and body.
type Hit = (String, HeaderMap, Vec<u8>);

/// A push service on this machine. It answers 410 for /gone, 500 for /down… and 201 for anything else, and keeps every request.
#[derive(Clone, Default)]
struct PushService {
    base: String,
    hits: Arc<Mutex<Vec<Hit>>>,
}

impl PushService {
    async fn start() -> Self {
        let hits: Arc<Mutex<Vec<Hit>>> = Arc::default();
        let keep = hits.clone();
        let app = Router::new().route(
            "/{id}",
            post(move |Path(id): Path<String>, headers: HeaderMap, body: bytes::Bytes| {
                let keep = keep.clone();
                async move {
                    keep.lock().unwrap().push((id.clone(), headers, body.to_vec()));
                    match id.as_str() {
                        "gone" => StatusCode::GONE,
                        id if id.starts_with("down") => StatusCode::INTERNAL_SERVER_ERROR,
                        _ => StatusCode::CREATED,
                    }
                }
            }),
        );
        let port = crate::hooks::serve_test_receiver(app).await;
        Self { base: format!("http://127.0.0.1:{port}"), hits }
    }

    fn paths(&self) -> Vec<String> {
        let mut paths: Vec<String> = self.hits.lock().unwrap().iter().map(|(p, _, _)| p.clone()).collect();
        paths.sort();
        paths
    }

    fn take(&self) -> Vec<Hit> {
        std::mem::take(&mut *self.hits.lock().unwrap())
    }
}

fn state(db: &PgPool) -> AppState {
    let provider = crate::mail::oauth::tests::FakeProvider { url: String::new(), hits: Arc::default(), forms: Arc::default() };
    let cfg = crate::config::Config { public_url: "https://splee.test".into(), ..crate::mail::oauth::tests::config(&provider) };
    AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0, rate_limits: Default::default() }
}

async fn org(db: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap()
}

/// A member signed in with the session "session-{name}".
async fn member(db: &PgPool, org: Uuid, name: &str) -> Uuid {
    let id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, 'x') returning id")
        .bind(format!("{name}@splee.test"))
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(org).bind(id).execute(db).await.unwrap();
    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
        .bind(sha256_hex(&format!("session-{name}")))
        .bind(id)
        .bind(org)
        .execute(db)
        .await
        .unwrap();
    id
}

/// Subscribes the user at {base}/{path} and returns the browser, for reading what it gets.
async fn subscribe(db: &PgPool, user: Uuid, service: &PushService, path: &str) -> Browser {
    let b = browser();
    sqlx::query("insert into push_subscriptions (user_id, endpoint, p256dh, auth) values ($1, $2, $3, $4)")
        .bind(user)
        .bind(format!("{}/{path}", service.base))
        .bind(&b.p256dh)
        .bind(&b.auth)
        .execute(db)
        .await
        .unwrap();
    b
}

async fn lead(db: &PgPool, org: Uuid, owner: Option<Uuid>) -> Lead {
    let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(db).await.unwrap();
    sqlx::query_as("insert into leads (project_id, full_name, company_name, email, owner_id) values ($1, 'Sarah Chen', 'Acme', 'sarah@acme.test', $2) returning *")
        .bind(project)
        .bind(owner)
        .fetch_one(db)
        .await
        .unwrap()
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_hot_reply_pushes_to_its_owner_or_the_team_minus_whoever_is_away_or_opted_out(db: PgPool) {
    let state = state(&db);
    let service = PushService::start().await;
    let (acme, beta) = (org(&db, "Acme").await, org(&db, "Beta").await);
    let (ana, ben, cy, dee) = (member(&db, acme, "ana").await, member(&db, acme, "ben").await, member(&db, acme, "cy").await, member(&db, acme, "dee").await);
    let eve = member(&db, beta, "eve").await;
    let ana_phone = subscribe(&db, ana, &service, "ana-phone").await;
    subscribe(&db, ana, &service, "ana-laptop").await;
    subscribe(&db, ben, &service, "ben").await;
    subscribe(&db, cy, &service, "cy").await;
    subscribe(&db, eve, &service, "eve").await;
    // Dee has push off: no browsers.
    sqlx::query("update memberships set away = true where user_id = $1").bind(ben).execute(&db).await.unwrap();
    sqlx::query("insert into notification_prefs (user_id, hot_lead_email) values ($1, false)").bind(cy).execute(&db).await.unwrap();

    let send = |owner: Option<Uuid>| {
        let (state, db) = (state.clone(), db.clone());
        async move {
            let lead = lead(&db, acme, owner).await;
            let push = reply(&state.cfg.public_url, &lead, "Fieldnote", "Sounds good.\nTuesday at 10?", true);
            lead_alert(&state, &lead, Pref::HotLeads, &push).await;
            lead
        }
    };

    let nobodys = send(None).await;
    assert_eq!(service.paths(), ["ana-laptop", "ana-phone"], "Ben is away, Cy turned hot leads off, Dee has no browser, Eve is in Beta");
    let hits = service.take();
    let (_, headers, body) = hits.iter().find(|(p, _, _)| p == "ana-phone").unwrap();
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["ttl"], TTL_SECS);
    assert!(headers["authorization"].to_str().unwrap().starts_with("vapid t="));
    let message: Value = serde_json::from_slice(&decrypt(ana_phone.private, &d(&ana_phone.p256dh), &d(&ana_phone.auth), body).unwrap()).unwrap();
    assert_eq!(message["title"], "Hot lead for Fieldnote");
    assert_eq!(message["body"], "Sarah Chen (Acme): Sounds good. Tuesday at 10?");
    assert_eq!(message["url"], format!("https://splee.test/app/p/{}/inbox?lead={}", nobodys.project_id, nobodys.id));
    assert_eq!(message["tag"], format!("lead-{}", nobodys.id));

    send(Some(dee)).await;
    assert!(service.take().is_empty(), "Dee's lead goes to Dee alone, and Dee has push off");
    send(Some(cy)).await;
    assert!(service.take().is_empty(), "Cy's own lead, with hot leads turned off");
    send(Some(ben)).await;
    assert_eq!(service.paths(), ["ana-laptop", "ana-phone"], "Ben is away, so the team hears instead");
    service.take();
    send(Some(eve)).await;
    assert_eq!(service.paths(), ["ana-laptop", "ana-phone"], "an owner from another org is nobody here");
    service.take();

    // A reminder goes through whatever the hot lead setting says.
    let cys = lead(&db, acme, Some(cy)).await;
    lead_alert(&state, &cys, Pref::Always, &reminder(&state.cfg.public_url, &cys, "Fieldnote")).await;
    assert_eq!(service.paths(), ["cy"]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_gone_subscription_is_deleted_and_a_failing_one_is_retried_once(db: PgPool) {
    let state = state(&db);
    let service = PushService::start().await;
    let ana = member(&db, org(&db, "Acme").await, "ana").await;
    for path in ["ok", "gone", "down"] {
        subscribe(&db, ana, &service, path).await;
    }

    let mut sent = send_test(&state, ana).await.unwrap();
    sent.sort_by_key(|s| *s as u8);
    assert_eq!(sent, [Sent::Delivered, Sent::Gone, Sent::Failed]);
    assert_eq!(service.paths(), ["down", "down", "gone", "ok"], "a 500 is tried twice and no more, a 410 once");
    let rows: Vec<(String, bool, i32)> = sqlx::query_as("select endpoint, last_success_at is not null, failures from push_subscriptions order by endpoint")
        .fetch_all(&db)
        .await
        .unwrap();
    let rows: Vec<_> = rows.into_iter().map(|(e, ok, f)| (e.rsplit('/').next().unwrap().to_string(), ok, f)).collect();
    assert_eq!(rows, [("down".to_string(), false, 1), ("ok".to_string(), true, 0)]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_approval_push_goes_out_hourly_and_only_for_new_drafts(db: PgPool) {
    let service = PushService::start().await;
    let acme = org(&db, "Acme").await;
    let (ana, ben) = (member(&db, acme, "ana").await, member(&db, acme, "ben").await);
    subscribe(&db, ana, &service, "ana").await;
    subscribe(&db, ben, &service, "ben").await;
    sqlx::query("insert into notification_prefs (user_id, daily_digest) values ($1, false)").bind(ben).execute(&db).await.unwrap();
    let lead = lead(&db, acme, None).await;
    let draft = || {
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, subject, body) values ($1, $2, 'outbound', 'initial', 'draft', 'Hi', 'Hello')")
            .bind(lead.project_id)
            .bind(lead.id)
            .execute(&db)
    };
    assert!(claim_approval_digests(&db).await.unwrap().is_empty(), "nothing waits");
    draft().await.unwrap();
    assert_eq!(claim_approval_digests(&db).await.unwrap(), [(ana, acme)], "Ben turned the daily digest off");
    draft().await.unwrap();
    assert!(claim_approval_digests(&db).await.unwrap().is_empty(), "within the hour");
    let an_hour_passes = || async {
        sqlx::query("update push_approval_digests set sent_at = sent_at - interval '61 minutes'").execute(&db).await.unwrap();
        sqlx::query("update email_messages set created_at = created_at - interval '61 minutes'").execute(&db).await.unwrap();
    };
    an_hour_passes().await;
    assert_eq!(claim_approval_digests(&db).await.unwrap(), [(ana, acme)], "a draft came in since, and an hour went by");
    an_hour_passes().await;
    assert!(claim_approval_digests(&db).await.unwrap().is_empty(), "the same drafts don't nag every hour");
}

/// The app's push routes on a fake server, as `who`.
struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn start(state: AppState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { base, client: reqwest::Client::new() }
    }

    async fn send(&self, who: &str, method: reqwest::Method, path: &str, body: Option<Value>) -> (u16, Value) {
        let mut req = self
            .client
            .request(method, format!("{}{path}", self.base))
            .header("cookie", format!("splee_session=session-{who}"))
            .header("user-agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) Gecko/20100101 Firefox/145.0");
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn each_member_sees_and_removes_only_their_own_browsers(db: PgPool) {
    use reqwest::Method;
    let state = state(&db);
    let service = PushService::start().await;
    let acme = org(&db, "Acme").await;
    member(&db, acme, "ana").await;
    let ben = member(&db, acme, "ben").await;
    let api = Api::start(state).await;

    let (_, key) = api.send("ana", Method::GET, "/push/key", None).await;
    let key = key["public_key"].as_str().unwrap().to_string();
    assert_eq!(d(&key).len(), 65);
    let (_, again) = api.send("ben", Method::GET, "/push/key", None).await;
    assert_eq!(again["public_key"], key.as_str(), "made once and kept");
    let stored: String = sqlx::query_scalar("select private_key_enc from push_vapid_keys").fetch_one(&db).await.unwrap();
    assert!(crate::crypto::decrypt("another secret", &stored).is_err(), "encrypted with APP_SECRET");

    let b = browser();
    let subscription = json!({"endpoint": format!("{}/ana", service.base), "expirationTime": null, "keys": {"p256dh": &b.p256dh, "auth": &b.auth}});
    let (status, device) = api.send("ana", Method::POST, "/push/subscriptions", Some(subscription.clone())).await;
    assert_eq!((status, device["user_agent"].as_str()), (200, Some("Firefox on macOS")), "{device}");
    let id = device["id"].as_str().unwrap().to_string();
    let (_, same) = api.send("ana", Method::POST, "/push/subscriptions", Some(subscription.clone())).await;
    assert_eq!(same["id"], id.as_str(), "sending it again updates the one row");
    let (status, _) = api.send("ana", Method::POST, "/push/subscriptions", Some(json!({"endpoint": "https://fcm.googleapis.com/x", "keys": {"p256dh": "AAAA", "auth": &b.auth}}))).await;
    assert_eq!(status, 400);

    let (_, bens) = api.send("ben", Method::GET, "/push/subscriptions", None).await;
    assert_eq!(bens, json!([]));
    let (status, _) = api.send("ben", Method::DELETE, &format!("/push/subscriptions/{id}"), None).await;
    assert_eq!(status, 404, "Ana's browser isn't Ben's to remove");
    let (_, test) = api.send("ben", Method::POST, "/push/test", None).await;
    assert_eq!(test["delivered"], 0, "Ben has no browsers");

    let (_, test) = api.send("ana", Method::POST, "/push/test", None).await;
    assert_eq!((test["delivered"].as_u64(), test["failed"].as_u64()), (Some(1), Some(0)), "{test}");
    let (_, anas) = api.send("ana", Method::GET, "/push/subscriptions", None).await;
    assert_eq!(anas[0]["id"], id.as_str());
    assert!(anas[0]["last_success_at"].is_string());
    let hits = service.take();
    let message: Value = serde_json::from_slice(&decrypt(b.private, &d(&b.p256dh), &d(&b.auth), &hits[0].2).unwrap()).unwrap();
    assert_eq!(message["title"], "Splee notifications are on");

    // Ben signs in on Ana's browser and turns push on there: it's his now.
    let (_, moved) = api.send("ben", Method::POST, "/push/subscriptions", Some(subscription)).await;
    assert_eq!(moved["id"], id.as_str());
    let (_, anas) = api.send("ana", Method::GET, "/push/subscriptions", None).await;
    assert_eq!(anas, json!([]));
    let owner: Uuid = sqlx::query_scalar("select user_id from push_subscriptions").fetch_one(&db).await.unwrap();
    assert_eq!(owner, ben);
    let (status, _) = api.send("ben", Method::DELETE, &format!("/push/subscriptions/{id}"), None).await;
    assert_eq!(status, 200);
    let left: i64 = sqlx::query_scalar("select count(*) from push_subscriptions").fetch_one(&db).await.unwrap();
    assert_eq!(left, 0);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_saved_search_pushes_its_owner_when_the_daily_digest_is_on(db: PgPool) {
    use crate::models::SavedSearch;
    use crate::saved_searches::{Found, announce};
    let state = state(&db);
    let service = PushService::start().await;
    let (acme, beta) = (org(&db, "Acme").await, org(&db, "Beta").await);
    let (ana, ben, eve) = (member(&db, acme, "ana").await, member(&db, acme, "ben").await, member(&db, beta, "eve").await);
    let ana_phone = subscribe(&db, ana, &service, "ana").await;
    subscribe(&db, ben, &service, "ben").await;
    subscribe(&db, eve, &service, "eve").await;
    sqlx::query("insert into notification_prefs (user_id, daily_digest) values ($1, false)").bind(ben).execute(&db).await.unwrap();
    let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(acme).fetch_one(&db).await.unwrap();
    let search = |owner: Uuid| {
        sqlx::query_as::<_, SavedSearch>("insert into saved_searches (project_id, user_id, name, city) values ($1, $2, 'Dentists in Leeds', 'Leeds') returning *")
            .bind(project)
            .bind(owner)
            .fetch_one(&db)
    };
    let found = Found { at: "2026-11-21T09:00:00Z".parse().unwrap(), new: 2, added: Some(("Dentists".into(), 2)) };

    let anas = search(ana).await.unwrap();
    announce(&state, &anas, &found).await;
    let hits = service.take();
    assert_eq!(hits.iter().map(|(p, _, _)| p.as_str()).collect::<Vec<_>>(), ["ana"]);
    let message: Value = serde_json::from_slice(&decrypt(ana_phone.private, &d(&ana_phone.p256dh), &d(&ana_phone.auth), &hits[0].2).unwrap()).unwrap();
    assert_eq!(message["title"], "New companies for Dentists in Leeds");
    assert_eq!(message["body"], "2 new companies match Dentists in Leeds. Added 2 to Dentists.");
    assert_eq!(message["url"], format!("https://splee.test/app/p/{project}/companies?tab=database&search={}&since=2026-11-21T09:00:00Z", anas.id));
    assert_eq!(message["tag"], format!("saved-search-{}", anas.id));

    announce(&state, &search(ben).await.unwrap(), &found).await;
    assert!(service.take().is_empty(), "Ben turned the daily digest off");
    announce(&state, &search(eve).await.unwrap(), &found).await;
    assert!(service.take().is_empty(), "Eve isn't in the project's organization");
    sqlx::query("update memberships set away = true where user_id = $1").bind(ana).execute(&db).await.unwrap();
    announce(&state, &anas, &found).await;
    assert!(service.take().is_empty(), "Ana is away");
}
