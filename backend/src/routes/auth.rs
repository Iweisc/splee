use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::projects;
use super::util_input;
use crate::audit::{self, Target, Who};
use crate::auth::{Auth, SESSION_COOKIE, clear_session_cookie, create_session, hash_password, verify_password};
use crate::crypto::sha256_hex;
use crate::error::{AppError, AppResult};
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

#[derive(Deserialize)]
pub struct ClaimBody {
    pub project_id: Uuid,
    pub token: String,
}

async fn signup(State(state): State<AppState>, jar: CookieJar, Json(body): Json<SignupBody>) -> AppResult<(CookieJar, Json<Me>)> {
    let email = util_input::email(&body.email).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?;
    if body.password.chars().count() < 8 {
        return Err(AppError::BadRequest("Password must be at least 8 characters".into()));
    }
    // The very first account is always allowed, so a fresh instance can get its owner.
    if body.invite_token.is_none() && !state.cfg.open_signup {
        let anyone: bool = sqlx::query_scalar("select exists (select 1 from users)").fetch_one(&state.db).await?;
        if anyone {
            return Err(AppError::Forbidden("Sign-ups here are invite-only. Ask someone on your team for an invite.".into()));
        }
    }
    let name = body.name.trim().to_string();
    let password_hash = hash_password(&body.password)?;

    let mut tx = state.db.begin().await?;
    let user_id: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $2, $3) returning id")
        .bind(&email)
        .bind(&name)
        .bind(&password_hash)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match AppError::from(e) {
            AppError::Conflict(_) => AppError::Conflict("An account with this email already exists. Sign in instead.".into()),
            other => other,
        })?;
    sqlx::query("insert into notification_prefs (user_id) values ($1)").bind(user_id).execute(&mut *tx).await?;

    let (org_id, joined_as) = match body.invite_token.as_deref() {
        Some(token) => {
            let (org_id, role): (Uuid, String) = take_invite(&mut tx, token)
                .await?
                .ok_or_else(|| AppError::BadRequest("This invite is no longer valid".into()))?;
            add_member(&mut tx, org_id, user_id, &role).await?;
            (org_id, Some(role))
        }
        None => {
            let org_name = if name.is_empty() { email.split('@').nth(1).unwrap_or(&email).to_string() } else { name.clone() };
            let org_id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id")
                .bind(&org_name)
                .fetch_one(&mut *tx)
                .await?;
            add_member(&mut tx, org_id, user_id, "owner").await?;
            (org_id, None)
        }
    };

    // A stale or already-claimed demo shouldn't block account creation, so a failed claim is ignored here.
    if let Some(claim) = &body.claim {
        projects::claim(&mut tx, claim.project_id, &claim.token, org_id, user_id).await?;
    }
    tx.commit().await?;
    if let Some(role) = joined_as {
        joined(&state, org_id, user_id, &role).await;
    }

    let cookie = create_session(&state.db, user_id, org_id, state.cfg.secure_cookies()).await?;
    Ok((jar.add(cookie), Json(me(&state, user_id, org_id).await?)))
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
    if !verify_password(&body.password, &hash) {
        audit::record_account(&state.db, user_id, "user.sign_in_failed", json!({ "reason": "password" })).await;
        return Err(invalid());
    }
    // The failures stay counted until the code is right too, or a known password would reset the limit between guesses.
    if two_factor::enabled(&state.db, user_id).await? {
        let two_factor_token = two_factor::start_login(&state.db, user_id).await?;
        return Ok((jar, Json(SignIn::Code { two_factor_token })));
    }
    sign_in(&state, jar, user_id, email).await
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
    if !two_factor::finish_login(&state.db, &body.two_factor_token).await? {
        return Err(expired());
    }
    sign_in(&state, jar, user_id, &email).await
}

/// Clears the email's failed attempts and starts a session in the user's first organization.
async fn sign_in(state: &AppState, jar: CookieJar, user_id: Uuid, email: &str) -> AppResult<(CookieJar, Json<SignIn>)> {
    clear_attempts(&state.db, email).await?;
    let org_id: Uuid = sqlx::query_scalar("select org_id from memberships where user_id = $1 order by created_at limit 1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Forbidden("Your account isn't part of any organization anymore".into()))?;
    let cookie = create_session(&state.db, user_id, org_id, state.cfg.secure_cookies()).await?;
    audit::record_account(&state.db, user_id, "user.signed_in", json!({})).await;
    Ok((jar.add(cookie), Json(SignIn::Done(Box::new(me(state, user_id, org_id).await?)))))
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
    current_password: String,
    new_password: String,
}

/// Signs out every other session too, so changing a leaked password also locks out whoever used it.
async fn change_password(State(state): State<AppState>, auth: Auth, Json(body): Json<PasswordBody>) -> AppResult<Json<Value>> {
    let hash: String = sqlx::query_scalar("select password_hash from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    if !verify_password(&body.current_password, &hash) {
        return Err(AppError::BadRequest("Your current password is wrong".into()));
    }
    if body.new_password.chars().count() < 8 {
        return Err(AppError::BadRequest("Password must be at least 8 characters".into()));
    }
    let new_hash = hash_password(&body.new_password)?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update users set password_hash = $2 where id = $1").bind(auth.user_id).bind(&new_hash).execute(&mut *tx).await?;
    sqlx::query("delete from sessions where user_id = $1 and token_hash <> $2").bind(auth.user_id).bind(&auth.token_hash).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record_account(&state.db, auth.user_id, "user.password_changed", json!({})).await;
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
    if !two_factor::take_code(&state.db, &state.cfg.app_secret, auth.user_id, proof).await? && !verify_password(proof, &hash) {
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
}
