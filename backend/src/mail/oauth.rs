//! Mailboxes connected by signing in to Google or Microsoft instead of with an app password. The Mailboxes page sends the
//! browser to the provider with the authorization code flow and S256 PKCE (routes/mailbox_oauth.rs), and the refresh token
//! it gets back is stored encrypted like a password. SMTP and IMAP then log in with XOAUTH2 and an access token, renewed
//! here when the stored one has under a minute left.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{Context, anyhow};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::Utc;
use reqwest::Url;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::config::{Config, OAuthApp};
use crate::crypto;
use crate::models::{Mailbox, Ts};
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Provider {
    Google,
    Microsoft,
}

pub const PROVIDERS: [Provider; 2] = [Provider::Google, Provider::Microsoft];

/// Microsoft's tenant for personal accounts (outlook.com, hotmail.com), which send through another SMTP host.
const CONSUMER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

/// Where a mailbox's server settings come from. Microsoft's personal accounts send through smtp-mail.outlook.com.
pub struct Hosts {
    pub smtp_host: &'static str,
    pub smtp_port: i32,
    pub smtp_security: &'static str,
    pub imap_host: &'static str,
    pub imap_port: i32,
}

impl Provider {
    pub fn parse(name: &str) -> Option<Self> {
        PROVIDERS.into_iter().find(|p| p.name() == name)
    }

    /// As stored in mailboxes.provider.
    pub fn name(self) -> &'static str {
        match self {
            Provider::Google => "google",
            Provider::Microsoft => "microsoft",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Google => "Google",
            Provider::Microsoft => "Microsoft",
        }
    }

    pub fn app(self, cfg: &Config) -> Option<&OAuthApp> {
        match self {
            Provider::Google => cfg.google.as_ref(),
            Provider::Microsoft => cfg.microsoft.as_ref(),
        }
    }

    /// openid and email put the address in the ID token. Microsoft only hands out a refresh token for offline_access.
    pub fn scope(self) -> &'static str {
        match self {
            Provider::Google => "https://mail.google.com/ openid email",
            Provider::Microsoft => "offline_access https://outlook.office.com/SMTP.Send https://outlook.office.com/IMAP.AccessAsUser.All openid email",
        }
    }

    pub fn hosts(self, consumer: bool) -> Hosts {
        let (smtp_host, imap_host) = match self {
            Provider::Google => ("smtp.gmail.com", "imap.gmail.com"),
            Provider::Microsoft if consumer => ("smtp-mail.outlook.com", "outlook.office365.com"),
            Provider::Microsoft => ("smtp.office365.com", "outlook.office365.com"),
        };
        Hosts { smtp_host, smtp_port: 587, smtp_security: "starttls", imap_host, imap_port: 993 }
    }
}

/// The providers this Splee has an app for, which the Mailboxes page shows a button for.
pub fn configured(cfg: &Config) -> Vec<&'static str> {
    PROVIDERS.into_iter().filter(|p| p.app(cfg).is_some()).map(Provider::name).collect()
}

pub fn redirect_uri(cfg: &Config) -> String {
    format!("{}/api/mailboxes/oauth/callback", cfg.public_url.trim_end_matches('/'))
}

pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Google only returns a refresh token with access_type=offline, and again for an account that already allowed the app
/// only with prompt=consent.
pub fn authorize_url(cfg: &Config, provider: Provider, state: &str, verifier: &str, login_hint: Option<&str>) -> anyhow::Result<String> {
    let app = provider.app(cfg).context("not configured")?;
    let mut url = Url::parse(&app.authorize_url)?;
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri(cfg))
        .append_pair("scope", provider.scope())
        .append_pair("state", state)
        .append_pair("code_challenge", &pkce_challenge(verifier))
        .append_pair("code_challenge_method", "S256");
    match provider {
        Provider::Google => url.query_pairs_mut().append_pair("access_type", "offline").append_pair("prompt", "consent"),
        Provider::Microsoft => url.query_pairs_mut().append_pair("prompt", "select_account"),
    };
    if let Some(hint) = login_hint {
        url.query_pairs_mut().append_pair("login_hint", hint);
    }
    Ok(url.into())
}

/// What a sign-in is for, so a state made for one can't finish another.
pub const MAILBOX: &str = "mailbox-oauth";
pub const TUMBLR: &str = "tumblr-oauth";

pub use crypto::{sign_state, state_nonce, verify_state};

/// The SASL XOAUTH2 initial response, before base64. lettre builds the same one for SMTP itself.
pub fn xoauth2(user: &str, access_token: &str) -> String {
    format!("user={user}\x01auth=Bearer {access_token}\x01\x01")
}

/// Why a token request failed. Only Unavailable is worth trying again on its own.
#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("{0} signed Splee out of this mailbox. Reconnect it to send and read replies again.")]
    Revoked(&'static str),
    #[error("Couldn't reach {0} to renew the sign-in: {1}")]
    Unavailable(&'static str, String),
    #[error("{0} refused to renew the sign-in: {1}")]
    Refused(&'static str, String),
}

/// A token request that may work in a few minutes: the provider was down, slow or busy.
pub fn retry_later(e: &anyhow::Error) -> bool {
    matches!(e.downcast_ref::<TokenError>(), Some(TokenError::Unavailable(..)))
}

#[derive(Deserialize)]
pub struct Tokens {
    pub access_token: String,
    pub expires_in: Option<i64>,
    pub refresh_token: Option<String>,
    pub id_token: Option<String>,
}

impl Tokens {
    pub fn expires_at(&self) -> Ts {
        Utc::now() + chrono::Duration::seconds(self.expires_in.unwrap_or(3600))
    }
}

/// `label` names the provider in errors.
pub(crate) async fn token_request(label: &'static str, app: &OAuthApp, form: &[(&str, &str)]) -> Result<Tokens, TokenError> {
    let unavailable = |e: String| TokenError::Unavailable(label, e);
    let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| unavailable(e.to_string()))?;
    let mut form = form.to_vec();
    form.extend([("client_id", app.client_id.as_str()), ("client_secret", app.client_secret.as_str())]);
    let res = client.post(&app.token_url).form(&form).send().await.map_err(|e| unavailable(e.without_url().to_string()))?;
    let status = res.status();
    if status.is_success() {
        return res.json::<Tokens>().await.map_err(|e| TokenError::Refused(label, format!("unreadable answer: {e}")));
    }
    if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(unavailable(status.to_string()));
    }
    let body: Value = res.json().await.unwrap_or_default();
    match body["error"].as_str() {
        Some("invalid_grant") => Err(TokenError::Revoked(label)),
        error => {
            let said = body["error_description"].as_str().or(error).unwrap_or(status.as_str());
            // Microsoft follows its reason with trace and correlation ids on new lines.
            Err(TokenError::Refused(label, said.lines().next().unwrap_or(said).trim().to_string()))
        }
    }
}

pub async fn exchange_code(cfg: &Config, provider: Provider, code: &str, verifier: &str) -> Result<Tokens, TokenError> {
    let app = provider.app(cfg).ok_or(TokenError::Refused(provider.label(), "sign-in isn't set up on this Splee".into()))?;
    let redirect = redirect_uri(cfg);
    let form = [("grant_type", "authorization_code"), ("code", code), ("redirect_uri", &redirect), ("code_verifier", verifier)];
    token_request(provider.label(), app, &form).await
}

/// The signed-in account, from the ID token. It came straight from the token endpoint over TLS, so its signature isn't
/// checked (OpenID Connect Core 3.1.3.7).
pub struct Account {
    pub email: String,
    /// A Microsoft personal account.
    pub consumer: bool,
}

pub fn account(id_token: &str) -> Option<Account> {
    let payload = URL_SAFE_NO_PAD.decode(id_token.split('.').nth(1)?.trim_end_matches('=')).ok()?;
    let claims: Value = serde_json::from_slice(&payload).ok()?;
    // Work accounts without a mailbox address in Entra ID only have their sign-in name, which is the address for most.
    let email = claims["email"].as_str().or(claims["preferred_username"].as_str())?;
    Some(Account { email: email.trim().to_lowercase(), consumer: claims["tid"].as_str() == Some(CONSUMER_TENANT) })
}

/// One lock per mailbox, so the sender, the warmup and the reply poller asking at once renew its token once.
static LOCKS: LazyLock<Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

fn lock_for(mailbox_id: Uuid) -> Arc<tokio::sync::Mutex<()>> {
    LOCKS.lock().unwrap_or_else(|e| e.into_inner()).entry(mailbox_id).or_default().clone()
}

/// An access token for a mailbox connected by signing in: the stored one while it has a minute left, else a new one.
/// The row is read again under the lock, because another task may have just renewed it or someone reconnected it.
pub async fn access_token(state: &AppState, mb: &Mailbox) -> anyhow::Result<String> {
    let provider = Provider::parse(&mb.provider).ok_or_else(|| anyhow!("{} doesn't sign in with OAuth", mb.email))?;
    let lock = lock_for(mb.id);
    let _guard = lock.lock().await;
    // The in-process lock keeps tasks here from each taking a connection. This one covers another backend on the database.
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('mailbox-token:' || $1::text, 0))").bind(mb.id).execute(&mut *tx).await?;
    let (refresh_enc, access_enc, expires_at, needs_reconnect): (String, String, Option<Ts>, bool) =
        sqlx::query_as("select oauth_refresh_token_enc, oauth_access_token_enc, oauth_expires_at, needs_reconnect from mailboxes where id = $1")
            .bind(mb.id)
            .fetch_one(&mut *tx)
            .await?;
    // Nothing asks the provider again for a token it refused, so a revoked mailbox can't be retried forever.
    if needs_reconnect {
        return Err(TokenError::Revoked(provider.label()).into());
    }
    let secret = &state.cfg.app_secret;
    if !access_enc.is_empty() && expires_at.is_some_and(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        return crypto::decrypt(secret, &access_enc);
    }
    let app = provider.app(&state.cfg).ok_or_else(|| {
        anyhow!("{} sign-in isn't set up on this Splee anymore. Set its client ID and secret on the server.", provider.label())
    })?;
    let refresh = crypto::decrypt(secret, &refresh_enc)?;
    let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", refresh.as_str())];
    // Microsoft hands out a token for the scopes asked for here, and the first one asked for without them.
    if provider == Provider::Microsoft {
        form.push(("scope", provider.scope()));
    }
    match token_request(provider.label(), app, &form).await {
        Ok(tokens) => {
            // Microsoft sends a new refresh token with each access token. The old one keeps working for a while.
            sqlx::query(
                "update mailboxes set oauth_access_token_enc = $3, oauth_expires_at = $4, oauth_refresh_token_enc = coalesce($5, oauth_refresh_token_enc)
                 where id = $1 and oauth_refresh_token_enc = $2",
            )
            .bind(mb.id)
            .bind(&refresh_enc)
            .bind(crypto::encrypt(secret, &tokens.access_token))
            .bind(tokens.expires_at())
            .bind(tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).map(|t| crypto::encrypt(secret, t)))
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(tokens.access_token)
        }
        Err(e @ TokenError::Revoked(_)) => {
            mark_revoked(&mut tx, mb.id, &refresh_enc, &e.to_string()).await?;
            tx.commit().await?;
            Err(e.into())
        }
        Err(e) => Err(e.into()),
    }
}

/// Stops the mailbox until someone signs in again. A paused one stays paused and keeps why. Matching on the refresh token
/// leaves alone a mailbox reconnected while the request was out.
async fn mark_revoked(db: &mut sqlx::PgConnection, mailbox_id: Uuid, refresh_enc: &str, error: &str) -> sqlx::Result<()> {
    tracing::warn!(mailbox = %mailbox_id, "the provider revoked a mailbox's sign-in");
    sqlx::query(
        "update mailboxes set needs_reconnect = true, oauth_access_token_enc = '', oauth_expires_at = null, recheck_at = null,
           status = case when status = 'paused' then status else 'error' end,
           last_error = case when status = 'paused' then last_error else $3 end
         where id = $1 and oauth_refresh_token_enc = $2",
    )
    .bind(mailbox_id)
    .bind(refresh_enc)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use axum::extract::State;
    use axum::routing::post;
    use axum::{Form, Json, Router};
    use sqlx::PgPool;

    use super::*;

    /// A token endpoint that counts its requests. The refresh token "revoked" gets invalid_grant, "down" a 503, and any
    /// other one a new access token numbered by the request. A code gets tokens for `account`.
    pub(crate) struct FakeProvider {
        pub url: String,
        pub hits: Arc<AtomicUsize>,
        pub forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
    }

    pub(crate) fn id_token(claims: Value) -> String {
        format!("e30.{}.sig", URL_SAFE_NO_PAD.encode(claims.to_string()))
    }

    impl FakeProvider {
        pub(crate) async fn start() -> Self {
            #[derive(Clone)]
            struct Fake {
                hits: Arc<AtomicUsize>,
                forms: Arc<Mutex<Vec<HashMap<String, String>>>>,
            }
            async fn token(State(f): State<Fake>, Form(form): Form<HashMap<String, String>>) -> (axum::http::StatusCode, Json<Value>) {
                let n = f.hits.fetch_add(1, Ordering::SeqCst) + 1;
                f.forms.lock().unwrap().push(form.clone());
                // Long enough for concurrent callers to pile up on the lock.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let ok = |body: Value| (axum::http::StatusCode::OK, Json(body));
                match (form["grant_type"].as_str(), form.get("refresh_token").map(String::as_str)) {
                    ("refresh_token", Some("revoked")) => (axum::http::StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "invalid_grant" }))),
                    ("refresh_token", Some("down")) => (axum::http::StatusCode::SERVICE_UNAVAILABLE, Json(Value::Null)),
                    ("refresh_token", _) => ok(serde_json::json!({ "access_token": format!("access-{n}"), "expires_in": 3599 })),
                    _ => ok(serde_json::json!({
                        "access_token": "access-from-code", "expires_in": 3599, "refresh_token": format!("refresh-for-{}", form["code"]),
                        "id_token": id_token(serde_json::json!({ "email": "Ana@Acme.test" })),
                    })),
                }
            }
            let fake = Fake { hits: Arc::default(), forms: Arc::default() };
            let (hits, forms) = (fake.hits.clone(), fake.forms.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route("/token", post(token)).with_state(fake);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { url, hits, forms }
        }

        pub(crate) fn app(&self) -> OAuthApp {
            OAuthApp {
                client_id: "client-123".into(),
                client_secret: "shh".into(),
                authorize_url: format!("{}/authorize", self.url),
                token_url: format!("{}/token", self.url),
            }
        }

        pub(crate) fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
    }

    pub(crate) fn config(fake: &FakeProvider) -> Config {
        Config {
            database_url: String::new(),
            app_secret: "test-secret".into(),
            public_url: "http://localhost:3404".into(),
            unsubscribe_url: String::new(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            operator_emails: Vec::new(),
            google: Some(fake.app()),
            microsoft: Some(fake.app()),
            slack: None,
            tumblr: None,
            ipinfo_token: None,
        }
    }

    pub(crate) fn state(db: PgPool, fake: &FakeProvider) -> AppState {
        AppState { db, cfg: Arc::new(config(fake)), events: tokio::sync::broadcast::channel(1).0 }
    }

    /// A Google mailbox with this refresh token and no access token yet.
    pub(crate) async fn google_mailbox(state: &AppState, refresh: &str) -> Mailbox {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&state.db).await.unwrap();
        sqlx::query_as::<_, Mailbox>(
            "insert into mailboxes (org_id, email, provider, smtp_host, smtp_username, smtp_password_enc, imap_host, imap_username, oauth_refresh_token_enc)
             values ($1, 'ana@acme.test', 'google', 'smtp.gmail.com', 'ana@acme.test', '', 'imap.gmail.com', 'ana@acme.test', $2) returning *",
        )
        .bind(org)
        .bind(crypto::encrypt(&state.cfg.app_secret, refresh))
        .fetch_one(&state.db)
        .await
        .unwrap()
    }

    #[test]
    fn the_authorize_url_asks_for_mail_access_with_pkce() {
        let fake = FakeProvider { url: "https://auth.test".into(), hits: Arc::default(), forms: Arc::default() };
        let cfg = config(&fake);
        let verifier = "a-verifier-of-at-least-forty-three-characters-0123456789";
        let url = Url::parse(&authorize_url(&cfg, Provider::Google, "nonce.sig", verifier, Some("ana@acme.test")).unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(url.path(), "/authorize");
        assert_eq!(q["client_id"], "client-123");
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["redirect_uri"], "http://localhost:3404/api/mailboxes/oauth/callback");
        assert_eq!(q["scope"], "https://mail.google.com/ openid email");
        assert_eq!(q["state"], "nonce.sig");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], URL_SAFE_NO_PAD.encode(Sha256::digest(verifier)));
        assert_eq!((q["access_type"].as_str(), q["prompt"].as_str()), ("offline", "consent"));
        assert_eq!(q["login_hint"], "ana@acme.test");

        let url = Url::parse(&authorize_url(&cfg, Provider::Microsoft, "s", verifier, None).unwrap()).unwrap();
        let q: HashMap<String, String> = url.query_pairs().into_owned().collect();
        assert!(q["scope"].starts_with("offline_access https://outlook.office.com/SMTP.Send https://outlook.office.com/IMAP.AccessAsUser.All"));
        assert!(!q.contains_key("access_type") && !q.contains_key("login_hint"));
        assert!(authorize_url(&Config { google: None, ..cfg }, Provider::Google, "s", verifier, None).is_err());
    }

    #[test]
    fn the_state_is_bound_to_the_user_and_org() {
        let (user, org) = (Uuid::new_v4(), Uuid::new_v4());
        let state = sign_state("secret", MAILBOX, "abc", user, org);
        assert_eq!(state_nonce(&state), Some("abc"));
        assert!(verify_state("secret", MAILBOX, &state, user, org));
        assert!(!verify_state("secret", MAILBOX, &state, user, Uuid::new_v4()));
        assert!(!verify_state("secret", MAILBOX, &state, Uuid::new_v4(), org));
        assert!(!verify_state("other secret", MAILBOX, &state, user, org));
        assert!(!verify_state("secret", MAILBOX, &state.replace("abc", "abd"), user, org));
        assert!(!verify_state("secret", MAILBOX, "abc", user, org));
        assert!(!verify_state("secret", TUMBLR, &state, user, org), "a mailbox state can't finish a Tumblr sign-in");
    }

    #[test]
    fn xoauth2_strings_follow_the_sasl_format() {
        assert_eq!(xoauth2("ana@acme.test", "ya29.token"), "user=ana@acme.test\x01auth=Bearer ya29.token\x01\x01");
        // Microsoft's example from "Authenticate an IMAP, POP or SMTP connection using OAuth".
        let microsoft = xoauth2("test@contoso.onmicrosoft.com", "EwBAAl3BAAUFFpUAo7J3Ve0bjLBWZWCclRC3EoAA");
        assert_eq!(
            base64::engine::general_purpose::STANDARD.encode(microsoft),
            "dXNlcj10ZXN0QGNvbnRvc28ub25taWNyb3NvZnQuY29tAWF1dGg9QmVhcmVyIEV3QkFBbDNCQUFVRkZwVUFvN0ozVmUwYmpMQldaV0NjbFJDM0VvQUEBAQ=="
        );
    }

    #[test]
    fn the_address_comes_from_the_id_token() {
        let found = |claims: Value| account(&id_token(claims)).map(|a| (a.email, a.consumer));
        assert_eq!(found(serde_json::json!({ "email": " Ana@Acme.test " })), Some(("ana@acme.test".into(), false)));
        let personal = serde_json::json!({ "preferred_username": "ana@outlook.com", "tid": CONSUMER_TENANT });
        assert_eq!(found(personal), Some(("ana@outlook.com".into(), true)));
        assert_eq!(found(serde_json::json!({ "sub": "1" })).map(|a| a.0), None);
        assert!(account("not-a-jwt").is_none());
        assert_eq!(Provider::Microsoft.hosts(true).smtp_host, "smtp-mail.outlook.com");
        assert_eq!(Provider::Microsoft.hosts(false).smtp_host, "smtp.office365.com");
    }

    /// Fake SMTP and IMAP servers on two ports that let anyone in and keep every line they were sent.
    async fn fake_mail_servers() -> (u16, u16, Arc<Mutex<Vec<String>>>) {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let seen: Arc<Mutex<Vec<String>>> = Arc::default();
        let smtp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let imap = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ports = (smtp.local_addr().unwrap().port(), imap.local_addr().unwrap().port());
        let log = seen.clone();
        tokio::spawn(async move {
            let (socket, _) = smtp.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            write.write_all(b"220 fake\r\n").await.unwrap();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log.lock().unwrap().push(format!("SMTP {line}"));
                let reply = match line.split(' ').next().unwrap_or_default() {
                    "EHLO" => "250-fake\r\n250 AUTH PLAIN LOGIN XOAUTH2",
                    "AUTH" => "235 2.7.0 Accepted",
                    "QUIT" => "221 bye",
                    _ => "250 ok",
                };
                write.write_all(format!("{reply}\r\n").as_bytes()).await.unwrap();
            }
        });
        let log = seen.clone();
        tokio::spawn(async move {
            let (socket, _) = imap.accept().await.unwrap();
            let (read, mut write) = socket.into_split();
            write.write_all(b"* OK fake\r\n").await.unwrap();
            let mut lines = BufReader::new(read).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log.lock().unwrap().push(format!("IMAP {line}"));
                let (tag, command) = line.split_once(' ').unwrap_or((&line, ""));
                let reply = match command.split(' ').next().unwrap_or_default() {
                    "AUTHENTICATE" => {
                        write.write_all(b"+ \r\n").await.unwrap();
                        let response = lines.next_line().await.unwrap().unwrap();
                        log.lock().unwrap().push(format!("IMAP {response}"));
                        format!("{tag} OK signed in")
                    }
                    "SELECT" => format!("* 0 EXISTS\r\n* OK [UIDVALIDITY 1] ok\r\n{tag} OK [READ-WRITE] done"),
                    "LOGOUT" => format!("* BYE\r\n{tag} OK bye"),
                    _ => format!("{tag} OK done"),
                };
                write.write_all(format!("{reply}\r\n").as_bytes()).await.unwrap();
            }
        });
        (ports.0, ports.1, seen)
    }

    async fn point_at(state: &AppState, mb: &Mailbox, smtp: u16, imap: u16) -> Mailbox {
        sqlx::query_as::<_, Mailbox>("update mailboxes set smtp_host = '127.0.0.1', smtp_port = $2, smtp_security = 'none', imap_host = '127.0.0.1', imap_port = $3 where id = $1 returning *")
            .bind(mb.id)
            .bind(i32::from(smtp))
            .bind(i32::from(imap))
            .fetch_one(&state.db)
            .await
            .unwrap()
    }

    fn decoded(line: &str) -> String {
        let b64 = line.rsplit(' ').next().unwrap();
        String::from_utf8(base64::engine::general_purpose::STANDARD.decode(b64).unwrap()).unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn smtp_and_imap_log_in_with_xoauth2(db: PgPool) {
        let fake = FakeProvider::start().await;
        let state = state(db, &fake);
        let mb = google_mailbox(&state, "good").await;
        let (smtp, imap, seen) = fake_mail_servers().await;
        let mb = point_at(&state, &mb, smtp, imap).await;

        super::super::test_mailbox(&state, &mb).await.unwrap();
        let seen = seen.lock().unwrap().clone();
        let auth = seen.iter().find(|l| l.starts_with("SMTP AUTH")).unwrap();
        assert!(auth.starts_with("SMTP AUTH XOAUTH2 "), "{auth}");
        assert_eq!(decoded(auth), "user=ana@acme.test\x01auth=Bearer access-1\x01\x01");
        let at = seen.iter().position(|l| l.ends_with("AUTHENTICATE XOAUTH2")).unwrap();
        assert_eq!(decoded(&seen[at + 1]), "user=ana@acme.test\x01auth=Bearer access-1\x01\x01");
        assert_eq!(fake.hits(), 1, "SMTP and IMAP share the token");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn password_mailboxes_still_log_in_with_their_password(db: PgPool) {
        let fake = FakeProvider::start().await;
        let state = state(db, &fake);
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&state.db).await.unwrap();
        let mb = sqlx::query_as::<_, Mailbox>(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, imap_host) values ($1, 'old@acme.test', 'x', 'old@acme.test', $2, 'x')
             returning *",
        )
        .bind(org)
        .bind(crypto::encrypt(&state.cfg.app_secret, "app-password"))
        .fetch_one(&state.db)
        .await
        .unwrap();
        assert_eq!(mb.provider, "password");
        let (smtp, imap, seen) = fake_mail_servers().await;
        let mb = point_at(&state, &mb, smtp, imap).await;

        super::super::test_mailbox(&state, &mb).await.unwrap();
        let seen = seen.lock().unwrap().clone();
        let auth = seen.iter().find(|l| l.starts_with("SMTP AUTH")).unwrap();
        assert!(auth.starts_with("SMTP AUTH PLAIN "), "{auth}");
        assert_eq!(decoded(auth), "\0old@acme.test\0app-password");
        assert!(seen.iter().any(|l| l.ends_with(r#"LOGIN "old@acme.test" "app-password""#)), "{seen:?}");
        assert_eq!(fake.hits(), 0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn access_tokens_are_renewed_once_and_reused_until_they_run_out(db: PgPool) {
        let fake = FakeProvider::start().await;
        let state = state(db, &fake);
        let mb = google_mailbox(&state, "good").await;

        // Five connections at once trade the refresh token once.
        let tokens = futures::future::join_all((0..5).map(|_| access_token(&state, &mb))).await;
        assert!(tokens.iter().all(|t| t.as_deref().unwrap() == "access-1"), "{:?}", tokens.iter().map(|t| t.as_ref().ok()).collect::<Vec<_>>());
        assert_eq!(fake.hits(), 1);
        let form = fake.forms.lock().unwrap()[0].clone();
        assert_eq!((form["grant_type"].as_str(), form["refresh_token"].as_str(), form["client_id"].as_str()), ("refresh_token", "good", "client-123"));
        assert_eq!(access_token(&state, &mb).await.unwrap(), "access-1");
        assert_eq!(fake.hits(), 1, "the stored token is reused");

        // Under a minute left counts as run out.
        sqlx::query("update mailboxes set oauth_expires_at = now() + interval '30 seconds' where id = $1").bind(mb.id).execute(&state.db).await.unwrap();
        assert_eq!(access_token(&state, &mb).await.unwrap(), "access-2");
        assert_eq!(fake.hits(), 2);
        let stored: String = sqlx::query_scalar("select oauth_access_token_enc from mailboxes where id = $1").bind(mb.id).fetch_one(&state.db).await.unwrap();
        assert_eq!(crypto::decrypt(&state.cfg.app_secret, &stored).unwrap(), "access-2");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_revoked_sign_in_stops_the_mailbox_and_is_not_tried_again(db: PgPool) {
        let fake = FakeProvider::start().await;
        let state = state(db, &fake);
        let mb = google_mailbox(&state, "revoked").await;

        let err = access_token(&state, &mb).await.unwrap_err();
        assert!(matches!(err.downcast_ref::<TokenError>(), Some(TokenError::Revoked("Google"))), "{err}");
        assert!(!retry_later(&err));
        let (status, error, reconnect, recheck): (String, String, bool, Option<Ts>) =
            sqlx::query_as("select status, last_error, needs_reconnect, recheck_at from mailboxes where id = $1").bind(mb.id).fetch_one(&state.db).await.unwrap();
        assert_eq!((status.as_str(), reconnect, recheck), ("error", true, None));
        assert!(error.starts_with("Google signed Splee out of this mailbox"), "{error}");

        assert!(access_token(&state, &mb).await.is_err());
        assert_eq!(fake.hits(), 1, "a revoked token isn't sent again");

        // A provider that's down is worth trying later, and doesn't mark the mailbox.
        let down = google_mailbox(&state, "down").await;
        assert!(retry_later(&access_token(&state, &down).await.unwrap_err()));
        let reconnect: bool = sqlx::query_scalar("select needs_reconnect from mailboxes where id = $1").bind(down.id).fetch_one(&state.db).await.unwrap();
        assert!(!reconnect);
    }
}
