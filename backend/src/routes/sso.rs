//! Sign in with Google and Sign in with Microsoft, through the same apps that connect mailboxes. Start sends the browser to
//! the provider with OpenID Connect's code flow, S256 PKCE and a nonce, and keeps what the callback needs in an encrypted
//! cookie. The cookie ties the sign-in to the browser that started it, so nobody can finish their own sign-in in someone
//! else's browser and have them work in the wrong account.
//!
//! The callback signs in the user linked to the provider's account, else links it to the user with the same address when the
//! provider vouches for that address, else makes a new account the way POST /auth/signup does.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::routing::{delete, get};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::{Cookie, SameSite};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgConnection;
use uuid::Uuid;

use super::auth::{ClaimBody, create_account, may_sign_up, start_session};
use super::util_input;
use crate::audit;
use crate::auth::{Auth, MaybeAuth, NO_PASSWORD};
use crate::config::Config;
use crate::crypto::{self, random_token};
use crate::error::{AppError, AppResult};
use crate::mail::oauth::{self, CONSUMER_TENANT, Provider, TokenError};
use crate::models::Identity;
use crate::state::AppState;
use crate::two_factor;

const COOKIE: &str = "splee_sso";
/// Only the start and the callback need the cookie.
const COOKIE_PATH: &str = "/api/auth/sso";
const MINUTES: i64 = 10;
const SCOPE: &str = "openid email profile";
const START_AGAIN: &str = "This sign-in expired or was started in another tab. Try again.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/sso", get(providers))
        .route("/auth/sso/callback", get(callback))
        .route("/auth/sso/{provider}/start", get(start))
        .route("/auth/sign-in-methods", get(methods))
        .route("/auth/identities/{id}", delete(disconnect))
}

/// Registered with both apps next to the mailbox callback.
fn redirect_uri(cfg: &Config) -> String {
    format!("{}/api/auth/sso/callback", cfg.public_url.trim_end_matches('/'))
}

/// For the sign-in and sign-up pages, which only show a button for each.
async fn providers(State(state): State<AppState>) -> Json<Value> {
    Json(json!({ "providers": oauth::configured(&state.cfg) }))
}

/// The page a sign-in started on, which its errors go back to.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Page {
    Signin,
    Signup,
    /// Connecting another sign-in method on the Account page.
    Account,
}

impl Page {
    fn path(self) -> &'static str {
        match self {
            Page::Signin => "/signin",
            Page::Signup => "/signup",
            Page::Account => "/app/account",
        }
    }
}

/// What the callback needs, kept encrypted in the cookie.
#[derive(Serialize, Deserialize)]
struct Pending {
    provider: String,
    state: String,
    nonce: String,
    verifier: String,
    expires_at: i64,
    page: Page,
    next: Option<String>,
    invite: Option<String>,
    claim: Option<ClaimBody>,
    /// The signed-in user connecting another sign-in method, when page is Account.
    user_id: Option<Uuid>,
}

#[derive(Deserialize)]
struct StartQuery {
    /// signin, signup or account.
    from: Option<String>,
    next: Option<String>,
    invite: Option<String>,
    /// A demo project to keep, as on the sign-up page.
    claim: Option<Uuid>,
    token: Option<String>,
}

/// The same rule as the sign-in page's safeNext, and only printable ASCII, since it ends up in a Location header. A backslash
/// is refused too, because browsers read /\host as //host.
fn safe_next(next: &str) -> bool {
    next.starts_with('/') && !next.starts_with("//") && !next.contains('\\') && next.chars().all(|c| c.is_ascii_graphic())
}

/// Invite and claim tokens are made by random_token. Anything else can't be one, and could break a URL it's put in.
fn token_like(token: &str) -> bool {
    !token.is_empty() && token.len() <= 128 && token.chars().all(|c| c.is_ascii_alphanumeric())
}

async fn start(
    State(state): State<AppState>,
    MaybeAuth(auth): MaybeAuth,
    jar: CookieJar,
    Path(provider): Path<String>,
    Query(q): Query<StartQuery>,
) -> AppResult<(CookieJar, Redirect)> {
    let provider = Provider::parse(&provider).filter(|p| p.app(&state.cfg).is_some()).ok_or(AppError::NotFound)?;
    let page = match q.from.as_deref() {
        Some("signup") => Page::Signup,
        Some("account") => Page::Account,
        _ => Page::Signin,
    };
    let user_id = match (page, auth) {
        (Page::Account, Some(auth)) => Some(auth.user_id),
        (Page::Account, None) => return Ok((jar, Redirect::to(&page_url("/signin", &[("next", "/app/account")])))),
        _ => None,
    };
    let claim = match (q.claim, q.token.filter(|t| token_like(t))) {
        (Some(project_id), Some(token)) => Some(ClaimBody { project_id, token }),
        _ => None,
    };
    let pending = Pending {
        provider: provider.name().into(),
        state: random_token(),
        nonce: random_token(),
        verifier: random_token(),
        expires_at: (Utc::now() + Duration::minutes(MINUTES)).timestamp(),
        page,
        next: q.next.filter(|n| safe_next(n)),
        invite: q.invite.filter(|t| token_like(t)),
        claim,
        user_id,
    };
    let url = authorize_url(&state.cfg, provider, &pending).map_err(AppError::Internal)?;
    let sealed = crypto::encrypt(&state.cfg.app_secret, &serde_json::to_string(&pending).map_err(anyhow::Error::from)?);
    let cookie = Cookie::build((COOKIE, sealed))
        .path(COOKIE_PATH)
        .http_only(true)
        .same_site(SameSite::Lax)
        .secure(state.cfg.secure_cookies())
        .max_age(time::Duration::minutes(MINUTES))
        .build();
    Ok((jar.add(cookie), Redirect::to(&url)))
}

fn authorize_url(cfg: &Config, provider: Provider, pending: &Pending) -> anyhow::Result<String> {
    let app = provider.app(cfg).ok_or_else(|| anyhow::anyhow!("{} sign-in isn't set up", provider.label()))?;
    let mut url = Url::parse(&app.authorize_url)?;
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("response_type", "code")
        .append_pair("redirect_uri", &redirect_uri(cfg))
        .append_pair("scope", SCOPE)
        .append_pair("state", &pending.state)
        .append_pair("nonce", &pending.nonce)
        .append_pair("code_challenge", &oauth::pkce_challenge(&pending.verifier))
        .append_pair("code_challenge_method", "S256")
        // Someone signed in to several accounts picks one, instead of getting whichever the browser is on.
        .append_pair("prompt", "select_account");
    Ok(url.into())
}

/// A path on the frontend with a query.
fn page_url(path: &str, params: &[(&str, &str)]) -> String {
    let params: Vec<_> = params.iter().filter(|(_, v)| !v.is_empty()).collect();
    if params.is_empty() {
        return path.to_string();
    }
    let url = Url::parse_with_params("http://splee/", params).expect("a fixed URL");
    format!("{path}?{}", url.query().unwrap_or_default())
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

enum Outcome {
    /// Signed in, with the session cookie and where to go.
    SignedIn(Box<Cookie<'static>>, String),
    /// Where to go: the code step of a sign-in with two-factor on, or the Account page after connecting.
    Go(String),
}

/// Always redirects, and always clears the cookie, so a sign-in can't be finished twice. Errors go back to the page the
/// sign-in started on as `sso_error`, keeping its invite, demo claim and next.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, jar: CookieJar, Query(q): Query<CallbackQuery>) -> (CookieJar, Redirect) {
    let pending = jar
        .get(COOKIE)
        .and_then(|c| crypto::decrypt(&state.cfg.app_secret, c.value()).ok())
        .and_then(|json| serde_json::from_str::<Pending>(&json).ok());
    let jar = jar.add(Cookie::build((COOKIE, "")).path(COOKIE_PATH).secure(state.cfg.secure_cookies()).max_age(time::Duration::ZERO).build());
    let (page, kept) = match &pending {
        Some(p) => {
            let claim = p.claim.as_ref().map(|c| (c.project_id.to_string(), c.token.clone())).unwrap_or_default();
            (p.page, [p.next.clone().unwrap_or_default(), p.invite.clone().unwrap_or_default(), claim.0, claim.1])
        }
        None => (Page::Signin, Default::default()),
    };
    match finish(&state, auth, q, pending).await {
        Ok(Outcome::SignedIn(cookie, to)) => (jar.add(*cookie), Redirect::to(&to)),
        Ok(Outcome::Go(to)) => (jar, Redirect::to(&to)),
        Err(error) => {
            let [next, invite, claim, token] = &kept;
            let params = match page {
                Page::Account => vec![("sso_error", error.as_str())],
                _ => vec![
                    ("sso_error", error.as_str()),
                    ("next", next.as_str()),
                    ("invite", invite.as_str()),
                    ("claim", claim.as_str()),
                    ("token", token.as_str()),
                ],
            };
            (jar, Redirect::to(&page_url(page.path(), &params)))
        }
    }
}

/// What a failed database call shows. The error is logged, and never holds a token.
fn internal(e: impl Into<AppError>) -> String {
    match e.into() {
        AppError::Internal(e) => {
            tracing::error!(error = ?e, "single sign-on failed");
            "Something went wrong signing you in. Try again.".into()
        }
        other => other.public_message(),
    }
}

async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery, pending: Option<Pending>) -> Result<Outcome, String> {
    let p = pending.ok_or(START_AGAIN)?;
    if q.state.as_deref() != Some(p.state.as_str()) || p.expires_at <= Utc::now().timestamp() {
        return Err(START_AGAIN.into());
    }
    let provider = Provider::parse(&p.provider).ok_or(START_AGAIN)?;
    let label = provider.label();
    if let Some(error) = q.error {
        return Err(match error.as_str() {
            "access_denied" => format!("You didn't let Splee use your {label} account, so you weren't signed in."),
            _ => format!("{label} couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        });
    }
    let code = q.code.filter(|c| !c.is_empty()).ok_or(START_AGAIN)?;
    let app = provider.app(&state.cfg).ok_or(format!("{label} sign-in isn't set up on this Splee anymore."))?;
    let redirect = redirect_uri(&state.cfg);
    let form = [("grant_type", "authorization_code"), ("code", code.as_str()), ("redirect_uri", redirect.as_str()), ("code_verifier", p.verifier.as_str())];
    let tokens = oauth::token_request(label, app, &form).await.map_err(|e| {
        tracing::warn!(provider = provider.name(), error = %e, "single sign-on code was refused");
        match e {
            TokenError::Unavailable(..) => format!("Couldn't reach {label}. Try again in a minute."),
            _ => format!("{label} turned the sign-in down. Try again."),
        }
    })?;
    let account = tokens.id_token.as_deref().ok_or("no ID token").and_then(|t| verify_id_token(provider, &app.client_id, t, &p.nonce, Utc::now().timestamp()));
    let account = account.map_err(|why| {
        tracing::warn!(provider = provider.name(), why, "refused a single sign-on ID token");
        format!("Splee couldn't check the {label} sign-in. Try again.")
    })?;
    match p.user_id {
        Some(user_id) => connect(state, auth, user_id, provider, &account).await,
        None => sign_in(state, provider, &account, &p).await,
    }
}

/// The signed-in account, from the ID token.
#[derive(Debug)]
struct ProviderAccount {
    subject: String,
    email: Option<String>,
    /// The provider vouches that the address is the account holder's.
    verified: bool,
    name: String,
}

/// The ID token came straight from the token endpoint over TLS, so like mail::oauth::account its signature isn't checked
/// (OpenID Connect Core 3.1.3.7). Its claims are, so a token made for another app or another sign-in isn't taken.
fn verify_id_token(provider: Provider, client_id: &str, id_token: &str, nonce: &str, now: i64) -> Result<ProviderAccount, &'static str> {
    let payload = id_token.split('.').nth(1).ok_or("not a JWT")?;
    let payload = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).map_err(|_| "not a JWT")?;
    let claims: Value = serde_json::from_slice(&payload).map_err(|_| "not a JWT")?;
    let (iss, tid) = (claims["iss"].as_str().unwrap_or_default(), claims["tid"].as_str().unwrap_or_default());
    let issuer = match provider {
        Provider::Google => iss == "https://accounts.google.com" || iss == "accounts.google.com",
        // One issuer per tenant, personal accounts included, since the app signs in on /common.
        Provider::Microsoft => !tid.is_empty() && iss == format!("https://login.microsoftonline.com/{tid}/v2.0"),
    };
    if !issuer {
        return Err("issuer");
    }
    let audience = match &claims["aud"] {
        Value::String(aud) => aud == client_id,
        Value::Array(auds) => auds.iter().any(|a| a.as_str() == Some(client_id)),
        _ => false,
    };
    if !audience {
        return Err("audience");
    }
    // A minute of leeway for the clocks.
    if claims["exp"].as_i64().is_none_or(|exp| exp + 60 < now) {
        return Err("expired");
    }
    if claims["nonce"].as_str() != Some(nonce) {
        return Err("nonce");
    }
    let subject = claims["sub"].as_str().filter(|s| !s.is_empty()).ok_or("no subject")?;
    let said = |claim: &str| claims[claim] == true || claims[claim] == "true";
    let (email, verified) = match provider {
        Provider::Google => (claims["email"].as_str(), said("email_verified")),
        // A work account's email claim is whatever its tenant's admin typed, so Microsoft only vouches for it with the
        // optional xms_edov claim. A personal account's address is the account itself.
        Provider::Microsoft => (claims["email"].as_str().or(claims["preferred_username"].as_str()), tid == CONSUMER_TENANT || said("xms_edov")),
    };
    let email = email.and_then(util_input::email);
    let name = claims["name"].as_str().unwrap_or_default().trim().to_string();
    Ok(ProviderAccount { subject: subject.to_string(), verified: verified && email.is_some(), email, name })
}

/// Links the provider's account to the user. A user has at most one account per provider.
async fn link(conn: &mut PgConnection, user_id: Uuid, provider: Provider, account: &ProviderAccount) -> AppResult<()> {
    let saved = sqlx::query("insert into user_identities (user_id, provider, subject, email) values ($1, $2, $3, $4)")
        .bind(user_id)
        .bind(provider.name())
        .bind(&account.subject)
        .bind(account.email.as_deref().unwrap_or_default())
        .execute(conn)
        .await;
    match saved.map_err(AppError::from) {
        Ok(_) => Ok(()),
        Err(AppError::Conflict(_)) => Err(AppError::Conflict(format!(
            "Your Splee account already signs in with another {label} account. Use that one, or disconnect it on your Account page first.",
            label = provider.label()
        ))),
        Err(e) => Err(e),
    }
}

async fn sign_in(state: &AppState, provider: Provider, account: &ProviderAccount, p: &Pending) -> Result<Outcome, String> {
    let (db, label) = (&state.db, provider.label());
    let linked: Option<(Uuid, String)> =
        sqlx::query_as("select u.id, u.email::text from user_identities i join users u on u.id = i.user_id where i.provider = $1 and i.subject = $2")
            .bind(provider.name())
            .bind(&account.subject)
            .fetch_optional(db)
            .await
            .map_err(internal)?;
    let (user_id, email) = match linked {
        Some(found) => found,
        None => {
            let email = account.email.clone().ok_or(format!("{label} didn't share your email address, so Splee can't sign you in with it."))?;
            let existing: Option<Uuid> =
                sqlx::query_scalar("select id from users where email = $1::citext").bind(&email).fetch_optional(db).await.map_err(internal)?;
            match existing {
                // An account made from an address nobody proved could hold someone else's email, and their later verified sign-in
                // would link to it. So a new account needs a confirmed address too.
                None if !account.verified => {
                    return Err(format!(
                        "{label} didn't confirm that {email} is yours, so Splee can't make an account with it. Sign up with email and a password instead."
                    ));
                }
                None => return create(state, provider, account, email, p).await,
                Some(user_id) if !account.verified => {
                    audit::record_account(db, user_id, "user.sign_in_failed", json!({ "reason": provider.name() })).await;
                    return Err(format!(
                        "{label} didn't confirm that {email} is yours. Sign in with your password, then connect {label} on your Account page."
                    ));
                }
                Some(user_id) => {
                    link(&mut *db.acquire().await.map_err(internal)?, user_id, provider, account).await.map_err(internal)?;
                    audit::record_account(db, user_id, "user.identity_linked", json!({ "provider": provider.name() })).await;
                    (user_id, email)
                }
            }
        }
    };
    let next = p.next.clone().or_else(|| p.invite.as_ref().map(|t| format!("/invite/{t}"))).unwrap_or_else(|| "/app".into());
    // The provider stands in for the password, so two-factor still asks for its code on the sign-in page.
    if two_factor::enabled(db, user_id).await.map_err(internal)? {
        let token = two_factor::start_login(db, user_id).await.map_err(internal)?;
        return Ok(Outcome::Go(format!("{}#two_factor_token={token}", page_url("/signin", &[("next", &next)]))));
    }
    let (cookie, _) = start_session(state, user_id, &email, json!({ "method": provider.name() })).await.map_err(internal)?;
    Ok(Outcome::SignedIn(Box::new(cookie), next))
}

/// A new account, made like POST /auth/signup with no password until they set one.
async fn create(state: &AppState, provider: Provider, account: &ProviderAccount, email: String, p: &Pending) -> Result<Outcome, String> {
    may_sign_up(state, p.invite.is_some()).await.map_err(internal)?;
    let mut tx = state.db.begin().await.map_err(internal)?;
    let new = create_account(&mut tx, &email, &account.name, NO_PASSWORD, p.invite.as_deref(), p.claim.as_ref()).await.map_err(internal)?;
    link(&mut tx, new.user_id, provider, account).await.map_err(internal)?;
    tx.commit().await.map_err(internal)?;
    new.joined(state).await;
    let (cookie, _) = start_session(state, new.user_id, &email, json!({ "method": provider.name() })).await.map_err(internal)?;
    let next = p.next.clone().or_else(|| p.claim.as_ref().map(|c| format!("/app/p/{}", c.project_id))).unwrap_or_else(|| "/app".into());
    Ok(Outcome::SignedIn(Box::new(cookie), next))
}

/// Connecting from the Account page, which needs the same user still signed in. The provider's address doesn't matter here.
async fn connect(state: &AppState, auth: Option<Auth>, user_id: Uuid, provider: Provider, account: &ProviderAccount) -> Result<Outcome, String> {
    let label = provider.label();
    auth.filter(|a| a.user_id == user_id).ok_or(format!("Sign in to Splee again, then connect {label}."))?;
    let owner: Option<Uuid> = sqlx::query_scalar("select user_id from user_identities where provider = $1 and subject = $2")
        .bind(provider.name())
        .bind(&account.subject)
        .fetch_optional(&state.db)
        .await
        .map_err(internal)?;
    match owner {
        Some(owner) if owner != user_id => return Err(format!("That {label} account already signs in to another Splee account.")),
        Some(_) => {}
        None => {
            link(&mut *state.db.acquire().await.map_err(internal)?, user_id, provider, account).await.map_err(internal)?;
            audit::record_account(&state.db, user_id, "user.identity_linked", json!({ "provider": provider.name() })).await;
        }
    }
    Ok(Outcome::Go(page_url(Page::Account.path(), &[("connected", provider.name())])))
}

#[derive(Serialize)]
struct Methods {
    /// False for an account made with Google or Microsoft that hasn't set a password.
    password: bool,
    identities: Vec<Identity>,
    /// The providers the server has an app for, which the Account page offers to connect.
    providers: Vec<&'static str>,
}

async fn sign_in_methods(state: &AppState, user_id: Uuid) -> AppResult<Methods> {
    let password: bool =
        sqlx::query_scalar("select password_hash <> $2 from users where id = $1").bind(user_id).bind(NO_PASSWORD).fetch_one(&state.db).await?;
    let identities = sqlx::query_as::<_, Identity>("select id, provider, email, created_at from user_identities where user_id = $1 order by created_at")
        .bind(user_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Methods { password, identities, providers: oauth::configured(&state.cfg) })
}

async fn methods(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Methods>> {
    Ok(Json(sign_in_methods(&state, auth.user_id).await?))
}

/// Refused for the last way in: no password and no other identity.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Methods>> {
    let mut tx = state.db.begin().await?;
    // Locks the user, so two tabs can't each remove one of the last two ways in.
    let hash: String = sqlx::query_scalar("select password_hash from users where id = $1 for update").bind(auth.user_id).fetch_one(&mut *tx).await?;
    let others: i64 = sqlx::query_scalar("select count(*) from user_identities where user_id = $1 and id <> $2")
        .bind(auth.user_id)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    let provider: String = sqlx::query_scalar("select provider from user_identities where id = $1 and user_id = $2")
        .bind(id)
        .bind(auth.user_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(AppError::NotFound)?;
    if hash == NO_PASSWORD && others == 0 {
        return Err(AppError::BadRequest("Set a password or connect another sign-in method first, so you can still sign in.".into()));
    }
    sqlx::query("delete from user_identities where id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.identity_unlinked", json!({ "provider": provider })).await;
    Ok(Json(sign_in_methods(&state, auth.user_id).await?))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Form;
    use axum::http::StatusCode;
    use axum::routing::post;
    use sqlx::PgPool;

    use super::*;
    use crate::mail::oauth::tests::{FakeProvider, config, id_token};

    const GOOGLE: &str = "https://accounts.google.com";

    /// A token endpoint that trades a code issued for one sign-in: only with the verifier matching that sign-in's challenge.
    #[derive(Clone, Default)]
    struct FakeOidc {
        /// code → (code_challenge, ID token claims)
        codes: Arc<Mutex<HashMap<String, (String, Value)>>>,
        hits: Arc<Mutex<usize>>,
    }

    async fn token(State(f): State<FakeOidc>, Form(form): Form<HashMap<String, String>>) -> (StatusCode, Json<Value>) {
        *f.hits.lock().unwrap() += 1;
        let issued = f.codes.lock().unwrap().remove(&form["code"]);
        match issued {
            Some((challenge, claims)) if oauth::pkce_challenge(&form["code_verifier"]) == challenge && form["client_id"] == "client-123" => {
                (StatusCode::OK, Json(json!({ "access_token": "access", "expires_in": 3599, "id_token": id_token(claims) })))
            }
            _ => (StatusCode::BAD_REQUEST, Json(json!({ "error": "invalid_grant" }))),
        }
    }

    struct Server {
        base: String,
        client: reqwest::Client,
        db: PgPool,
        fake: FakeOidc,
    }

    /// A sign-in started in the browser: the provider's authorize query and the cookie Splee set.
    struct Started {
        q: HashMap<String, String>,
        cookie: String,
    }

    /// Where the callback sent the browser, and the session it started.
    struct Back {
        location: String,
        session: Option<String>,
    }

    impl Back {
        fn error(&self) -> String {
            let query = Url::parse(&format!("http://x{}", self.location)).unwrap();
            query.query_pairs().find(|(k, _)| k == "sso_error").map(|(_, v)| v.into_owned()).unwrap_or_default()
        }
    }

    impl Server {
        async fn start(db: PgPool, open_signup: bool) -> Self {
            let fake = FakeOidc::default();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let app = Router::new().route("/token", post(token)).with_state(fake.clone());
            tokio::spawn(async move { axum::serve(listener, app).await });
            let cfg = Config { open_signup, ..config(&FakeProvider { url, hits: Default::default(), forms: Default::default() }) };
            let state = AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0, rate_limits: Default::default() };

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", crate::routes::router(&state)).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap();
            Self { base, client, db, fake }
        }

        async fn begin(&self, provider: &str, query: &[(&str, &str)], session: Option<&str>) -> Started {
            let mut req = self.client.get(Url::parse_with_params(&format!("{}/auth/sso/{provider}/start", self.base), query).unwrap());
            if let Some(session) = session {
                req = req.header("cookie", format!("splee_session={session}"));
            }
            let res = req.send().await.unwrap();
            assert_eq!(res.status(), 303);
            let cookie = set_cookie(&res, COOKIE).expect("the start sets the sign-in cookie");
            let q = Url::parse(res.headers()["location"].to_str().unwrap()).unwrap().query_pairs().into_owned().collect();
            Started { q, cookie }
        }

        /// The provider signs someone in for `started` with these claims, over its defaults for Google, and hands out a code.
        fn issue(&self, started: &Started, claims: Value) -> String {
            let mut all = json!({ "iss": GOOGLE, "aud": "client-123", "exp": Utc::now().timestamp() + 3600, "nonce": started.q["nonce"] });
            all.as_object_mut().unwrap().extend(claims.as_object().unwrap().clone());
            let code = random_token();
            self.fake.codes.lock().unwrap().insert(code.clone(), (started.q["code_challenge"].clone(), all));
            code
        }

        async fn back(&self, cookie: Option<&str>, query: &[(&str, &str)], session: Option<&str>) -> Back {
            let cookies: Vec<String> = [cookie.map(|c| format!("{COOKIE}={c}")), session.map(|s| format!("splee_session={s}"))].into_iter().flatten().collect();
            let res = self
                .client
                .get(Url::parse_with_params(&format!("{}/auth/sso/callback", self.base), query).unwrap())
                .header("cookie", cookies.join("; "))
                .send()
                .await
                .unwrap();
            assert_eq!(res.status(), 303);
            assert_eq!(set_cookie(&res, COOKIE).as_deref(), Some(""), "the callback always clears the sign-in cookie");
            Back { location: res.headers()["location"].to_str().unwrap().to_string(), session: set_cookie(&res, "splee_session") }
        }

        /// Starts a sign-in, has the provider sign in with `claims` and comes back.
        async fn sign_in(&self, provider: &str, query: &[(&str, &str)], claims: Value) -> Back {
            let started = self.begin(provider, query, None).await;
            let code = self.issue(&started, claims);
            self.back(Some(&started.cookie), &[("code", &code), ("state", &started.q["state"])], None).await
        }

        async fn password_login(&self, email: &str, password: &str) -> u16 {
            let res = self.client.post(format!("{}/auth/login", self.base)).json(&json!({ "email": email, "password": password })).send().await.unwrap();
            res.status().as_u16()
        }

        async fn user(&self, email: &str) -> Option<(Uuid, String)> {
            sqlx::query_as("select id, password_hash from users where email = $1::citext").bind(email).fetch_optional(&self.db).await.unwrap()
        }

        async fn identities(&self) -> Vec<(Uuid, String, String, String)> {
            sqlx::query_as("select user_id, provider, subject, email from user_identities order by created_at").fetch_all(&self.db).await.unwrap()
        }

        async fn actions(&self, user_id: Uuid) -> Vec<(String, Value)> {
            sqlx::query_as("select action, details from audit_events where actor_user_id = $1 order by created_at").bind(user_id).fetch_all(&self.db).await.unwrap()
        }

        /// Ana has a password, "correct horse", and owns Acme.
        async fn ana(&self) -> Uuid {
            let ana: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', $1) returning id")
                .bind(crate::auth::hash_password("correct horse").unwrap())
                .fetch_one(&self.db)
                .await
                .unwrap();
            let acme: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&self.db).await.unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(acme).bind(ana).execute(&self.db).await.unwrap();
            ana
        }
    }

    /// The value a response sets a cookie to, empty when it clears it.
    fn set_cookie(res: &reqwest::Response, name: &str) -> Option<String> {
        res.headers().get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).find_map(|v| {
            let (pair, _) = v.split_once(';').unwrap_or((v, ""));
            pair.strip_prefix(&format!("{name}=")).map(str::to_string)
        })
    }

    fn verified(email: &str, sub: &str) -> Value {
        json!({ "sub": sub, "email": email, "email_verified": true, "name": "Someone New" })
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_new_google_account_signs_up_without_a_password(db: PgPool) {
        let s = Server::start(db, true).await;
        let started = s.begin("google", &[("from", "signup"), ("next", "/app/leads")], None).await;
        assert_eq!(started.q["scope"], "openid email profile");
        assert_eq!(started.q["redirect_uri"], "http://localhost:3404/api/auth/sso/callback");
        assert_eq!((started.q["code_challenge_method"].as_str(), started.q["prompt"].as_str()), ("S256", "select_account"));
        let code = s.issue(&started, verified("New@Example.test", "g-1"));
        let back = s.back(Some(&started.cookie), &[("code", &code), ("state", &started.q["state"])], None).await;
        assert_eq!(back.location, "/app/leads");
        assert!(back.session.is_some_and(|c| !c.is_empty()));

        let (user, hash) = s.user("new@example.test").await.unwrap();
        assert_eq!(hash, NO_PASSWORD);
        let (org, role): (String, String) = sqlx::query_as("select o.name, m.role from memberships m join organizations o on o.id = m.org_id where m.user_id = $1")
            .bind(user)
            .fetch_one(&s.db)
            .await
            .unwrap();
        assert_eq!((org.as_str(), role.as_str()), ("Someone New", "owner"));
        assert_eq!(s.identities().await, [(user, "google".to_string(), "g-1".to_string(), "new@example.test".to_string())]);
        assert_eq!(s.actions(user).await, [("user.signed_in".to_string(), json!({ "method": "google" }))]);
        assert_eq!(s.password_login("new@example.test", NO_PASSWORD).await, 400, "no password signs in to it");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_verified_address_links_the_existing_account(db: PgPool) {
        let s = Server::start(db, false).await;
        let ana = s.ana().await;
        let back = s.sign_in("google", &[], verified("ana@acme.test", "g-ana")).await;
        assert_eq!(back.location, "/app");
        assert!(back.session.is_some());
        assert_eq!(s.identities().await, [(ana, "google".to_string(), "g-ana".to_string(), "ana@acme.test".to_string())]);
        let actions: Vec<String> = s.actions(ana).await.into_iter().map(|(a, _)| a).collect();
        assert_eq!(actions, ["user.identity_linked", "user.signed_in"]);
        assert_eq!(s.password_login("ana@acme.test", "correct horse").await, 200, "the password still works");

        // A Microsoft personal account vouches for its address too.
        let personal = json!({ "iss": format!("https://login.microsoftonline.com/{CONSUMER_TENANT}/v2.0"), "tid": CONSUMER_TENANT, "sub": "m-ana",
                               "email": "ana@acme.test" });
        assert!(s.sign_in("microsoft", &[], personal).await.session.is_some());
        assert_eq!(s.identities().await.len(), 2);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_unverified_address_is_not_linked(db: PgPool) {
        let s = Server::start(db, true).await;
        let ana = s.ana().await;
        let back = s.sign_in("google", &[("next", "/app/inbox")], json!({ "sub": "g-x", "email": "ana@acme.test", "email_verified": false })).await;
        assert!(back.location.starts_with("/signin?"), "{}", back.location);
        assert!(back.location.contains("next=%2Fapp%2Finbox"), "the page keeps where it was going: {}", back.location);
        assert_eq!(back.error(), "Google didn't confirm that ana@acme.test is yours. Sign in with your password, then connect Google on your Account page.");
        assert!(back.session.is_none());

        // Nor a Microsoft work account's email claim, which its tenant's admin sets, unless Microsoft vouches for it.
        let tenant = "5f1c0b5e-0000-4000-8000-000000000001";
        let work = json!({ "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"), "tid": tenant, "sub": "m-x", "email": "ana@acme.test" });
        assert!(s.sign_in("microsoft", &[], work.clone()).await.error().starts_with("Microsoft didn't confirm"));
        assert!(s.identities().await.is_empty());
        let failed = |reason: &str| ("user.sign_in_failed".to_string(), json!({ "reason": reason }));
        assert_eq!(s.actions(ana).await, [failed("google"), failed("microsoft")]);

        let mut vouched = work;
        vouched["xms_edov"] = json!(true);
        assert!(s.sign_in("microsoft", &[], vouched).await.session.is_some());

        // Nor does an unconfirmed address make a new account, which a later verified sign-in for that address would link to.
        let stranger = json!({ "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"), "tid": tenant, "sub": "m-y", "email": "bo@acme.test" });
        assert!(s.sign_in("microsoft", &[], stranger).await.error().contains("can't make an account with it"));
        assert!(s.user("bo@acme.test").await.is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_linked_account_signs_in_by_subject_after_its_address_changes(db: PgPool) {
        let s = Server::start(db, true).await;
        let ana = s.ana().await;
        s.sign_in("google", &[], verified("ana@acme.test", "g-ana")).await;
        let back = s.sign_in("google", &[], json!({ "sub": "g-ana", "email": "ana@elsewhere.test", "email_verified": false })).await;
        assert_eq!(back.location, "/app");
        let session = back.session.unwrap();
        let signed_in: Uuid =
            sqlx::query_scalar("select user_id from sessions where token_hash = $1").bind(crypto::sha256_hex(&session)).fetch_one(&s.db).await.unwrap();
        assert_eq!(signed_in, ana);
        assert!(s.user("ana@elsewhere.test").await.is_none(), "no second account");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn two_factor_still_asks_for_its_code(db: PgPool) {
        let s = Server::start(db, true).await;
        let ana = s.ana().await;
        sqlx::query("insert into two_factor (user_id, secret_enc, enabled_at) values ($1, 'x', now())").bind(ana).execute(&s.db).await.unwrap();
        let back = s.sign_in("google", &[("next", "/app/leads")], verified("ana@acme.test", "g-ana")).await;
        assert!(back.session.is_none());
        let (page, token) = back.location.split_once("#two_factor_token=").unwrap();
        assert_eq!(page, "/signin?next=%2Fapp%2Fleads");
        let pending = two_factor::pending_login(&s.db, token).await.unwrap();
        assert_eq!(pending, Some((ana, "ana@acme.test".into())), "the same pending sign-in a password gets");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_invite_joins_its_organization_even_when_signups_are_closed(db: PgPool) {
        let s = Server::start(db, false).await;
        let ana = s.ana().await;
        let acme: Uuid = sqlx::query_scalar("select org_id from memberships where user_id = $1").bind(ana).fetch_one(&s.db).await.unwrap();
        sqlx::query("insert into invites (org_id, email, role, token) values ($1, 'bo@acme.test', 'member', 'invite123')").bind(acme).execute(&s.db).await.unwrap();

        let back = s.sign_in("google", &[("from", "signup"), ("invite", "invite123")], verified("bo@acme.test", "g-bo")).await;
        assert_eq!(back.location, "/app");
        let (bo, _) = s.user("bo@acme.test").await.unwrap();
        let orgs: Vec<(Uuid, String)> = sqlx::query_as("select org_id, role from memberships where user_id = $1").bind(bo).fetch_all(&s.db).await.unwrap();
        assert_eq!(orgs, [(acme, "member".to_string())]);
        let accepted: bool = sqlx::query_scalar("select accepted_at is not null from invites where token = 'invite123'").fetch_one(&s.db).await.unwrap();
        assert!(accepted);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn closed_signups_turn_a_new_account_away(db: PgPool) {
        let s = Server::start(db, false).await;
        s.ana().await;
        let back = s.sign_in("google", &[("from", "signup"), ("claim", &Uuid::nil().to_string()), ("token", "abc")], verified("eve@else.test", "g-eve")).await;
        assert!(back.location.starts_with("/signup?"), "{}", back.location);
        assert!(back.location.contains("claim=00000000-0000-0000-0000-000000000000&token=abc"), "the demo claim is kept: {}", back.location);
        assert_eq!(back.error(), "Sign-ups here are invite-only. Ask someone on your team for an invite.");
        assert!(back.session.is_none());
        assert!(s.user("eve@else.test").await.is_none());
        assert!(s.identities().await.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_wrong_state_verifier_or_nonce_is_refused(db: PgPool) {
        let s = Server::start(db, true).await;
        let claims = || verified("new@example.test", "g-1");

        // A state that isn't this browser's, or no cookie at all, is refused before any code is traded.
        let started = s.begin("google", &[], None).await;
        let code = s.issue(&started, claims());
        assert_eq!(s.back(Some(&started.cookie), &[("code", &code), ("state", "someone-elses")], None).await.error(), START_AGAIN);
        assert_eq!(s.back(None, &[("code", &code), ("state", &started.q["state"])], None).await.error(), START_AGAIN);
        assert_eq!(s.back(Some("forged"), &[("code", &code), ("state", &started.q["state"])], None).await.error(), START_AGAIN);
        assert_eq!(*s.fake.hits.lock().unwrap(), 0);

        // A code issued for another sign-in's challenge doesn't trade with this one's verifier.
        let other = s.begin("google", &[], None).await;
        let back = s.back(Some(&other.cookie), &[("code", &code), ("state", &other.q["state"])], None).await;
        assert_eq!(back.error(), "Google turned the sign-in down. Try again.");

        // Nor does an ID token made for another sign-in's nonce.
        let mut stolen = claims();
        stolen["nonce"] = json!(started.q["nonce"]);
        let code = s.issue(&other, stolen);
        let back = s.back(Some(&other.cookie), &[("code", &code), ("state", &other.q["state"])], None).await;
        assert_eq!(back.error(), "Splee couldn't check the Google sign-in. Try again.");
        assert!(back.session.is_none());
        assert!(s.user("new@example.test").await.is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_last_way_in_cannot_be_disconnected(db: PgPool) {
        let s = Server::start(db, true).await;
        let session = s.sign_in("google", &[], verified("new@example.test", "g-1")).await.session.unwrap();
        let call = |method: reqwest::Method, path: &str| {
            s.client.request(method, format!("{}{path}", s.base)).header("cookie", format!("splee_session={session}"))
        };
        let methods: Value = call(reqwest::Method::GET, "/auth/sign-in-methods").send().await.unwrap().json().await.unwrap();
        assert_eq!((methods["password"].clone(), methods["providers"].clone()), (json!(false), json!(["google", "microsoft"])));
        let google = methods["identities"][0]["id"].as_str().unwrap().to_string();

        let res = call(reqwest::Method::DELETE, &format!("/auth/identities/{google}")).send().await.unwrap();
        assert_eq!(res.status(), 400);
        assert_eq!(s.identities().await.len(), 1);

        // Connecting Microsoft from the Account page gives a second way in.
        let started = s.begin("microsoft", &[("from", "account")], Some(&session)).await;
        let tenant = "5f1c0b5e-0000-4000-8000-000000000001";
        let code = s.issue(&started, json!({ "iss": format!("https://login.microsoftonline.com/{tenant}/v2.0"), "tid": tenant, "sub": "m-1" }));
        let back = s.back(Some(&started.cookie), &[("code", &code), ("state", &started.q["state"])], Some(&session)).await;
        assert_eq!(back.location, "/app/account?connected=microsoft");
        let res = call(reqwest::Method::DELETE, &format!("/auth/identities/{google}")).send().await.unwrap();
        assert_eq!(res.status(), 200);
        let methods: Value = res.json().await.unwrap();
        assert_eq!(methods["identities"][0]["provider"], "microsoft");
        let microsoft = methods["identities"][0]["id"].as_str().unwrap().to_string();
        assert_eq!(call(reqwest::Method::DELETE, &format!("/auth/identities/{microsoft}")).send().await.unwrap().status(), 400);

        // Setting a password takes no current one for an account without, and then the last identity can go.
        let res = call(reqwest::Method::POST, "/auth/password").json(&json!({ "new_password": "a new password" })).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(call(reqwest::Method::DELETE, &format!("/auth/identities/{microsoft}")).send().await.unwrap().status(), 200);
        assert_eq!(s.password_login("new@example.test", "a new password").await, 200);
        let res = call(reqwest::Method::POST, "/auth/password").json(&json!({ "new_password": "another one" })).send().await.unwrap();
        assert_eq!(res.status(), 400, "now the current password is needed");
    }

    #[test]
    fn the_id_token_must_be_for_this_app_and_this_sign_in() {
        let now = 1_800_000_000;
        let base = json!({ "iss": GOOGLE, "aud": "client-123", "exp": now + 600, "nonce": "n", "sub": "g-1", "email": "Ana@Acme.test", "email_verified": true });
        let check = |change: Value| {
            let mut claims = base.clone();
            claims.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
            verify_id_token(Provider::Google, "client-123", &id_token(claims), "n", now)
        };
        let ok = check(json!({})).unwrap();
        assert_eq!((ok.subject.as_str(), ok.email.as_deref(), ok.verified), ("g-1", Some("ana@acme.test"), true));
        assert_eq!(check(json!({ "aud": ["other", "client-123"] })).map(|a| a.subject).ok().as_deref(), Some("g-1"));
        assert_eq!(check(json!({ "iss": "https://evil.test" })).unwrap_err(), "issuer");
        assert_eq!(check(json!({ "aud": "another-app" })).unwrap_err(), "audience");
        assert_eq!(check(json!({ "exp": now - 120 })).unwrap_err(), "expired");
        assert_eq!(check(json!({ "nonce": "other" })).unwrap_err(), "nonce");
        assert_eq!(check(json!({ "sub": "" })).unwrap_err(), "no subject");
        assert!(!check(json!({ "email_verified": "false" })).unwrap().verified);
        assert!(verify_id_token(Provider::Google, "client-123", "not-a-jwt", "n", now).is_err());
        let ms = json!({ "iss": "https://login.microsoftonline.com/t1/v2.0", "tid": "t2", "aud": "client-123", "exp": now, "nonce": "n", "sub": "m" });
        assert_eq!(verify_id_token(Provider::Microsoft, "client-123", &id_token(ms), "n", now).unwrap_err(), "issuer", "another tenant's issuer");
    }

    #[test]
    fn next_paths_stay_on_this_site() {
        assert!(safe_next("/app/leads?x=1"));
        for bad in ["https://evil.test", "//evil.test", "/\\evil.test", "/app\nSet-Cookie: x", "/caf\u{e9}"] {
            assert!(!safe_next(bad), "{bad}");
        }
        assert!(token_like("abc123") && !token_like("a/b") && !token_like(""));
    }
}
