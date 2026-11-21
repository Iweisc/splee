//! Browser push notifications, sent straight to each browser's push service with no third-party service in between. Each
//! message is encrypted to the browser's keys (RFC 8291, aes128gcm) and carries a VAPID token this server signs (RFC 8292).
//! Endpoints are capability URLs and messages carry leads' replies, so neither is ever logged.

use std::time::Duration;

use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use reqwest::Url;
use reqwest::header::{AUTHORIZATION, CONTENT_ENCODING, CONTENT_TYPE};
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use ring::{aead, agreement, hkdf};
use serde_json::json;
use uuid::Uuid;

use crate::models::Lead;
use crate::state::AppState;

/// What a notification shows. A newer one with the same `tag` replaces the older one instead of piling up.
#[derive(Clone, Debug)]
pub struct Push {
    pub title: String,
    pub body: String,
    pub url: String,
    pub tag: String,
}

impl Push {
    /// What the service worker reads. Short enough for a lock screen, and well under what a push service takes.
    fn json(&self) -> Vec<u8> {
        let message = json!({"title": cut(&self.title, 100), "body": cut(&self.body, 240), "url": self.url, "tag": self.tag});
        serde_json::to_vec(&message).expect("a JSON object serializes")
    }
}

fn cut(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    match text.char_indices().nth(max - 1) {
        Some((end, _)) if text.chars().nth(max).is_some() => format!("{}…", &text[..end]),
        _ => text,
    }
}

/// Which notification_prefs column lets a kind of push through.
#[derive(Clone, Copy)]
pub enum Pref {
    /// Hot replies and questions, with the hot lead email.
    HotLeads,
    /// Emails waiting for approval, with the daily digest.
    Digest,
    /// The member's own reminders, which they set themselves.
    Always,
}

impl Pref {
    fn sql(self) -> &'static str {
        match self {
            Pref::HotLeads => "coalesce(n.hot_lead_email, true)",
            Pref::Digest => "coalesce(n.daily_digest, true)",
            Pref::Always => "true",
        }
    }
}

// VAPID (RFC 8292)

const ALG: &ring::signature::EcdsaSigningAlgorithm = &ECDSA_P256_SHA256_FIXED_SIGNING;

struct Vapid {
    key: EcdsaKeyPair,
    /// The uncompressed public key, base64url, which browsers subscribe with.
    public: String,
}

impl Vapid {
    fn new(key: EcdsaKeyPair) -> Self {
        Self { public: URL_SAFE_NO_PAD.encode(key.public_key().as_ref()), key }
    }

    /// The Authorization header for a message to `endpoint`, with a token for its push service that's good for 12 hours.
    fn authorization(&self, endpoint: &Url, subject: &str) -> anyhow::Result<String> {
        let exp = chrono::Utc::now().timestamp() + 12 * 3600;
        Ok(format!("vapid t={}, k={}", jwt(&self.key, &endpoint.origin().ascii_serialization(), subject, exp)?, self.public))
    }
}

fn jwt(key: &EcdsaKeyPair, aud: &str, sub: &str, exp: i64) -> anyhow::Result<String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&json!({"aud": aud, "exp": exp, "sub": sub}))?);
    let input = format!("{header}.{claims}");
    let signature = key.sign(&SystemRandom::new(), input.as_bytes()).map_err(|_| anyhow!("couldn't sign the VAPID token"))?;
    Ok(format!("{input}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref())))
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// How a push service can reach whoever runs this server: VAPID_SUBJECT, a mailto: or https: URL, or else PUBLIC_URL.
fn subject(public_url: &str) -> String {
    env("VAPID_SUBJECT").unwrap_or_else(|| public_url.to_string())
}

/// Base64url as browsers and key generators write it, with or without padding.
fn b64(text: &str) -> anyhow::Result<Vec<u8>> {
    Ok(URL_SAFE_NO_PAD.decode(text.trim().trim_end_matches('='))?)
}

/// VAPID_PRIVATE_KEY and VAPID_PUBLIC_KEY when both are set, in the raw base64url form `web-push generate-vapid-keys` prints.
/// Otherwise the stored key pair, made the first time it's needed.
async fn vapid(state: &AppState) -> anyhow::Result<Vapid> {
    let rng = SystemRandom::new();
    if let (Some(private), Some(public)) = (env("VAPID_PRIVATE_KEY"), env("VAPID_PUBLIC_KEY")) {
        let key = EcdsaKeyPair::from_private_key_and_public_key(ALG, &b64(&private)?, &b64(&public)?, &rng)
            .map_err(|e| anyhow!("VAPID_PRIVATE_KEY and VAPID_PUBLIC_KEY aren't a P-256 key pair: {e}"))?;
        return Ok(Vapid::new(key));
    }
    let load = || sqlx::query_scalar::<_, String>("select private_key_enc from push_vapid_keys").fetch_optional(&state.db);
    let encrypted = match load().await? {
        Some(encrypted) => encrypted,
        None => {
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(ALG, &rng).map_err(|_| anyhow!("couldn't make a VAPID key"))?;
            // Two servers may both get here. The first key saved wins, and both load it.
            sqlx::query("insert into push_vapid_keys (private_key_enc) values ($1) on conflict do nothing")
                .bind(crate::crypto::encrypt(&state.cfg.app_secret, &STANDARD.encode(pkcs8.as_ref())))
                .execute(&state.db)
                .await?;
            load().await?.context("the VAPID key wasn't saved")?
        }
    };
    let pkcs8 = STANDARD.decode(crate::crypto::decrypt(&state.cfg.app_secret, &encrypted)?)?;
    Ok(Vapid::new(EcdsaKeyPair::from_pkcs8(ALG, &pkcs8, &rng).map_err(|e| anyhow!("the stored VAPID key is broken: {e}"))?))
}

/// The key browsers subscribe with, base64url.
pub async fn public_key(state: &AppState) -> anyhow::Result<String> {
    Ok(vapid(state).await?.public)
}

// Encryption (RFC 8291)

const RECORD_SIZE: u32 = 4096;
/// Push services take 4096 bytes of body: the 86-byte header, the message, its 1-byte delimiter and the 16-byte tag.
const MAX_PLAINTEXT: usize = 4096 - 86 - 1 - 16;

/// The aes128gcm body of a message to the browser with these keys, as one record from a new key pair and salt.
pub fn encrypt(ua_public: &[u8], auth: &[u8], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    let rng = SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).map_err(|_| anyhow!("couldn't make a key"))?;
    let as_public = private.compute_public_key().map_err(|_| anyhow!("couldn't make a key"))?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let ecdh = agreement::agree_ephemeral(private, &peer, |secret| secret.to_vec()).map_err(|_| anyhow!("the browser's key isn't a P-256 key"))?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| anyhow!("couldn't make a salt"))?;
    seal(&ecdh, as_public.as_ref(), ua_public, auth, &salt, plaintext)
}

/// The rest of `encrypt` once the ECDH secret is agreed, apart so the RFC's example can be checked byte for byte.
fn seal(ecdh: &[u8], as_public: &[u8], ua_public: &[u8], auth: &[u8], salt: &[u8; 16], plaintext: &[u8]) -> anyhow::Result<Vec<u8>> {
    anyhow::ensure!(auth.len() == 16, "the browser's auth secret isn't 16 bytes");
    anyhow::ensure!(plaintext.len() <= MAX_PLAINTEXT, "the message is too long to push");
    let key_info = [b"WebPush: info\0".as_slice(), ua_public, as_public].concat();
    let ikm = hkdf_sha256(auth, ecdh, &key_info, 32);
    let cek = hkdf_sha256(salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf_sha256(salt, &ikm, b"Content-Encoding: nonce\0", 12);
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).expect("16 bytes is an AES-128 key"));
    // 2 marks the last record, with no padding after it.
    let mut record = [plaintext, &[2]].concat();
    let nonce = aead::Nonce::try_assume_unique_for_key(&nonce).expect("the nonce is 12 bytes");
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record).map_err(|_| anyhow!("couldn't encrypt the message"))?;
    let mut body = salt.to_vec();
    body.extend(RECORD_SIZE.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend(as_public);
    body.extend(record);
    Ok(body)
}

/// HKDF-SHA256: extract a key from `ikm` with `salt`, then expand `info` to `len` bytes.
fn hkdf_sha256(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let mut out = vec![0; len];
    let info = [info];
    prk.expand(&info, Len(len)).and_then(|okm| okm.fill(&mut out)).expect("HKDF-SHA256 expands to up to 8160 bytes");
    out
}

/// Why a subscription a browser sent can't be saved, or None when it can.
pub fn invalid(endpoint: &str, p256dh: &str, auth: &str) -> Option<&'static str> {
    let Ok(url) = Url::parse(endpoint) else { return Some("The push endpoint isn't a URL") };
    // The tests' fake push service runs on plain http on this machine. hooks::public_client still turns away anything else there.
    let fake = cfg!(test) && url.host_str() == Some("127.0.0.1");
    if !(url.scheme() == "https" || fake) || endpoint.len() > 2048 {
        return Some("The push endpoint must be an https URL");
    }
    let point = b64(p256dh).ok().filter(|k| k.len() == 65 && k[0] == 4);
    let usable = point.is_some_and(|k| {
        let Ok(probe) = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &SystemRandom::new()) else { return false };
        agreement::agree_ephemeral(probe, &agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, &k), |_| ()).is_ok()
    });
    if !usable {
        return Some("The browser's push key isn't a P-256 public key");
    }
    if !b64(auth).is_ok_and(|a| a.len() == 16) {
        return Some("The browser's push auth secret isn't 16 bytes");
    }
    None
}

// Delivery

#[derive(sqlx::FromRow)]
struct Subscription {
    id: Uuid,
    endpoint: String,
    p256dh: String,
    auth: String,
}

/// What became of one message.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sent {
    Delivered,
    /// The push service said the subscription is gone, so it was deleted.
    Gone,
    Failed,
}

/// How long a push service keeps a message for a browser that's offline. A hot lead from yesterday still matters.
const TTL_SECS: &str = "86400";
const RETRY_AFTER: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(3) };

/// Pushes to every browser of each of `users`, all at once. Each subscription's row records how it went.
async fn send_to(state: &AppState, users: &[Uuid], push: &Push) -> anyhow::Result<Vec<Sent>> {
    let subscriptions: Vec<Subscription> =
        sqlx::query_as("select id, endpoint, p256dh, auth from push_subscriptions where user_id = any($1) order by created_at")
            .bind(users)
            .fetch_all(&state.db)
            .await?;
    if subscriptions.is_empty() {
        return Ok(Vec::new());
    }
    let vapid = vapid(state).await?;
    let subject = subject(&state.cfg.public_url);
    let message = push.json();
    Ok(futures::future::join_all(subscriptions.iter().map(|s| deliver(state, &vapid, &subject, s, &message))).await)
}

async fn deliver(state: &AppState, vapid: &Vapid, subject: &str, sub: &Subscription, message: &[u8]) -> Sent {
    let sent = match post(vapid, subject, sub, message).await {
        Ok(status) if status.is_success() => Sent::Delivered,
        Ok(status) if matches!(status.as_u16(), 404 | 410) => Sent::Gone,
        Ok(status) => {
            tracing::warn!(subscription = %sub.id, %status, "push service refused the message");
            Sent::Failed
        }
        Err(e) => {
            tracing::warn!(subscription = %sub.id, error = %e, "push failed");
            Sent::Failed
        }
    };
    let sql = match sent {
        Sent::Delivered => "update push_subscriptions set last_success_at = now(), failures = 0 where id = $1",
        Sent::Gone => "delete from push_subscriptions where id = $1",
        Sent::Failed => "update push_subscriptions set failures = failures + 1 where id = $1",
    };
    if let Err(e) = sqlx::query(sql).bind(sub.id).execute(&state.db).await {
        tracing::warn!(subscription = %sub.id, error = %e, "couldn't save how the push went");
    }
    sent
}

/// Posts the message, and once more after a moment when the push service is down, busy or unreachable.
async fn post(vapid: &Vapid, subject: &str, sub: &Subscription, message: &[u8]) -> anyhow::Result<reqwest::StatusCode> {
    let url = Url::parse(&sub.endpoint)?;
    let body = encrypt(&b64(&sub.p256dh)?, &b64(&sub.auth)?, message)?;
    let authorization = vapid.authorization(&url, subject)?;
    // The endpoint came from a browser, so it gets the same checks as a webhook URL someone typed.
    let client = crate::hooks::public_client(&url).await?;
    let mut last = Err(anyhow!("not sent"));
    for attempt in 0..2 {
        if attempt > 0 {
            tokio::time::sleep(RETRY_AFTER).await;
        }
        let request = client
            .post(url.clone())
            .header("TTL", TTL_SECS)
            .header("Urgency", "high")
            .header(CONTENT_ENCODING, "aes128gcm")
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(AUTHORIZATION, &authorization)
            .body(body.clone());
        last = match request.send().await {
            Ok(res) if res.status().is_server_error() || res.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => Ok(res.status()),
            Ok(res) => return Ok(res.status()),
            Err(e) if e.is_timeout() || e.is_connect() => Err(e.without_url().into()),
            // Without the URL, which is the subscription's secret.
            Err(e) => return Err(e.without_url().into()),
        };
    }
    last
}

// Who gets what

/// Who hears about a lead: its owner alone while they're on the team and not away, or else everyone on the team who isn't
/// away, the way round robin skips them. Only those with push on whose `pref` allows it.
async fn lead_recipients(db: &sqlx::PgPool, org_id: Uuid, owner: Option<Uuid>, pref: Pref) -> sqlx::Result<Vec<Uuid>> {
    sqlx::query_scalar(&format!(
        "select m.user_id from memberships m left join notification_prefs n on n.user_id = m.user_id
         where m.org_id = $1 and not m.away and {}
           and (m.user_id = $2 or not exists (select 1 from memberships o where o.org_id = $1 and o.user_id = $2 and not o.away))
           and exists (select 1 from push_subscriptions s where s.user_id = m.user_id)
         order by m.user_id",
        pref.sql()
    ))
    .bind(org_id)
    .bind(owner)
    .fetch_all(db)
    .await
}

/// Pushes news about a lead to whoever should hear it (see lead_recipients). Failures are logged, never returned.
pub async fn lead_alert(state: &AppState, lead: &Lead, pref: Pref, push: &Push) {
    let result: anyhow::Result<()> = async {
        let org_id: Option<Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;
        let Some(org_id) = org_id else { return Ok(()) };
        let users = lead_recipients(&state.db, org_id, lead.owner_id, pref).await?;
        send_to(state, &users, push).await.map(drop)
    }
    .await;
    if let Err(e) = result {
        tracing::warn!(lead_id = %lead.id, error = %e, "couldn't push the lead alert");
    }
}

/// lead_alert in the background, so a run or a worker doesn't wait on push services.
pub fn spawn_lead_alert(state: &AppState, lead: &Lead, pref: Pref, push: Push) {
    let (state, lead) = (state.clone(), lead.clone());
    tokio::spawn(async move { lead_alert(&state, &lead, pref, &push).await });
}

fn who(lead: &Lead) -> String {
    if lead.company_name.is_empty() { lead.full_name.clone() } else { format!("{} ({})", lead.full_name, lead.company_name) }
}

/// A hot reply or a question from a lead. Both open the thread and share a tag, so the newest replaces the older.
pub fn reply(public_url: &str, lead: &Lead, project_name: &str, reply: &str, hot: bool) -> Push {
    let label = if hot { "Hot lead" } else { "Question" };
    Push {
        title: format!("{label} for {project_name}"),
        body: format!("{}: {}", who(lead), reply.trim()),
        url: crate::hooks::lead_url(public_url, lead.project_id, lead.id),
        tag: format!("lead-{}", lead.id),
    }
}

/// A reminder on a lead that just came due.
pub fn reminder(public_url: &str, lead: &Lead, project_name: &str) -> Push {
    let notes = lead.notes.trim();
    Push {
        title: format!("Follow up with {}", lead.full_name),
        body: if notes.is_empty() { format!("{} for {project_name}", who(lead)) } else { format!("{}: {notes}", who(lead)) },
        url: crate::hooks::lead_url(public_url, lead.project_id, lead.id),
        tag: format!("reminder-{}", lead.id),
    }
}

/// Drafts waiting for someone to approve them. Keep in sync with slack::WAITING.
const WAITING: &str = "e.direction = 'outbound' and e.kind in ('initial', 'follow_up') and e.status = 'draft'";

/// Marks the approval push as sent for each member with push on, who isn't away and gets the daily digest, in an organization
/// with drafts that came in since their last one, at most hourly. Returns them. Claiming first keeps two runs from both sending it.
async fn claim_approval_digests(db: &sqlx::PgPool) -> sqlx::Result<Vec<(Uuid, Uuid)>> {
    sqlx::query_as(&format!(
        "insert into push_approval_digests (user_id, org_id, sent_at)
         select m.user_id, m.org_id, now() from memberships m left join notification_prefs n on n.user_id = m.user_id
         where not m.away and {}
           and exists (select 1 from push_subscriptions s where s.user_id = m.user_id)
           and exists (select 1 from email_messages e join projects p on p.id = e.project_id where p.org_id = m.org_id and {WAITING}
                         and e.created_at > coalesce((select d.sent_at from push_approval_digests d
                                                      where d.user_id = m.user_id and d.org_id = m.org_id), '-infinity'))
         on conflict (user_id, org_id) do update set sent_at = now() where push_approval_digests.sent_at <= now() - interval '1 hour'
         returning user_id, org_id",
        Pref::Digest.sql()
    ))
    .fetch_all(db)
    .await
}

/// Every 15 minutes: tells members that emails wait for their approval. See claim_approval_digests.
pub async fn approval_digests(state: &AppState) -> anyhow::Result<()> {
    for (user_id, org_id) in claim_approval_digests(&state.db).await? {
        let waiting: i64 =
            sqlx::query_scalar(&format!("select count(*) from email_messages e join projects p on p.id = e.project_id where p.org_id = $1 and {WAITING}"))
                .bind(org_id)
                .fetch_one(&state.db)
                .await?;
        let push = Push {
            title: "Emails to approve".into(),
            body: if waiting == 1 { "1 email is waiting for your approval.".into() } else { format!("{waiting} emails are waiting for your approval.") },
            url: format!("{}/app/emails", state.cfg.public_url),
            tag: format!("approvals-{org_id}"),
        };
        if let Err(e) = send_to(state, &[user_id], &push).await {
            tracing::warn!(error = %e, "couldn't push the approval digest");
        }
    }
    Ok(())
}

/// A test message to each of the member's browsers.
pub async fn send_test(state: &AppState, user_id: Uuid) -> anyhow::Result<Vec<Sent>> {
    let push = Push {
        title: "Splee notifications are on".into(),
        body: "Hot replies, questions, emails to approve and your reminders show up here.".into(),
        url: format!("{}/app/notifications", state.cfg.public_url),
        tag: "test".into(),
    };
    send_to(state, &[user_id], &push).await
}

/// "Chrome on macOS", from a browser's User-Agent, for the list of a member's devices.
pub fn device_label(user_agent: &str) -> String {
    let has = |s: &str| user_agent.contains(s);
    let browser = if has("Edg/") || has("EdgA/") || has("EdgiOS/") {
        "Edge"
    } else if has("OPR/") {
        "Opera"
    } else if has("SamsungBrowser/") {
        "Samsung Internet"
    } else if has("Firefox/") || has("FxiOS/") {
        "Firefox"
    } else if has("Chrome/") || has("CriOS/") {
        "Chrome"
    } else if has("Safari/") {
        "Safari"
    } else {
        "A browser"
    };
    let os = if has("iPhone") {
        "iPhone"
    } else if has("iPad") {
        "iPad"
    } else if has("Android") {
        "Android"
    } else if has("Windows") {
        "Windows"
    } else if has("CrOS") {
        "ChromeOS"
    } else if has("Macintosh") {
        "macOS"
    } else if has("Linux") {
        "Linux"
    } else {
        return browser.to_string();
    };
    format!("{browser} on {os}")
}

#[cfg(test)]
mod tests;
