use std::net::SocketAddr;
use std::sync::LazyLock;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use axum_extra::extract::cookie::Cookie;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::demo::client_ip;
use super::projects;
use super::util_input;
use crate::audit::{self, Target, Who};
use crate::auth::{Auth, NO_PASSWORD, SESSION_COOKIE, clear_session_cookie, create_session, hash_password, verify_password};
use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{INVITE_TTL, Organization, User};
use crate::state::AppState;
use crate::two_factor;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
        .route("/auth/login/code", post(login_code))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(get_me).patch(update_me))
        .route("/auth/password", post(change_password))
        .route("/auth/password/forgot", post(forgot_password))
        .route("/auth/password/reset", post(reset_password))
        .route("/auth/switch-org", post(switch_org))
        .route("/auth/two-factor", get(two_factor_status))
        .route("/auth/two-factor/setup", post(two_factor_setup))
        .route("/auth/two-factor/enable", post(two_factor_enable))
        .route("/auth/two-factor/disable", post(two_factor_disable))
        .route("/auth/two-factor/backup-codes", post(new_backup_codes))
        .route("/invites/{token}", get(get_invite))
        .route("/invites/{token}/accept", post(accept_invite))
}

#[derive(Serialize)]
pub struct Me {
    user: User,
    org: Organization,
    role: String,
    orgs: Vec<OrgRef>,
    /// Links in emails (unsubscribe, invites) point here. The mailboxes page warns when it's left on localhost.
    public_url: String,
    /// In OPERATOR_EMAILS, so the Companies page offers to load countries.
    operator: bool,
    /// "google" and "microsoft" when the server has their apps, so the Mailboxes page offers to connect by signing in.
    mailbox_sign_in: Vec<&'static str>,
    /// The server has a Tumblr app, so the integrations page offers to connect Tumblr.
    tumblr_sign_in: bool,
    /// The server has a Meta app, so the integrations page offers to sign in with Facebook for Pages and Instagram.
    meta_sign_in: bool,
    /// The CRMs that sign in which the server has an app for, so the integrations page offers them.
    crm_sign_in: Vec<&'static str>,
    /// Round robin skips this member while it's on.
    away: bool,
    /// The owner filter each of ownership::VIEWS was left on, like {"inbox": "mine"}.
    owner_filters: Value,
}

#[derive(Serialize, FromRow)]
struct OrgRef {
    id: Uuid,
    name: String,
    role: String,
    #[serde(skip)]
    away: bool,
    #[serde(skip)]
    owner_filters: Value,
}

pub async fn me(state: &AppState, user_id: Uuid, org_id: Uuid) -> AppResult<Me> {
    let db = &state.db;
    let user = sqlx::query_as::<_, User>("select * from users where id = $1").bind(user_id).fetch_one(db).await?;
    let org = sqlx::query_as::<_, Organization>("select * from organizations where id = $1")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    let orgs = sqlx::query_as::<_, OrgRef>(
        "select o.id, o.name, m.role, m.away, m.owner_filters from memberships m join organizations o on o.id = m.org_id
         where m.user_id = $1 order by m.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let current = orgs.iter().find(|o| o.id == org_id).ok_or(AppError::Unauthorized)?;
    let (role, away, owner_filters) = (current.role.clone(), current.away, current.owner_filters.clone());
    let operator = state.cfg.is_operator(&user.email);
    let mailbox_sign_in = crate::mail::oauth::configured(&state.cfg);
    let tumblr_sign_in = state.cfg.tumblr.is_some();
    let meta_sign_in = state.cfg.meta.is_some();
    let crm_sign_in = crate::crm::signin::configured(&state.cfg);
    Ok(Me {
        user,
        org,
        role,
        orgs,
        public_url: state.cfg.public_url.clone(),
        operator,
        mailbox_sign_in,
        tumblr_sign_in,
        meta_sign_in,
        crm_sign_in,
        away,
        owner_filters,
    })
}

#[derive(Deserialize)]
struct MeBody {
    away: Option<bool>,
    /// One of ownership::VIEWS and the filter to remember for it.
    view: Option<String>,
    owner_filter: Option<String>,
}

/// Your own settings in this organization: Away, and the owner filter each page was left on.
async fn update_me(State(state): State<AppState>, auth: Auth, Json(body): Json<MeBody>) -> AppResult<Json<Me>> {
    let remembered = match (&body.view, &body.owner_filter) {
        (Some(view), Some(filter)) => {
            util_input::one_of("view", view, crate::ownership::VIEWS)?;
            util_input::one_of("owner_filter", filter, crate::ownership::VIEW_FILTERS)?;
            Some(json!({ view: filter }))
        }
        (None, None) => None,
        _ => return Err(AppError::BadRequest("Send view and owner_filter together".into())),
    };
    sqlx::query("update memberships set away = coalesce($3, away), owner_filters = owner_filters || coalesce($4, '{}') where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(auth.user_id)
        .bind(body.away)
        .bind(remembered)
        .execute(&state.db)
        .await?;
    Ok(Json(me(&state, auth.user_id, auth.org_id).await?))
}

#[derive(Deserialize)]
struct SignupBody {
    email: String,
    password: String,
    #[serde(default)]
    name: String,
    claim: Option<ClaimBody>,
    invite_token: Option<String>,
}

#[derive(Deserialize, Serialize)]
pub struct ClaimBody {
    pub project_id: Uuid,
    pub token: String,
}

async fn signup(State(state): State<AppState>, jar: CookieJar, Json(body): Json<SignupBody>) -> AppResult<(CookieJar, Json<Me>)> {
    let email = util_input::email(&body.email).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?;
    check_new_password(&body.password)?;
    may_sign_up(&state, body.invite_token.is_some()).await?;
    let password_hash = hash_password(&body.password)?;

    let mut tx = state.db.begin().await?;
    let account = create_account(&mut tx, &email, body.name.trim(), &password_hash, body.invite_token.as_deref(), body.claim.as_ref()).await?;
    tx.commit().await?;
    account.joined(&state).await;

    let cookie = create_session(&state.db, account.user_id, account.org_id, state.cfg.secure_cookies()).await?;
    Ok((jar.add(cookie), Json(me(&state, account.user_id, account.org_id).await?)))
}

/// Refuses a new account without an invite when OPEN_SIGNUP=false. The very first account is always allowed, so a fresh
/// instance can get its owner.
pub(super) async fn may_sign_up(state: &AppState, invited: bool) -> AppResult<()> {
    if invited || state.cfg.open_signup {
        return Ok(());
    }
    let anyone: bool = sqlx::query_scalar("select exists (select 1 from users)").fetch_one(&state.db).await?;
    if anyone {
        return Err(AppError::Forbidden("Sign-ups here are invite-only. Ask someone on your team for an invite.".into()));
    }
    Ok(())
}

pub(super) struct NewAccount {
    pub user_id: Uuid,
    pub org_id: Uuid,
    /// The invite's role, when they joined an organization rather than starting one.
    joined_as: Option<String>,
}

impl NewAccount {
    /// Records the join in the invite's organization, after the transaction that made the account commits.
    pub async fn joined(&self, state: &AppState) {
        if let Some(role) = &self.joined_as {
            joined(state, self.org_id, self.user_id, role).await;
        }
    }
}

/// Makes the user with their notification settings, in the invite's organization or in a new one they own. Signing up with
/// a password and with Google or Microsoft both come through here.
pub(super) async fn create_account(
    tx: &mut PgConnection,
    email: &str,
    name: &str,
    password_hash: &str,
    invite_token: Option<&str>,
    claim: Option<&ClaimBody>,
) -> AppResult<NewAccount> {
    let user_id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, $3) returning id")
        .bind(email)
        .bind(name)
        .bind(password_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match AppError::from(e) {
            AppError::Conflict(_) => AppError::Conflict("An account with this email already exists. Sign in instead.".into()),
            other => other,
        })?;
    sqlx::query("insert into notification_prefs (user_id) values ($1)").bind(user_id).execute(&mut *tx).await?;

    let (org_id, joined_as) = match invite_token {
        Some(token) => {
            let (org_id, role): (Uuid, String) =
                take_invite(tx, token).await?.ok_or_else(|| AppError::BadRequest("This invite is no longer valid".into()))?;
            add_member(tx, org_id, user_id, &role).await?;
            (org_id, Some(role))
        }
        None => {
            let org_name = if name.is_empty() { email.split('@').nth(1).unwrap_or(email) } else { name };
            let org_id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(org_name).fetch_one(&mut *tx).await?;
            add_member(tx, org_id, user_id, "owner").await?;
            (org_id, None)
        }
    };

    // A stale or already-claimed demo shouldn't block account creation, so a failed claim is ignored here.
    if let Some(claim) = claim {
        projects::claim(tx, claim.project_id, &claim.token, org_id, user_id).await?;
    }
    Ok(NewAccount { user_id, org_id, joined_as })
}

async fn joined(state: &AppState, org_id: Uuid, user_id: Uuid, role: &str) {
    let email: String =
        sqlx::query_scalar("select email::text from users where id = $1").bind(user_id).fetch_one(&state.db).await.unwrap_or_default();
    audit::record(&state.db, Who::member(org_id, user_id), "member.joined", Target::new("user", user_id, &email), json!({ "role": role })).await;
}

async fn add_member(conn: &mut PgConnection, org_id: Uuid, user_id: Uuid, role: &str) -> AppResult<()> {
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3) on conflict do nothing")
        .bind(org_id)
        .bind(user_id)
        .bind(role)
        .execute(conn)
        .await?;
    Ok(())
}

async fn take_invite(conn: &mut PgConnection, token: &str) -> AppResult<Option<(Uuid, String)>> {
    Ok(sqlx::query_as(&format!(
        "update invites set accepted_at = now()
         where token = $1 and accepted_at is null and created_at > now() - interval '{INVITE_TTL}' returning org_id, role"
    ))
        .bind(token)
        .fetch_optional(conn)
        .await?)
}

#[derive(Deserialize)]
struct LoginBody {
    email: String,
    password: String,
}

// Verifying against a throwaway hash keeps login timing the same whether or not the email exists.
static DUMMY_HASH: LazyLock<String> = LazyLock::new(|| hash_password("splee-timing-dummy").expect("argon2 hashing works"));

/// False for an account without a password, after the same work as checking one, so timing doesn't tell those accounts apart.
fn password_matches(password: &str, hash: &str) -> bool {
    if hash == NO_PASSWORD {
        verify_password(password, &DUMMY_HASH);
        return false;
    }
    verify_password(password, hash)
}

/// Per email, not per IP: guessing one account's password is the attack, and the IP behind the proxy can be spoofed.
/// Unknown emails count the same, so the limit doesn't reveal which accounts exist.
const MAX_LOGIN_ATTEMPTS: i64 = 10;
const LOGIN_WINDOW: &str = "15 minutes";

/// What a sign-in answers: the session's Me, or with two-factor on, a token for POST /auth/login/code.
#[derive(Serialize)]
#[serde(untagged)]
enum SignIn {
    Done(Box<Me>),
    Code { two_factor_token: String },
}

/// Counts an attempt to sign in as `email` and refuses it past the limit. Recorded before the password or code is checked,
/// under a per-email lock, so a burst of parallel guesses can't slip past the limit. A successful sign-in clears the
/// email's rows, so only failures accumulate.
async fn count_attempt(db: &PgPool, email: &str) -> AppResult<()> {
    sqlx::query(&format!("delete from login_failures where created_at < now() - interval '{LOGIN_WINDOW}'")).execute(db).await?;
    let mut tx = db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('login:' || lower($1)))").bind(email).execute(&mut *tx).await?;
    sqlx::query("insert into login_failures (email) values ($1::citext)").bind(email).execute(&mut *tx).await?;
    let attempts: i64 = sqlx::query_scalar(&format!(
        "select count(*) from login_failures where email = $1::citext and created_at > now() - interval '{LOGIN_WINDOW}'"
    ))
    .bind(email)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    if attempts > MAX_LOGIN_ATTEMPTS {
        return Err(AppError::TooManyRequests("Too many failed sign-ins for this email. Try again in 15 minutes.".into()));
    }
    Ok(())
}

async fn clear_attempts(db: &PgPool, email: &str) -> AppResult<()> {
    sqlx::query("delete from login_failures where email = $1::citext").bind(email).execute(db).await?;
    Ok(())
}

async fn login(State(state): State<AppState>, jar: CookieJar, Json(body): Json<LoginBody>) -> AppResult<(CookieJar, Json<SignIn>)> {
    let email = body.email.trim();
    count_attempt(&state.db, email).await?;
    let invalid = || AppError::BadRequest("Invalid email or password".into());
    let user: Option<(Uuid, String)> = sqlx::query_as("select id, password_hash from users where email = $1::citext")
        .bind(email)
        .fetch_optional(&state.db)
        .await?;
    let Some((user_id, hash)) = user else {
        verify_password(&body.password, &DUMMY_HASH);
        return Err(invalid());
    };
    if !password_matches(&body.password, &hash) {
        audit::record_account(&state.db, user_id, "user.sign_in_failed", json!({ "reason": "password" })).await;
        return Err(invalid());
    }
    let mut tx = state.db.begin().await?;
    if !hold_user(&mut tx, user_id, Some(&hash)).await? {
        return Err(invalid());
    }
    // The failures stay counted until the code is right too, or a known password would reset the limit between guesses.
    if two_factor::enabled(&state.db, user_id).await? {
        let two_factor_token = two_factor::start_login(&mut tx, user_id).await?;
        tx.commit().await?;
        return Ok((jar, Json(SignIn::Code { two_factor_token })));
    }
    sign_in(&state, jar, tx, user_id, email).await
}

/// Locks the user's row until `tx` ends, so a password reset either lands first, and this sign-in then sees the new
/// password or its pending login gone, or waits for it and signs it out with the rest. Checking the password before the
/// lock isn't enough: a reset could land during the slow hash check. False when the password is no longer `hash`.
async fn hold_user(conn: &mut PgConnection, user_id: Uuid, hash: Option<&str>) -> sqlx::Result<bool> {
    let held: Option<i32> = sqlx::query_scalar("select 1 from users where id = $1 and ($2::text is null or password_hash = $2) for share")
        .bind(user_id)
        .bind(hash)
        .fetch_optional(conn)
        .await?;
    Ok(held.is_some())
}

#[derive(Deserialize)]
struct LoginCodeBody {
    two_factor_token: String,
    code: String,
}

/// The second half of signing in with two-factor on. Each code counts as an attempt for the email, so codes get the same
/// 10 tries in 15 minutes as passwords.
async fn login_code(State(state): State<AppState>, jar: CookieJar, Json(body): Json<LoginCodeBody>) -> AppResult<(CookieJar, Json<SignIn>)> {
    let expired = || AppError::BadRequest("This sign-in expired. Enter your password again.".into());
    let (user_id, email) = two_factor::pending_login(&state.db, &body.two_factor_token).await?.ok_or_else(expired)?;
    count_attempt(&state.db, &email).await?;
    if !two_factor::take_code(&state.db, &state.cfg.app_secret, user_id, &body.code).await? {
        audit::record_account(&state.db, user_id, "user.sign_in_failed", json!({ "reason": "code" })).await;
        return Err(AppError::BadRequest("That code didn't work. Try the newest one from your app.".into()));
    }
    let mut tx = state.db.begin().await?;
    hold_user(&mut tx, user_id, None).await?;
    if !two_factor::finish_login(&mut *tx, &body.two_factor_token).await? {
        return Err(expired());
    }
    sign_in(&state, jar, tx, user_id, &email).await
}

/// Starts the session inside `tx`, which holds the user's row (see hold_user).
async fn sign_in(
    state: &AppState,
    jar: CookieJar,
    mut tx: Transaction<'_, Postgres>,
    user_id: Uuid,
    email: &str,
) -> AppResult<(CookieJar, Json<SignIn>)> {
    let (cookie, org_id) = start_session(state, &mut *tx, user_id, email, json!({})).await?;
    tx.commit().await?;
    Ok((jar.add(cookie), Json(SignIn::Done(Box::new(me(state, user_id, org_id).await?)))))
}

/// Clears the email's failed attempts, starts a session in the user's first organization and records the sign-in with
/// `details`. Returns the session cookie and that organization.
pub(super) async fn start_session(
    state: &AppState,
    db: impl sqlx::PgExecutor<'_>,
    user_id: Uuid,
    email: &str,
    details: Value,
) -> AppResult<(Cookie<'static>, Uuid)> {
    clear_attempts(&state.db, email).await?;
    let org_id: Uuid = sqlx::query_scalar("select org_id from memberships where user_id = $1 order by created_at limit 1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Forbidden("Your account isn't part of any organization anymore".into()))?;
    let cookie = create_session(db, user_id, org_id, state.cfg.secure_cookies()).await?;
    audit::record_account(&state.db, user_id, "user.signed_in", details).await;
    Ok((cookie, org_id))
}

async fn logout(State(state): State<AppState>, jar: CookieJar) -> AppResult<(CookieJar, Json<Value>)> {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        sqlx::query("delete from sessions where token_hash = $1").bind(sha256_hex(cookie.value())).execute(&state.db).await?;
    }
    Ok((jar.add(clear_session_cookie(state.cfg.secure_cookies())), Json(json!({ "ok": true }))))
}

async fn get_me(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Me>> {
    Ok(Json(me(&state, auth.user_id, auth.org_id).await?))
}

#[derive(Deserialize)]
struct SwitchOrgBody {
    org_id: Uuid,
}

async fn switch_org(State(state): State<AppState>, auth: Auth, Json(body): Json<SwitchOrgBody>) -> AppResult<Json<Me>> {
    sqlx::query("select 1 from memberships where org_id = $1 and user_id = $2")
        .bind(body.org_id)
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    sqlx::query("update sessions set org_id = $1 where token_hash = $2")
        .bind(body.org_id)
        .bind(&auth.token_hash)
        .execute(&state.db)
        .await?;
    Ok(Json(me(&state, auth.user_id, body.org_id).await?))
}

#[derive(Serialize, FromRow)]
struct InvitePreview {
    org_name: String,
    email: String,
    role: String,
}

async fn get_invite(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<InvitePreview>> {
    let invite = sqlx::query_as::<_, InvitePreview>(&format!(
        "select o.name as org_name, i.email::text as email, i.role from invites i join organizations o on o.id = i.org_id
         where i.token = $1 and i.accepted_at is null and i.created_at > now() - interval '{INVITE_TTL}'"
    ))
    .bind(token)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(invite))
}

async fn accept_invite(State(state): State<AppState>, auth: Auth, Path(token): Path<String>) -> AppResult<Json<Me>> {
    let mut tx = state.db.begin().await?;
    let (org_id, role) =
        take_invite(&mut tx, &token).await?.ok_or_else(|| AppError::BadRequest("This invite is no longer valid".into()))?;
    add_member(&mut tx, org_id, auth.user_id, &role).await?;
    sqlx::query("update sessions set org_id = $1 where token_hash = $2")
        .bind(org_id)
        .bind(&auth.token_hash)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    joined(&state, org_id, auth.user_id, &role).await;
    Ok(Json(me(&state, auth.user_id, org_id).await?))
}

#[derive(Deserialize)]
struct PasswordBody {
    /// Not needed by an account that signed up with Google or Microsoft and has no password yet.
    #[serde(default)]
    current_password: String,
    new_password: String,
}

/// Signs out every other session too, so changing a leaked password also locks out whoever used it.
async fn change_password(State(state): State<AppState>, auth: Auth, Json(body): Json<PasswordBody>) -> AppResult<Json<Value>> {
    let hash: String = sqlx::query_scalar("select password_hash from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    if hash != NO_PASSWORD && !verify_password(&body.current_password, &hash) {
        return Err(AppError::BadRequest("Your current password is wrong".into()));
    }
    check_new_password(&body.new_password)?;
    let new_hash = hash_password(&body.new_password)?;
    let mut tx = state.db.begin().await?;
    // Matched on the hash that was checked, so a password set elsewhere in the meantime can't be replaced without it.
    let changed = sqlx::query("update users set password_hash = $2 where id = $1 and password_hash = $3")
        .bind(auth.user_id)
        .bind(&new_hash)
        .bind(&hash)
        .execute(&mut *tx)
        .await?;
    if changed.rows_affected() == 0 {
        return Err(AppError::Conflict("Your password changed in another tab. Reload the page.".into()));
    }
    sqlx::query("delete from sessions where user_id = $1 and token_hash <> $2").bind(auth.user_id).bind(&auth.token_hash).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.password_changed", json!({ "first": hash == NO_PASSWORD })).await;
    Ok(Json(json!({ "ok": true })))
}

/// The rule for every new password: signing up, changing it and resetting it.
fn check_new_password(password: &str) -> AppResult<()> {
    if password.chars().count() < 8 {
        return Err(AppError::BadRequest("Password must be at least 8 characters".into()));
    }
    Ok(())
}

const RESET_MINUTES: i64 = 30;
const RESETS_PER_EMAIL: i64 = 3;
const RESETS_PER_IP: i64 = 10;

#[derive(Deserialize)]
struct ForgotBody {
    email: String,
}

/// Always the same answer, after the same work, so it can't tell anyone which emails have accounts or a mailbox to send
/// from. The link is looked up and sent after the response. Without a mailbox, `splee reset-password` is the way back in.
async fn forgot_password(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ForgotBody>,
) -> AppResult<Json<Value>> {
    let ok = Json(json!({ "ok": true }));
    let Some(email) = util_input::email(&body.email) else { return Ok(ok) };
    if reset_request_allowed(&state.db, &email, &client_ip(&headers, peer)).await? {
        tokio::spawn(async move { email_reset_link(&state, &email).await });
    }
    Ok(ok)
}

/// Counts a request when it's within 3 an hour for the email and 10 for the address. One lock for every request keeps
/// both counts exact, and this form is used rarely enough for that not to matter.
async fn reset_request_allowed(db: &PgPool, email: &str, ip: &str) -> AppResult<bool> {
    sqlx::query("delete from password_reset_requests where created_at < now() - interval '1 hour'").execute(db).await?;
    let mut tx = db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext('password_reset_requests'))").execute(&mut *tx).await?;
    let (for_email, for_ip): (i64, i64) = sqlx::query_as(
        "select count(*) filter (where email = $1::citext), count(*) filter (where ip = $2) from password_reset_requests
         where created_at > now() - interval '1 hour'",
    )
    .bind(email)
    .bind(ip)
    .fetch_one(&mut *tx)
    .await?;
    let allowed = for_email < RESETS_PER_EMAIL && for_ip < RESETS_PER_IP;
    if allowed {
        sqlx::query("insert into password_reset_requests (email, ip) values ($1, $2)").bind(email).bind(ip).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(allowed)
}

async fn email_reset_link(state: &AppState, email: &str) {
    let reset = match reset_email(&state.db, &state.cfg.public_url, email).await {
        Ok(Some(reset)) => reset,
        Ok(None) => return,
        Err(e) => return tracing::error!(error = ?e, "could not make a password reset link"),
    };
    if let Err(e) = mail::send_system_email(state, reset.org_id, email, "Reset your Splee password", &reset.body).await {
        tracing::warn!(error = ?e, user_id = %reset.user_id, "password reset email failed");
    }
}

struct ResetEmail {
    user_id: Uuid,
    org_id: Uuid,
    body: String,
}

/// A new reset link for the user with `email`, from the first organization they're in that has an active mailbox. None
/// when there's no such user or organization, and then no link is made.
async fn reset_email(db: &PgPool, public_url: &str, email: &str) -> sqlx::Result<Option<ResetEmail>> {
    let found: Option<(Uuid, Uuid)> = sqlx::query_as(
        "select u.id, m.org_id from users u join memberships m on m.user_id = u.id
         where u.email = $1::citext and exists (select 1 from mailboxes b where b.org_id = m.org_id and b.status = 'active')
         order by m.created_at limit 1",
    )
    .bind(email)
    .fetch_optional(db)
    .await?;
    let Some((user_id, org_id)) = found else { return Ok(None) };
    sqlx::query("delete from password_reset_tokens where expires_at < now() - interval '1 day'").execute(db).await?;
    let token = random_token();
    sqlx::query(&format!(
        "insert into password_reset_tokens (user_id, token_hash, expires_at) values ($1, $2, now() + interval '{RESET_MINUTES} minutes')"
    ))
    .bind(user_id)
    .bind(sha256_hex(&token))
    .execute(db)
    .await?;
    let body = format!(
        "Someone asked to reset the password of your Splee account, {email}.\n\nChoose a new password: {public_url}/reset-password?token={token}\n\n\
         The link works once, for the next {RESET_MINUTES} minutes. If you didn't ask for this, ignore this email and your password stays the same.\n"
    );
    Ok(Some(ResetEmail { user_id, org_id, body }))
}

#[derive(Deserialize)]
struct ResetBody {
    token: String,
    password: String,
}

/// Sets the password from an emailed link and signs the user out everywhere. It doesn't sign them in, so two-factor
/// still stands between the new password and the account. 409 for a link that's unknown, used or expired.
async fn reset_password(State(state): State<AppState>, Json(body): Json<ResetBody>) -> AppResult<Json<Value>> {
    let token_hash = sha256_hex(&body.token);
    let dead = |why: &str| AppError::Conflict(format!("{why} Request a new one."));
    let used = || dead("This link was already used.");
    let found: Option<(Uuid, bool, bool)> =
        sqlx::query_as("select user_id, used_at is not null, expires_at <= now() from password_reset_tokens where token_hash = $1")
            .bind(&token_hash)
            .fetch_optional(&state.db)
            .await?;
    let user_id = match found {
        None => return Err(dead("This link isn't valid.")),
        Some((_, true, _)) => return Err(used()),
        Some((_, _, true)) => return Err(dead("This link expired.")),
        Some((user_id, ..)) => user_id,
    };
    check_new_password(&body.password)?;
    let hash = hash_password(&body.password)?;
    let mut tx = state.db.begin().await?;
    // Taken again in the transaction, so two requests with the same link can't both get through.
    let taken = sqlx::query("update password_reset_tokens set used_at = now() where token_hash = $1 and used_at is null and expires_at > now()")
        .bind(&token_hash)
        .execute(&mut *tx)
        .await?;
    if taken.rows_affected() == 0 {
        return Err(used());
    }
    sqlx::query("update password_reset_tokens set used_at = now() where user_id = $1 and used_at is null").bind(user_id).execute(&mut *tx).await?;
    let email: String = sqlx::query_scalar("update users set password_hash = $2 where id = $1 returning email::text")
        .bind(user_id)
        .bind(&hash)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("delete from sessions where user_id = $1").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("delete from pending_logins where user_id = $1").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("delete from login_failures where email = $1::citext").bind(&email).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record_account(&state.db, user_id, "user.password_reset", json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
struct TwoFactorStatus {
    enabled: bool,
    backup_codes_left: i64,
}

async fn two_factor_status(State(state): State<AppState>, auth: Auth) -> AppResult<Json<TwoFactorStatus>> {
    let (enabled, backup_codes_left): (bool, i64) = sqlx::query_as(
        "select exists (select 1 from two_factor where user_id = $1 and enabled_at is not null),
                (select count(*) from backup_codes where user_id = $1 and used_at is null)",
    )
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(TwoFactorStatus { enabled, backup_codes_left }))
}

#[derive(Serialize)]
struct TwoFactorSetup {
    /// Base32, for typing into the app when the QR code can't be scanned.
    secret: String,
    uri: String,
    /// The URI as an SVG data URL.
    qr: String,
}

/// Makes a new key for the member's app. Sign-in doesn't change until /enable gets a code made with it.
async fn two_factor_setup(State(state): State<AppState>, auth: Auth) -> AppResult<Json<TwoFactorSetup>> {
    let email: String = sqlx::query_scalar("select email::text from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    let (secret_enc, secret) = two_factor::new_key(&state.cfg.app_secret);
    let saved = sqlx::query(
        "insert into two_factor (user_id, secret_enc) values ($1, $2)
         on conflict (user_id) do update set secret_enc = excluded.secret_enc where two_factor.enabled_at is null",
    )
    .bind(auth.user_id)
    .bind(&secret_enc)
    .execute(&state.db)
    .await?;
    if saved.rows_affected() == 0 {
        return Err(AppError::Conflict("Two-factor sign-in is already on".into()));
    }
    let uri = two_factor::otpauth_uri(&email, &secret);
    let qr = two_factor::qr_data_url(&uri)?;
    Ok(Json(TwoFactorSetup { secret, uri, qr }))
}

#[derive(Deserialize)]
struct CodeBody {
    code: String,
}

#[derive(Serialize)]
struct BackupCodes {
    backup_codes: Vec<String>,
}

async fn two_factor_enable(State(state): State<AppState>, auth: Auth, Json(body): Json<CodeBody>) -> AppResult<Json<BackupCodes>> {
    let secret_enc: String = sqlx::query_scalar("select secret_enc from two_factor where user_id = $1 and enabled_at is null")
        .bind(auth.user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Conflict("Two-factor sign-in is already on, or its setup was never started".into()))?;
    let step = two_factor::matching_step(&state.cfg.app_secret, &secret_enc, &body.code)?
        .ok_or_else(|| AppError::BadRequest("That code didn't work. Check that the time on your phone is right.".into()))?;
    let mut tx = state.db.begin().await?;
    // Matched on the key that was checked, so a second setup in another tab can't swap it in between.
    let turned_on = sqlx::query(
        "update two_factor set enabled_at = now(), last_step = $3 where user_id = $1 and secret_enc = $2 and enabled_at is null",
    )
    .bind(auth.user_id)
    .bind(&secret_enc)
    .bind(step)
    .execute(&mut *tx)
    .await?;
    if turned_on.rows_affected() == 0 {
        return Err(AppError::Conflict("Two-factor sign-in changed in another tab. Reload the page.".into()));
    }
    let backup_codes = two_factor::replace_backup_codes(&mut tx, &state.cfg.app_secret, auth.user_id).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.two_factor_enabled", json!({})).await;
    Ok(Json(BackupCodes { backup_codes }))
}

#[derive(Deserialize)]
struct ProofBody {
    /// The member's password, a code from their app or a backup code.
    proof: String,
}

/// Turning two-factor off or making new backup codes takes the password or a code, so a session left open on someone
/// else's computer isn't enough. Each try counts as a sign-in attempt, so this can't be used to guess either.
async fn check_proof(state: &AppState, auth: &Auth, proof: &str) -> AppResult<()> {
    let (email, hash): (String, String) = sqlx::query_as("select email::text, password_hash from users where id = $1")
        .bind(auth.user_id)
        .fetch_one(&state.db)
        .await?;
    count_attempt(&state.db, &email).await?;
    if !two_factor::take_code(&state.db, &state.cfg.app_secret, auth.user_id, proof).await? && !password_matches(proof, &hash) {
        return Err(AppError::BadRequest("That isn't your password or a current code".into()));
    }
    clear_attempts(&state.db, &email).await
}

async fn two_factor_disable(State(state): State<AppState>, auth: Auth, Json(body): Json<ProofBody>) -> AppResult<Json<Value>> {
    check_proof(&state, &auth, &body.proof).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("delete from two_factor where user_id = $1").bind(auth.user_id).execute(&mut *tx).await?;
    sqlx::query("delete from backup_codes where user_id = $1").bind(auth.user_id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.two_factor_disabled", json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

async fn new_backup_codes(State(state): State<AppState>, auth: Auth, Json(body): Json<ProofBody>) -> AppResult<Json<BackupCodes>> {
    if !two_factor::enabled(&state.db, auth.user_id).await? {
        return Err(AppError::BadRequest("Turn on two-factor sign-in first".into()));
    }
    check_proof(&state, &auth, &body.proof).await?;
    let mut tx = state.db.begin().await?;
    let backup_codes = two_factor::replace_backup_codes(&mut tx, &state.cfg.app_secret, auth.user_id).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.backup_codes_created", json!({})).await;
    Ok(Json(BackupCodes { backup_codes }))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;
    use crate::crypto::decrypt;
    use crate::two_factor::{code_at, current_step};

    type Answer = AppResult<(CookieJar, Json<SignIn>)>;

    fn state(db: PgPool) -> AppState {
        let cfg = Config {
            database_url: String::new(),
            app_secret: "test-secret".into(),
            public_url: "http://localhost:3424".into(),
            unsubscribe_url: String::new(),
            bind_addr: String::new(),
            agent_model: String::new(),
            agent_concurrency: 0,
            sending_enabled: false,
            bird_bin: None,
            open_signup: true,
            public_agent: false,
            porkbun_api_url: String::new(),
            twilio_api_url: String::new(),
            bots_api_url: String::new(),
            outreach_api_url: String::new(),
            postmaster_api_url: String::new(),
            operator_emails: Vec::new(),
            google: None,
            microsoft: None,
            slack: None,
            tumblr: None,
            meta: None,
            graph_api_url: String::new(),
            salesforce: None,
            zoho: None,
            crm_api_url: String::new(),
            payments_api_url: String::new(),
            meetings_api_url: String::new(),
            webflow_api_url: String::new(),
            linkedin_ads: None,
            google_ads_developer_token: None,
            ads_api_url: String::new(),
            ipinfo_token: None,
            places_api_url: String::new(),
            local_sites_url: String::new(),
            sheets_api_url: String::new(),
            sec_user_agent: None,
            sec_api_url: String::new(),
            news_sites_url: String::new(),
            s3_api_url: String::new(),
        };
        AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0, rate_limits: Default::default() }
    }

    /// Ana, the owner of Acme, signed in as `auth`, with the password "correct horse".
    async fn ana(db: PgPool) -> (AppState, Auth) {
        let state = state(db);
        let user_id: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('ana@acme.test', $1) returning id")
            .bind(hash_password("correct horse").unwrap())
            .fetch_one(&state.db)
            .await
            .unwrap();
        let org_id: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&state.db).await.unwrap();
        add_member(&mut state.db.acquire().await.unwrap(), org_id, user_id, "owner").await.unwrap();
        (state, Auth { user_id, org_id, role: "owner".into(), token_hash: String::new() })
    }

    async fn password(state: &AppState, password: &str) -> Answer {
        login(State(state.clone()), CookieJar::new(), Json(LoginBody { email: "ana@acme.test".into(), password: password.into() })).await
    }

    async fn code(state: &AppState, token: &str, code: &str) -> Answer {
        let body = LoginCodeBody { two_factor_token: token.into(), code: code.into() };
        login_code(State(state.clone()), CookieJar::new(), Json(body)).await
    }

    fn token(answer: Answer) -> String {
        match answer {
            Ok((_, Json(SignIn::Code { two_factor_token }))) => two_factor_token,
            _ => panic!("expected to be asked for a code"),
        }
    }

    fn signed_in(answer: &Answer) -> bool {
        matches!(answer, Ok((jar, Json(SignIn::Done(_)))) if jar.get(SESSION_COOKIE).is_some())
    }

    fn refused(answer: &Answer) -> bool {
        matches!(answer, Err(AppError::BadRequest(_)))
    }

    /// Ana's app's code for `step`, and the step a code was last taken for.
    async fn app_code(state: &AppState, auth: &Auth, step: i64) -> String {
        let secret_enc: String =
            sqlx::query_scalar("select secret_enc from two_factor where user_id = $1").bind(auth.user_id).fetch_one(&state.db).await.unwrap();
        let key = hex::decode(decrypt(&state.cfg.app_secret, &secret_enc).unwrap()).unwrap();
        format!("{:06}", code_at(&key, step))
    }

    async fn last_step(state: &AppState, auth: &Auth) -> i64 {
        sqlx::query_scalar("select last_step from two_factor where user_id = $1").bind(auth.user_id).fetch_one(&state.db).await.unwrap()
    }

    /// A code the app isn't showing in any step that counts now.
    async fn wrong_code(state: &AppState, auth: &Auth) -> String {
        let mut live = Vec::new();
        for step in current_step() - 1..=current_step() + 1 {
            live.push(app_code(state, auth, step).await);
        }
        (0..4).map(|n| format!("{n:06}")).find(|c| !live.contains(c)).unwrap()
    }

    async fn turn_on(state: &AppState, auth: &Auth) -> Vec<String> {
        let Json(setup) = two_factor_setup(State(state.clone()), auth.clone()).await.unwrap();
        assert!(setup.uri.starts_with(&format!("otpauth://totp/Splee:ana@acme.test?secret={}&issuer=Splee", setup.secret)));
        assert!(setup.qr.starts_with("data:image/svg+xml;base64,"));
        let enable = |code: String| two_factor_enable(State(state.clone()), auth.clone(), Json(CodeBody { code }));
        assert!(matches!(enable(wrong_code(state, auth).await).await, Err(AppError::BadRequest(_))));
        assert!(signed_in(&password(state, "correct horse").await), "two-factor stays off until a code from the app is right");
        let Json(codes) = enable(app_code(state, auth, current_step()).await).await.unwrap();
        assert_eq!(codes.backup_codes.len(), 10);
        codes.backup_codes
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn with_two_factor_on_signing_in_takes_a_code_each_used_once(db: PgPool) {
        let (state, auth) = ana(db).await;
        let backup_codes = turn_on(&state, &auth).await;
        assert!(matches!(two_factor_setup(State(state.clone()), auth.clone()).await, Err(AppError::Conflict(_))));

        let pending = token(password(&state, "correct horse").await);
        assert!(refused(&password(&state, "wrong horse").await));
        assert!(refused(&code(&state, "made-up", &app_code(&state, &auth, current_step()).await).await));
        let enabled_on = last_step(&state, &auth).await;
        assert!(refused(&code(&state, &pending, &app_code(&state, &auth, enabled_on).await).await), "turning it on took that step's code");
        let next = app_code(&state, &auth, enabled_on + 1).await;
        assert!(signed_in(&code(&state, &pending, &next).await));
        assert!(refused(&code(&state, &pending, &next).await), "the pending sign-in is used up");
        let again = token(password(&state, "correct horse").await);
        assert!(refused(&code(&state, &again, &next).await), "a code works for one sign-in");

        // Typed any which way, a backup code works once.
        assert!(signed_in(&code(&state, &again, &backup_codes[0].to_uppercase().replace('-', " ")).await));
        let third = token(password(&state, "correct horse").await);
        assert!(refused(&code(&state, &third, &backup_codes[0]).await));
        assert!(signed_in(&code(&state, &third, &backup_codes[1]).await));
        let Json(status) = two_factor_status(State(state.clone()), auth.clone()).await.unwrap();
        assert_eq!((status.enabled, status.backup_codes_left), (true, 8));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn sign_ins_and_two_factor_changes_go_in_the_audit_log(db: PgPool) {
        let (state, auth) = ana(db).await;
        assert!(refused(&password(&state, "wrong horse").await));
        assert!(signed_in(&password(&state, "correct horse").await));
        turn_on(&state, &auth).await;
        let events: Vec<(String, String, Value)> =
            sqlx::query_as("select action, target_label, details from audit_events where org_id = $1 and actor_user_id = $2 order by created_at")
                .bind(auth.org_id)
                .bind(auth.user_id)
                .fetch_all(&state.db)
                .await
                .unwrap();
        let actions: Vec<&str> = events.iter().map(|(action, ..)| action.as_str()).collect();
        // turn_on checks that a password alone still signs in before the code turns two-factor on.
        assert_eq!(actions, ["user.sign_in_failed", "user.signed_in", "user.signed_in", "user.two_factor_enabled"]);
        assert_eq!((events[0].1.as_str(), &events[0].2), ("ana@acme.test", &json!({ "reason": "password" })));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn wrong_codes_count_toward_the_sign_in_limit(db: PgPool) {
        let (state, auth) = ana(db).await;
        turn_on(&state, &auth).await;
        let pending = token(password(&state, "correct horse").await);
        let wrong = wrong_code(&state, &auth).await;
        // The password was the first of the 10 tries, and it stays counted until the code is right too.
        for _ in 0..9 {
            assert!(refused(&code(&state, &pending, &wrong).await));
        }
        let right = app_code(&state, &auth, last_step(&state, &auth).await + 1).await;
        assert!(matches!(code(&state, &pending, &right).await, Err(AppError::TooManyRequests(_))));
        assert!(matches!(password(&state, "correct horse").await, Err(AppError::TooManyRequests(_))));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn turning_it_off_or_making_new_backup_codes_takes_the_password_or_a_code(db: PgPool) {
        let (state, auth) = ana(db).await;
        let old_codes = turn_on(&state, &auth).await;
        let proof = |proof: &str| Json(ProofBody { proof: proof.into() });
        let new = |p: &str| new_backup_codes(State(state.clone()), auth.clone(), proof(p));
        assert!(matches!(new("wrong horse").await, Err(AppError::BadRequest(_))));
        let Json(codes) = new("correct horse").await.unwrap();
        assert!(codes.backup_codes.iter().all(|c| !old_codes.contains(c)));
        let pending = token(password(&state, "correct horse").await);
        assert!(refused(&code(&state, &pending, &old_codes[0]).await), "new codes replace the old");

        let off = |p: String| two_factor_disable(State(state.clone()), auth.clone(), proof(&p));
        assert!(matches!(off(wrong_code(&state, &auth).await).await, Err(AppError::BadRequest(_))));
        let Json(turned_off) = off(app_code(&state, &auth, last_step(&state, &auth).await + 1).await).await.unwrap();
        assert_eq!(turned_off["ok"], true);
        assert!(signed_in(&password(&state, "correct horse").await));
        let Json(status) = two_factor_status(State(state.clone()), auth.clone()).await.unwrap();
        assert_eq!((status.enabled, status.backup_codes_left), (false, 0));
    }

    async fn forgot(state: &AppState, email: &str) -> Value {
        let peer: SocketAddr = "203.0.113.7:4000".parse().unwrap();
        let Json(answer) = forgot_password(State(state.clone()), ConnectInfo(peer), HeaderMap::new(), Json(ForgotBody { email: email.into() })).await.unwrap();
        answer
    }

    async fn reset(state: &AppState, token: &str, password: &str) -> AppResult<Json<Value>> {
        reset_password(State(state.clone()), Json(ResetBody { token: token.into(), password: password.into() })).await
    }

    fn dead(answer: &AppResult<Json<Value>>, why: &str) -> bool {
        matches!(answer, Err(AppError::Conflict(message)) if message.contains(why))
    }

    /// Its SMTP host can't resolve, so nothing a test starts can reach anyone.
    async fn add_mailbox(state: &AppState, auth: &Auth, status: &str) {
        sqlx::query(
            "insert into mailboxes (org_id, email, from_name, smtp_host, smtp_username, smtp_password_enc, status)
             values ($1, 'team@acme.test', 'Acme', 'smtp.invalid', '', '', $2)",
        )
        .bind(auth.org_id)
        .bind(status)
        .execute(&state.db)
        .await
        .unwrap();
    }

    /// The token in a new reset email for Ana, as the forgot route would send it.
    async fn reset_link(state: &AppState) -> String {
        let reset = reset_email(&state.db, &state.cfg.public_url, "ana@acme.test").await.unwrap().expect("a reset email");
        assert!(reset.body.contains("30 minutes") && reset.body.contains("ignore this email"));
        let link = reset.body.split_whitespace().find(|w| w.starts_with("http://localhost:3424/reset-password?token=")).unwrap();
        link.rsplit('=').next().unwrap().to_string()
    }

    async fn allowed(state: &AppState, email: &str, ip: &str) -> bool {
        reset_request_allowed(&state.db, email, ip).await.unwrap()
    }

    async fn rows(state: &AppState, table: &str) -> i64 {
        sqlx::query_scalar(&format!("select count(*) from {table}")).fetch_one(&state.db).await.unwrap()
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn forgetting_an_unknown_email_answers_the_same_and_sends_nothing(db: PgPool) {
        let (state, auth) = ana(db).await;
        add_mailbox(&state, &auth, "active").await;
        let unknown = forgot(&state, "nobody@acme.test").await;
        assert!(reset_email(&state.db, &state.cfg.public_url, "nobody@acme.test").await.unwrap().is_none());
        assert_eq!(rows(&state, "password_reset_tokens").await, 0);
        assert_eq!(unknown, forgot(&state, "ana@acme.test").await);
        assert_eq!(unknown, json!({ "ok": true }));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn without_an_active_mailbox_no_link_is_made_and_the_answer_is_the_same(db: PgPool) {
        let (state, auth) = ana(db).await;
        add_mailbox(&state, &auth, "paused").await;
        assert_eq!(forgot(&state, "ana@acme.test").await, forgot(&state, "nobody@acme.test").await);
        assert!(reset_email(&state.db, &state.cfg.public_url, "ana@acme.test").await.unwrap().is_none());
        assert_eq!(rows(&state, "password_reset_tokens").await, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reset_link_sets_the_password_once_and_signs_out_everywhere(db: PgPool) {
        let (state, auth) = ana(db).await;
        add_mailbox(&state, &auth, "active").await;
        for token_hash in ["a", "b"] {
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                .bind(token_hash)
                .bind(auth.user_id)
                .bind(auth.org_id)
                .execute(&state.db)
                .await
                .unwrap();
        }
        assert!(refused(&password(&state, "wrong horse").await));

        let link = reset_link(&state).await;
        let stored: Vec<String> = sqlx::query_scalar("select token_hash from password_reset_tokens").fetch_all(&state.db).await.unwrap();
        assert_eq!(stored, [sha256_hex(&link)], "one token, and only its hash is kept");
        assert!(matches!(reset(&state, &link, "short").await, Err(AppError::BadRequest(_))), "signup's password rule");
        let Json(done) = reset(&state, &link, "battery staple").await.unwrap();
        assert_eq!(done, json!({ "ok": true }));
        assert_eq!(rows(&state, "sessions").await + rows(&state, "login_failures").await, 0);
        assert!(dead(&reset(&state, &link, "another staple").await, "already used"));
        assert!(dead(&reset(&state, "made-up", "another staple").await, "isn't valid"));

        assert!(refused(&password(&state, "correct horse").await));
        assert!(signed_in(&password(&state, "battery staple").await));
        let resets: i64 = sqlx::query_scalar("select count(*) from audit_events where action = 'user.password_reset' and actor_user_id = $1")
            .bind(auth.user_id)
            .fetch_one(&state.db)
            .await
            .unwrap();
        assert_eq!(resets, 1);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reset_link_dies_after_30_minutes_or_once_another_is_used(db: PgPool) {
        let (state, auth) = ana(db).await;
        add_mailbox(&state, &auth, "active").await;
        let expired = reset_link(&state).await;
        sqlx::query("update password_reset_tokens set expires_at = now() - interval '1 second'").execute(&state.db).await.unwrap();
        assert!(dead(&reset(&state, &expired, "battery staple").await, "expired"));

        let (first, second) = (reset_link(&state).await, reset_link(&state).await);
        assert!(reset(&state, &first, "battery staple").await.is_ok());
        assert!(dead(&reset(&state, &second, "other staple").await, "already used"));
        assert!(signed_in(&password(&state, "battery staple").await));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn reset_requests_are_limited_per_email_and_per_address(db: PgPool) {
        let (state, _) = ana(db).await;
        let (here, there) = ("203.0.113.7", "198.51.100.2");
        for _ in 0..3 {
            assert!(allowed(&state, "ana@acme.test", here).await);
        }
        assert!(!allowed(&state, "Ana@Acme.test", there).await, "3 an hour for an email, from anywhere");
        for n in 0..7 {
            assert!(allowed(&state, &format!("guess{n}@acme.test"), here).await);
        }
        assert!(!allowed(&state, "guess7@acme.test", here).await, "10 an hour from an address");
        assert!(allowed(&state, "guess7@acme.test", there).await);
        assert_eq!(forgot(&state, "ana@acme.test").await, json!({ "ok": true }), "a refused request answers the same");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn after_a_reset_two_factor_still_asks_for_a_code(db: PgPool) {
        let (state, auth) = ana(db).await;
        turn_on(&state, &auth).await;
        add_mailbox(&state, &auth, "active").await;
        let pending = token(password(&state, "correct horse").await);
        let link = reset_link(&state).await;
        assert!(reset(&state, &link, "battery staple").await.is_ok());
        let next = app_code(&state, &auth, last_step(&state, &auth).await + 1).await;
        assert!(refused(&code(&state, &pending, &next).await), "a sign-in started with the old password is gone");
        let answer = password(&state, "battery staple").await;
        assert!(!signed_in(&answer));
        token(answer);
    }

    /// A reset that lands while a sign-in is hashing the old password: the sign-in waits for it, then sees the new password
    /// instead of starting a session the reset already cleared out.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_sign_in_racing_a_reset_does_not_outlive_it(db: PgPool) {
        let (state, auth) = ana(db).await;
        let mut reset = state.db.begin().await.unwrap();
        sqlx::query("update users set password_hash = $2 where id = $1")
            .bind(auth.user_id)
            .bind(hash_password("battery staple").unwrap())
            .execute(&mut *reset)
            .await
            .unwrap();
        sqlx::query("delete from sessions where user_id = $1").bind(auth.user_id).execute(&mut *reset).await.unwrap();
        let racing = tokio::spawn({
            let state = state.clone();
            async move { password(&state, "correct horse").await }
        });
        let mut waited = 0;
        while waited < 100 {
            let waiting = "select count(*) from pg_stat_activity where wait_event_type = 'Lock' and datname = current_database()";
            let blocked: i64 = sqlx::query_scalar(waiting).fetch_one(&state.db).await.unwrap();
            if blocked > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            waited += 1;
        }
        assert!(waited < 100, "the sign-in should wait on the reset's row lock");
        reset.commit().await.unwrap();
        assert!(refused(&racing.await.unwrap()));
        assert_eq!(rows(&state, "sessions").await, 0);
    }
}
