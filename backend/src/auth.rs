use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use crate::crypto::{random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::models::Project;
use crate::state::AppState;

pub const SESSION_COOKIE: &str = "splee_session";
const SESSION_DAYS: i64 = 30;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| anyhow::anyhow!("hash failed: {e}"))
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|parsed| Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
        .unwrap_or(false)
}

/// Creates a session row and returns the cookie to set on the response.
pub async fn create_session(db: &PgPool, user_id: Uuid, org_id: Uuid) -> AppResult<Cookie<'static>> {
    let token = random_token();
    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, $4)")
        .bind(sha256_hex(&token))
        .bind(user_id)
        .bind(org_id)
        .bind(Utc::now() + Duration::days(SESSION_DAYS))
        .execute(db)
        .await?;
    Ok(Cookie::build((SESSION_COOKIE, token))
        .path("/")
        .http_only(true)
        .same_site(SameSite::Lax)
        .max_age(time_days(SESSION_DAYS))
        .build())
}

pub fn clear_session_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, "")).path("/").max_age(time_days(0)).build()
}

fn time_days(days: i64) -> time::Duration {
    time::Duration::days(days)
}

/// Signed-in user plus the organization the session is scoped to.
#[derive(Debug, Clone)]
pub struct Auth {
    pub user_id: Uuid,
    pub org_id: Uuid,
    pub role: String,
    pub token_hash: String,
}

impl Auth {
    pub fn require_admin(&self) -> AppResult<()> {
        if self.role == "owner" || self.role == "admin" {
            Ok(())
        } else {
            Err(AppError::Forbidden("Only admins can do this".into()))
        }
    }

    /// Loads a project and checks it belongs to the session's organization.
    pub async fn project(&self, db: &PgPool, project_id: Uuid) -> AppResult<Project> {
        sqlx::query_as::<_, Project>("select * from projects where id = $1 and org_id = $2")
            .bind(project_id)
            .bind(self.org_id)
            .fetch_optional(db)
            .await?
            .ok_or(AppError::NotFound)
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let jar = CookieJar::from_headers(&parts.headers);
        let token = jar.get(SESSION_COOKIE).map(|c| c.value().to_string()).ok_or(AppError::Unauthorized)?;
        let token_hash = sha256_hex(&token);
        let row: Option<(Uuid, Uuid, String)> = sqlx::query_as(
            "select s.user_id, s.org_id, m.role from sessions s
             join memberships m on m.user_id = s.user_id and m.org_id = s.org_id
             where s.token_hash = $1 and s.expires_at > now()",
        )
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await?;
        let (user_id, org_id, role) = row.ok_or(AppError::Unauthorized)?;
        Ok(Auth { user_id, org_id, role, token_hash })
    }
}

/// Same as Auth but doesn't reject anonymous visitors. Used by public pages that behave
/// differently when signed in (demo claim, support chat).
pub struct MaybeAuth(pub Option<Auth>);

impl FromRequestParts<AppState> for MaybeAuth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        match Auth::from_request_parts(parts, state).await {
            Ok(a) => Ok(MaybeAuth(Some(a))),
            Err(AppError::Unauthorized) => Ok(MaybeAuth(None)),
            Err(e) => Err(e),
        }
    }
}

/// Public API v1 auth: `Authorization: Bearer splee_<token>`.
#[derive(Debug, Clone)]
pub struct ApiKeyAuth {
    pub org_id: Uuid,
}

pub const API_KEY_PREFIX: &str = "splee_";

impl FromRequestParts<AppState> for ApiKeyAuth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        let org_id: Option<Uuid> = sqlx::query_scalar(
            "update api_keys set last_used_at = now()
             where key_hash = $1 and revoked_at is null returning org_id",
        )
        .bind(sha256_hex(key))
        .fetch_optional(&state.db)
        .await?;
        Ok(ApiKeyAuth { org_id: org_id.ok_or(AppError::Unauthorized)? })
    }
}
