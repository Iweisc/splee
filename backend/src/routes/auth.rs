use std::sync::LazyLock;

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use axum_extra::extract::CookieJar;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use super::projects;
use super::util_input;
use crate::auth::{Auth, SESSION_COOKIE, clear_session_cookie, create_session, hash_password, verify_password};
use crate::crypto::sha256_hex;
use crate::error::{AppError, AppResult};
use crate::models::{INVITE_TTL, Organization, User};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/signup", post(signup))
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(get_me))
        .route("/auth/password", post(change_password))
        .route("/auth/switch-org", post(switch_org))
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
}

#[derive(Serialize, FromRow)]
struct OrgRef {
    id: Uuid,
    name: String,
    role: String,
}

pub async fn me(state: &AppState, user_id: Uuid, org_id: Uuid) -> AppResult<Me> {
    let db = &state.db;
    let user = sqlx::query_as::<_, User>("select * from users where id = $1").bind(user_id).fetch_one(db).await?;
    let org = sqlx::query_as::<_, Organization>("select * from organizations where id = $1")
        .bind(org_id)
        .fetch_one(db)
        .await?;
    let orgs = sqlx::query_as::<_, OrgRef>(
        "select o.id, o.name, m.role from memberships m join organizations o on o.id = m.org_id
         where m.user_id = $1 order by m.created_at",
    )
    .bind(user_id)
    .fetch_all(db)
    .await?;
    let role = orgs.iter().find(|o| o.id == org_id).map(|o| o.role.clone()).ok_or(AppError::Unauthorized)?;
    let operator = state.cfg.is_operator(&user.email);
    let mailbox_sign_in = crate::mail::oauth::configured(&state.cfg);
    Ok(Me { user, org, role, orgs, public_url: state.cfg.public_url.clone(), operator, mailbox_sign_in })
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

    let org_id = match body.invite_token.as_deref() {
        Some(token) => {
            let (org_id, role): (Uuid, String) = take_invite(&mut tx, token)
                .await?
                .ok_or_else(|| AppError::BadRequest("This invite is no longer valid".into()))?;
            add_member(&mut tx, org_id, user_id, &role).await?;
            org_id
        }
        None => {
            let org_name = if name.is_empty() { email.split('@').nth(1).unwrap_or(&email).to_string() } else { name.clone() };
            let org_id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id")
                .bind(&org_name)
                .fetch_one(&mut *tx)
                .await?;
            add_member(&mut tx, org_id, user_id, "owner").await?;
            org_id
        }
    };

    // A stale or already-claimed demo shouldn't block account creation, so a failed claim is ignored here.
    if let Some(claim) = &body.claim {
        projects::claim(&mut tx, claim.project_id, &claim.token, org_id, user_id).await?;
    }
    tx.commit().await?;

    let cookie = create_session(&state.db, user_id, org_id, state.cfg.secure_cookies()).await?;
    Ok((jar.add(cookie), Json(me(&state, user_id, org_id).await?)))
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

async fn login(State(state): State<AppState>, jar: CookieJar, Json(body): Json<LoginBody>) -> AppResult<(CookieJar, Json<Me>)> {
    let email = body.email.trim();
    // Recorded before checking the password, under a per-email lock, so a burst of parallel guesses can't slip past
    // the limit. A successful sign-in clears the email's rows, so only failures accumulate.
    sqlx::query(&format!("delete from login_failures where created_at < now() - interval '{LOGIN_WINDOW}'")).execute(&state.db).await?;
    let mut tx = state.db.begin().await?;
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
        return Err(invalid());
    }
    sqlx::query("delete from login_failures where email = $1::citext").bind(email).execute(&state.db).await?;
    let org_id: Uuid = sqlx::query_scalar("select org_id from memberships where user_id = $1 order by created_at limit 1")
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or_else(|| AppError::Forbidden("Your account isn't part of any organization anymore".into()))?;
    let cookie = create_session(&state.db, user_id, org_id, state.cfg.secure_cookies()).await?;
    Ok((jar.add(cookie), Json(me(&state, user_id, org_id).await?)))
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
    Ok(Json(json!({ "ok": true })))
}
