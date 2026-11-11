//! Connect Google and Connect Microsoft on the Mailboxes page. Start returns the provider's sign-in URL, and the provider sends
//! the browser back to the callback, which saves the mailbox and redirects to the Mailboxes page with how it went.
//! The tokens and XOAUTH2 logins are in mail::oauth.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_input;
use crate::auth::{Auth, MaybeAuth};
use crate::crypto::{self, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth::{self, Provider};
use crate::models::Ts;
use crate::state::AppState;

const STATE_MINUTES: i64 = 10;
const PAGE: &str = "/app/domains";

/// What a Google or Microsoft sign-in started here is for. Postmaster Tools signs in through the same app and callback,
/// so the operator registers one redirect URI.
pub(super) struct Purpose {
    /// Signed into the state, so a state made for one can't finish another.
    pub state: &'static str,
    /// As stored in mailbox_oauth_states.purpose.
    pub name: &'static str,
    /// What the user is connecting, in messages.
    pub what: &'static str,
}

pub(super) const MAILBOX: Purpose = Purpose { state: oauth::MAILBOX, name: "mailbox", what: "the mailbox" };
pub(super) const POSTMASTER: Purpose = Purpose { state: oauth::POSTMASTER, name: "postmaster", what: "Google Postmaster" };

fn start_again(purpose: &Purpose) -> String {
    format!("This sign-in expired or was already used. Connect {} again.", purpose.what)
}

pub fn router() -> Router<AppState> {
    Router::new().route("/mailboxes/oauth/callback", get(callback)).route("/mailboxes/oauth/{provider}", post(start))
}

#[derive(Deserialize)]
struct StartBody {
    /// The address of a mailbox being reconnected, so the provider offers that account first.
    email: Option<String>,
}

async fn start(State(state): State<AppState>, auth: Auth, Path(provider): Path<String>, Json(body): Json<StartBody>) -> AppResult<Json<Value>> {
    let provider = Provider::parse(&provider).ok_or(AppError::NotFound)?;
    let hint = body.email.as_deref().and_then(util_input::email);
    let url = start_sign_in(&state, &auth, provider, &MAILBOX, provider.scope(), hint.as_deref()).await?;
    Ok(Json(json!({ "url": url })))
}

/// Saves a pending sign-in and returns the provider's URL to send the browser to.
pub(super) async fn start_sign_in(
    state: &AppState,
    auth: &Auth,
    provider: Provider,
    purpose: &Purpose,
    scope: &str,
    login_hint: Option<&str>,
) -> AppResult<String> {
    auth.require_admin()?;
    if provider.app(&state.cfg).is_none() {
        return Err(AppError::BadRequest(format!("{} sign-in isn't set up on this Splee", provider.label())));
    }
    let (nonce, verifier) = (random_token(), random_token());
    sqlx::query("delete from mailbox_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query(
        "insert into mailbox_oauth_states (nonce_hash, user_id, org_id, provider, code_verifier_enc, expires_at, purpose)
         values ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(sha256_hex(&nonce))
    .bind(auth.user_id)
    .bind(auth.org_id)
    .bind(provider.name())
    .bind(crypto::encrypt(&state.cfg.app_secret, &verifier))
    .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
    .bind(purpose.name)
    .execute(&state.db)
    .await?;
    let signed = oauth::sign_state(&state.cfg.app_secret, purpose.state, &nonce, auth.user_id, auth.org_id);
    oauth::authorize_url(&state.cfg, provider, scope, &signed, &verifier, login_hint).map_err(AppError::Internal)
}

#[derive(Deserialize)]
pub(super) struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Always redirects: a Postmaster sign-in to the Deliverability page, and a mailbox's to the Mailboxes page, with
/// `connected`, the mailbox's id, which the page then tests like a mailbox added with a password, or with `mailbox_error`.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let nonce = q.state.as_deref().and_then(oauth::state_nonce).map(sha256_hex);
    let purpose: Option<String> =
        sqlx::query_scalar("select purpose from mailbox_oauth_states where nonce_hash = $1").bind(nonce).fetch_optional(&state.db).await.ok().flatten();
    if purpose.as_deref() == Some(POSTMASTER.name) {
        return super::deliverability::postmaster_callback(&state, auth, q).await;
    }
    let params = match finish(&state, auth, q).await {
        Ok(id) => vec![("connected", id.to_string())],
        Err(error) => vec![("mailbox_error", error)],
    };
    redirect(PAGE, &params)
}

pub(super) fn redirect(page: &str, params: &[(&str, String)]) -> Redirect {
    let url = Url::parse_with_params("http://splee/", params).expect("a fixed URL");
    Redirect::to(&format!("{page}?{}", url.query().unwrap_or_default()))
}

#[derive(sqlx::FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    provider: String,
    code_verifier_enc: String,
    expires_at: Ts,
}

pub(super) struct SignedIn {
    pub auth: Auth,
    pub provider: Provider,
    pub tokens: oauth::Tokens,
    pub account: oauth::Account,
}

/// Checks the callback's state against its pending sign-in and trades the code for tokens, or says what went wrong as
/// the page shows it.
pub(super) async fn signed_in(state: &AppState, auth: Option<Auth>, q: CallbackQuery, purpose: &Purpose) -> Result<SignedIn, String> {
    let what = purpose.what;
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "{what} sign-in failed");
        format!("Something went wrong saving {what}. Try again.")
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or_else(|| start_again(purpose))?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from mailbox_oauth_states where nonce_hash = $1 and purpose = $2 returning user_id, org_id, provider, code_verifier_enc, expires_at",
    )
    .bind(sha256_hex(nonce))
    .bind(purpose.name)
    .fetch_optional(&state.db)
    .await
    .map_err(|e| internal(&e))?
    .ok_or_else(|| start_again(purpose))?;
    if !oauth::verify_state(&state.cfg.app_secret, purpose.state, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(start_again(purpose));
    }
    let provider = Provider::parse(&pending.provider).ok_or_else(|| start_again(purpose))?;
    let label = provider.label();
    if let Some(error) = q.error {
        return Err(match error.as_str() {
            "access_denied" => format!("You didn't allow Splee into the {label} account, so nothing was connected."),
            _ => format!("{label} couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        });
    }
    let auth = auth.ok_or(format!("Sign in to Splee, then connect {what} again."))?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err(format!("You switched organizations while signing in. Connect {what} again from this one."));
    }
    auth.require_admin().map_err(|e| e.public_message())?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(|| start_again(purpose))?;

    let verifier = crypto::decrypt(&state.cfg.app_secret, &pending.code_verifier_enc).map_err(|e| internal(&e))?;
    let tokens = oauth::exchange_code(&state.cfg, provider, &code, &verifier).await.map_err(|e| e.to_string())?;
    let account = tokens.id_token.as_deref().and_then(oauth::account).ok_or(format!("{label} didn't share the account's email address."))?;
    Ok(SignedIn { auth, provider, tokens, account })
}

/// The saved mailbox, or what went wrong as the page shows it.
async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<Uuid, String> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "mailbox sign-in failed");
        "Something went wrong saving the mailbox. Try again.".to_string()
    };
    let SignedIn { auth, provider, tokens, account } = signed_in(state, auth, q, &MAILBOX).await?;
    let label = provider.label();
    let secret = &state.cfg.app_secret;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or(format!("{label} didn't let Splee stay signed in. Connect the mailbox again."))?;
    let email = util_input::email(&account.email).ok_or(format!("{label} signed in as {}, which isn't an email address.", account.email))?;

    let hosts = provider.hosts(account.consumer);
    // Reconnecting an address that's already here, with a password or a revoked sign-in, keeps its history and settings.
    sqlx::query_scalar(
        "insert into mailboxes (org_id, email, provider, smtp_host, smtp_port, smtp_security, smtp_username, smtp_password_enc, imap_host, imap_port,
           imap_username, imap_password_enc, oauth_refresh_token_enc, oauth_access_token_enc, oauth_expires_at, ramp_up, warmup, warmup_daily_cap,
           total_daily_limit, hold_until_warm, owner_id)
         values ($1, $2, $3, $4, $5, $6, $2, '', $7, $8, $2, '', $9, $10, $11, true, true, 40, 80, true, $12)
         on conflict (org_id, email) do update set
           provider = excluded.provider, smtp_host = excluded.smtp_host, smtp_port = excluded.smtp_port, smtp_security = excluded.smtp_security,
           smtp_username = excluded.smtp_username, smtp_password_enc = '', imap_username = excluded.imap_username, imap_password_enc = '',
           imap_host = excluded.imap_host, imap_port = excluded.imap_port,
           -- Another IMAP server starts over like a new mailbox, as in the PATCH route.
           imap_last_uid = case when mailboxes.imap_host <> excluded.imap_host then -1 else mailboxes.imap_last_uid end,
           imap_uid_validity = case when mailboxes.imap_host <> excluded.imap_host then null else mailboxes.imap_uid_validity end,
           oauth_refresh_token_enc = excluded.oauth_refresh_token_enc, oauth_access_token_enc = excluded.oauth_access_token_enc,
           oauth_expires_at = excluded.oauth_expires_at, needs_reconnect = false, recheck_at = null,
           owner_id = coalesce(mailboxes.owner_id, excluded.owner_id),
           status = case when mailboxes.status = 'error' then 'active' else mailboxes.status end,
           last_error = case when mailboxes.status = 'paused' then mailboxes.last_error else '' end
         returning id",
    )
    .bind(auth.org_id)
    .bind(&email)
    .bind(provider.name())
    .bind(hosts.smtp_host)
    .bind(hosts.smtp_port)
    .bind(hosts.smtp_security)
    .bind(hosts.imap_host)
    .bind(hosts.imap_port)
    .bind(crypto::encrypt(secret, refresh))
    .bind(crypto::encrypt(secret, &tokens.access_token))
    .bind(tokens.expires_at())
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| internal(&e))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::models::Mailbox;
    use crate::mail::oauth::tests::{FakeProvider, google_mailbox, state as fake_state};

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        fake: FakeProvider,
        acme: Uuid,
    }

    impl Server {
        /// Ana owns Acme and Beta, and has a session in each. Acme already has a password mailbox.
        async fn start(db: PgPool) -> Self {
            let fake = FakeProvider::start().await;
            let state = fake_state(db.clone(), &fake);
            let ana: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id")
                .fetch_one(&db)
                .await
                .unwrap();
            let mut orgs = vec![];
            for name in ["acme", "beta"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(ana).execute(&db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(&format!("session-{name}")))
                    .bind(ana)
                    .bind(org)
                    .execute(&db)
                    .await
                    .unwrap();
                orgs.push(org);
            }
            sqlx::query(
                "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, imap_host)
                 values ($1, 'old@acme.test', 'smtp.acme.test', 'old@acme.test', $2, '')",
            )
            .bind(orgs[0])
            .bind(crypto::encrypt(&state.cfg.app_secret, "app-password"))
            .execute(&db)
            .await
            .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router()).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
            Self { base, client, db, fake, acme: orgs[0] }
        }

        /// The authorize URL's query for a fresh sign-in started in `org`.
        async fn authorize(&self, org: &str) -> std::collections::HashMap<String, String> {
            let res = self
                .client
                .post(format!("{}/mailboxes/oauth/google", self.base))
                .header("cookie", format!("splee_session=session-{org}"))
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 200);
            let body: Value = res.json().await.unwrap();
            Url::parse(body["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect()
        }

        /// Where the callback sent the browser, as the Mailboxes page's query.
        async fn callback(&self, org: &str, state: &str) -> std::collections::HashMap<String, String> {
            let res = self
                .client
                .get(Url::parse_with_params(&format!("{}/mailboxes/oauth/callback", self.base), [("code", "the-code"), ("state", state)]).unwrap())
                .header("cookie", format!("splee_session=session-{org}"))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 303);
            let location = res.headers()["location"].to_str().unwrap().to_string();
            assert!(location.starts_with("/app/domains?"), "{location}");
            Url::parse(&format!("http://x{location}")).unwrap().query_pairs().into_owned().collect()
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signing_in_saves_a_google_mailbox_once_per_state(db: PgPool) {
        let s = Server::start(db).await;
        let q = s.authorize("acme").await;
        let back = s.callback("acme", &q["state"]).await;

        let form = s.fake.forms.lock().unwrap()[0].clone();
        assert_eq!((form["grant_type"].as_str(), form["code"].as_str()), ("authorization_code", "the-code"));
        assert_eq!(oauth::pkce_challenge(&form["code_verifier"]), q["code_challenge"], "the verifier matches the challenge");
        assert_eq!(form["redirect_uri"], q["redirect_uri"]);

        let mb = sqlx::query_as::<_, Mailbox>("select * from mailboxes where id = $1").bind(back["connected"].parse::<Uuid>().unwrap()).fetch_one(&s.db).await.unwrap();
        assert_eq!((mb.org_id, mb.email.as_str()), (s.acme, "ana@acme.test"));
        assert_eq!((mb.provider.as_str(), mb.smtp_host.as_str(), mb.smtp_port, mb.imap_host.as_str()), ("google", "smtp.gmail.com", 587, "imap.gmail.com"));
        let refresh: String = sqlx::query_scalar("select oauth_refresh_token_enc from mailboxes where id = $1").bind(mb.id).fetch_one(&s.db).await.unwrap();
        assert_eq!(crypto::decrypt("test-secret", &refresh).unwrap(), "refresh-for-the-code");
        assert!(mb.smtp_password_enc.is_empty() && !mb.needs_reconnect);

        // The same state again is refused, and nothing more is asked of the provider.
        assert_eq!(s.callback("acme", &q["state"]).await["mailbox_error"], start_again(&MAILBOX));
        assert_eq!(s.fake.hits(), 1);

        // The password mailbox is as it was.
        let old = sqlx::query_as::<_, Mailbox>("select * from mailboxes where email = 'old@acme.test'").fetch_one(&s.db).await.unwrap();
        assert_eq!((old.provider.as_str(), old.smtp_host.as_str()), ("password", "smtp.acme.test"));
        assert_eq!(crypto::decrypt("test-secret", &old.smtp_password_enc).unwrap(), "app-password");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_callback_refuses_a_bad_expired_or_foreign_state(db: PgPool) {
        let s = Server::start(db).await;
        let error = |q: std::collections::HashMap<String, String>| q.get("mailbox_error").cloned().unwrap_or_default();

        assert_eq!(error(s.callback("acme", "made-up.state").await), start_again(&MAILBOX));
        let q = s.authorize("acme").await;
        let (nonce, _) = q["state"].split_once('.').unwrap();
        assert_eq!(error(s.callback("acme", &format!("{nonce}.{}", "0".repeat(64))).await), start_again(&MAILBOX), "a forged signature");
        assert_eq!(error(s.callback("acme", &q["state"]).await), start_again(&MAILBOX), "the forged try used the state up");

        let q = s.authorize("acme").await;
        assert!(error(s.callback("beta", &q["state"]).await).starts_with("You switched organizations"));

        let q = s.authorize("acme").await;
        sqlx::query("update mailbox_oauth_states set expires_at = now() - interval '1 second'").execute(&s.db).await.unwrap();
        assert_eq!(error(s.callback("acme", &q["state"]).await), start_again(&MAILBOX));

        assert_eq!(s.fake.hits(), 0, "no code was traded");
        let oauth: i64 = sqlx::query_scalar("select count(*) from mailboxes where provider <> 'password'").fetch_one(&s.db).await.unwrap();
        assert_eq!(oauth, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_postmaster_sign_in_comes_back_to_deliverability_without_adding_a_mailbox(db: PgPool) {
        let s = Server::start(db).await;
        let res = s
            .client
            .post(format!("{}/deliverability/postmaster/oauth", s.base))
            .header("cookie", "splee_session=session-acme")
            .send()
            .await
            .unwrap();
        let url: Value = res.json().await.unwrap();
        let q: std::collections::HashMap<String, String> = Url::parse(url["url"].as_str().unwrap()).unwrap().query_pairs().into_owned().collect();
        assert_eq!(q["scope"], crate::deliverability::postmaster::SCOPE);

        let res = s
            .client
            .get(Url::parse_with_params(&format!("{}/mailboxes/oauth/callback", s.base), [("code", "the-code"), ("state", q["state"].as_str())]).unwrap())
            .header("cookie", "splee_session=session-acme")
            .send()
            .await
            .unwrap();
        assert_eq!(res.headers()["location"], "/app/deliverability?postmaster=connected");
        let (email, refresh): (String, String) =
            sqlx::query_as("select email, refresh_token_enc from postmaster_connections where org_id = $1").bind(s.acme).fetch_one(&s.db).await.unwrap();
        assert_eq!((email.as_str(), crypto::decrypt("test-secret", &refresh).unwrap().as_str()), ("ana@acme.test", "refresh-for-the-code"));
        let oauth: i64 = sqlx::query_scalar("select count(*) from mailboxes where provider <> 'password'").fetch_one(&s.db).await.unwrap();
        assert_eq!(oauth, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn reconnecting_clears_a_revoked_sign_in(db: PgPool) {
        let s = Server::start(db).await;
        let state = fake_state(s.db.clone(), &s.fake);
        let mb = google_mailbox(&state, "revoked").await;
        assert!(oauth::access_token(&state, &mb).await.is_err());
        sqlx::query("update mailboxes set org_id = $1 where id = $2").bind(s.acme).bind(mb.id).execute(&s.db).await.unwrap();

        let q = s.authorize("acme").await;
        assert_eq!(s.callback("acme", &q["state"]).await["connected"], mb.id.to_string(), "the same mailbox");
        let (reconnect, refresh): (bool, String) =
            sqlx::query_as("select needs_reconnect, oauth_refresh_token_enc from mailboxes where id = $1").bind(mb.id).fetch_one(&s.db).await.unwrap();
        assert!(!reconnect);
        assert_eq!(crypto::decrypt("test-secret", &refresh).unwrap(), "refresh-for-the-code");
    }
}
