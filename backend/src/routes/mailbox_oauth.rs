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
const START_AGAIN: &str = "This sign-in expired or was already used. Connect the mailbox again.";

pub fn router() -> Router<AppState> {
    Router::new().route("/mailboxes/oauth/callback", get(callback)).route("/mailboxes/oauth/{provider}", post(start))
}

#[derive(Deserialize)]
struct StartBody {
    /// The address of a mailbox being reconnected, so the provider offers that account first.
    email: Option<String>,
}

async fn start(State(state): State<AppState>, auth: Auth, Path(provider): Path<String>, Json(body): Json<StartBody>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let provider = Provider::parse(&provider).ok_or(AppError::NotFound)?;
    if provider.app(&state.cfg).is_none() {
        return Err(AppError::BadRequest(format!("{} sign-in isn't set up on this Splee", provider.label())));
    }
    let (nonce, verifier) = (random_token(), random_token());
    sqlx::query("delete from mailbox_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query(
        "insert into mailbox_oauth_states (nonce_hash, user_id, org_id, provider, code_verifier_enc, expires_at) values ($1, $2, $3, $4, $5, $6)",
    )
    .bind(sha256_hex(&nonce))
    .bind(auth.user_id)
    .bind(auth.org_id)
    .bind(provider.name())
    .bind(crypto::encrypt(&state.cfg.app_secret, &verifier))
    .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
    .execute(&state.db)
    .await?;
    let signed = oauth::sign_state(&state.cfg.app_secret, oauth::MAILBOX, &nonce, auth.user_id, auth.org_id);
    let hint = body.email.as_deref().and_then(util_input::email);
    let url = oauth::authorize_url(&state.cfg, provider, &signed, &verifier, hint.as_deref()).map_err(AppError::Internal)?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

/// Always redirects to the Mailboxes page: with `connected`, the mailbox's id, which the page then tests like a mailbox added
/// with a password, or with `mailbox_error`.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let params = match finish(&state, auth, q).await {
        Ok(id) => vec![("connected", id.to_string())],
        Err(error) => vec![("mailbox_error", error)],
    };
    let url = Url::parse_with_params(&format!("http://splee{PAGE}"), &params).expect("a fixed URL");
    Redirect::to(&format!("{PAGE}?{}", url.query().unwrap_or_default()))
}

#[derive(sqlx::FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    provider: String,
    code_verifier_enc: String,
    expires_at: Ts,
}

/// The saved mailbox, or what went wrong as the page shows it.
async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<Uuid, String> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "mailbox sign-in failed");
        "Something went wrong saving the mailbox. Try again.".to_string()
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or(START_AGAIN)?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from mailbox_oauth_states where nonce_hash = $1 returning user_id, org_id, provider, code_verifier_enc, expires_at",
    )
    .bind(sha256_hex(nonce))
    .fetch_optional(&state.db)
    .await
    .map_err(|e| internal(&e))?
    .ok_or(START_AGAIN)?;
    if !oauth::verify_state(&state.cfg.app_secret, oauth::MAILBOX, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(START_AGAIN.into());
    }
    let provider = Provider::parse(&pending.provider).ok_or(START_AGAIN)?;
    let label = provider.label();
    if let Some(error) = q.error {
        return Err(match error.as_str() {
            "access_denied" => format!("You didn't allow Splee into the {label} account, so nothing was connected."),
            _ => format!("{label} couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        });
    }
    let auth = auth.ok_or("Sign in to Splee, then connect the mailbox again.")?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err("You switched organizations while signing in. Connect the mailbox again from this one.".into());
    }
    auth.require_admin().map_err(|e| e.public_message())?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or(START_AGAIN)?;

    let secret = &state.cfg.app_secret;
    let verifier = crypto::decrypt(secret, &pending.code_verifier_enc).map_err(|e| internal(&e))?;
    let tokens = oauth::exchange_code(&state.cfg, provider, &code, &verifier).await.map_err(|e| e.to_string())?;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or(format!("{label} didn't let Splee stay signed in. Connect the mailbox again."))?;
    let account = tokens.id_token.as_deref().and_then(oauth::account).ok_or(format!("{label} didn't share the account's email address."))?;
    let email = util_input::email(&account.email).ok_or(format!("{label} signed in as {}, which isn't an email address.", account.email))?;

    let hosts = provider.hosts(account.consumer);
    // Reconnecting an address that's already here, with a password or a revoked sign-in, keeps its history and settings.
    sqlx::query_scalar(
        "insert into mailboxes (org_id, email, provider, smtp_host, smtp_port, smtp_security, smtp_username, smtp_password_enc, imap_host, imap_port,
           imap_username, imap_password_enc, oauth_refresh_token_enc, oauth_access_token_enc, oauth_expires_at, ramp_up, warmup, warmup_daily_cap,
           total_daily_limit, hold_until_warm)
         values ($1, $2, $3, $4, $5, $6, $2, '', $7, $8, $2, '', $9, $10, $11, true, true, 40, 80, true)
         on conflict (org_id, email) do update set
           provider = excluded.provider, smtp_host = excluded.smtp_host, smtp_port = excluded.smtp_port, smtp_security = excluded.smtp_security,
           smtp_username = excluded.smtp_username, smtp_password_enc = '', imap_username = excluded.imap_username, imap_password_enc = '',
           imap_host = excluded.imap_host, imap_port = excluded.imap_port,
           -- Another IMAP server starts over like a new mailbox, as in the PATCH route.
           imap_last_uid = case when mailboxes.imap_host <> excluded.imap_host then -1 else mailboxes.imap_last_uid end,
           imap_uid_validity = case when mailboxes.imap_host <> excluded.imap_host then null else mailboxes.imap_uid_validity end,
           oauth_refresh_token_enc = excluded.oauth_refresh_token_enc, oauth_access_token_enc = excluded.oauth_access_token_enc,
           oauth_expires_at = excluded.oauth_expires_at, needs_reconnect = false, recheck_at = null,
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
        assert_eq!(s.callback("acme", &q["state"]).await["mailbox_error"], START_AGAIN);
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

        assert_eq!(error(s.callback("acme", "made-up.state").await), START_AGAIN);
        let q = s.authorize("acme").await;
        let (nonce, _) = q["state"].split_once('.').unwrap();
        assert_eq!(error(s.callback("acme", &format!("{nonce}.{}", "0".repeat(64))).await), START_AGAIN, "a forged signature");
        assert_eq!(error(s.callback("acme", &q["state"]).await), START_AGAIN, "the forged try used the state up");

        let q = s.authorize("acme").await;
        assert!(error(s.callback("beta", &q["state"]).await).starts_with("You switched organizations"));

        let q = s.authorize("acme").await;
        sqlx::query("update mailbox_oauth_states set expires_at = now() - interval '1 second'").execute(&s.db).await.unwrap();
        assert_eq!(error(s.callback("acme", &q["state"]).await), START_AGAIN);

        assert_eq!(s.fake.hits(), 0, "no code was traded");
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
