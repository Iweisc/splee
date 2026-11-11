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

/// Backs `splee reset-password <email>`. There's no reset email, because an org may have no mailbox to send it from,
/// so whoever runs the server sets a random password and hands it over. Returns None when no user has that email.
pub async fn reset_password(db: &PgPool, email: &str) -> anyhow::Result<Option<String>> {
    let password = Uuid::new_v4().simple().to_string();
    let hash = hash_password(&password)?;
    let mut tx = db.begin().await?;
    let user_id: Option<Uuid> = sqlx::query_scalar("update users set password_hash = $2 where email = $1::citext returning id")
        .bind(email)
        .bind(&hash)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(user_id) = user_id else { return Ok(None) };
    sqlx::query("delete from sessions where user_id = $1").bind(user_id).execute(&mut *tx).await?;
    sqlx::query("delete from login_failures where email = $1::citext").bind(email).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(password))
}

/// Creates a session row and returns the cookie to set on the response.
/// `secure` comes from Config::secure_cookies, so the cookie is HTTPS-only wherever the app is served over HTTPS.
pub async fn create_session(db: &PgPool, user_id: Uuid, org_id: Uuid, secure: bool) -> AppResult<Cookie<'static>> {
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
        .secure(secure)
        .max_age(time_days(SESSION_DAYS))
        .build())
}

pub fn clear_session_cookie(secure: bool) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, "")).path("/").secure(secure).max_age(time_days(0)).build()
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
        let KeyAuth { org_id, .. } = KeyAuth::from_request_parts(parts, state).await?;
        Ok(ApiKeyAuth { org_id })
    }
}

/// Like ApiKeyAuth, with the key too, for what belongs to a key rather than the organization: the REST hook subscriptions
/// Zapier, Make and n8n make, which stop when their key is revoked.
#[derive(Debug, Clone)]
pub struct KeyAuth {
    pub org_id: Uuid,
    pub key_id: Uuid,
}

impl FromRequestParts<AppState> for KeyAuth {
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or(AppError::Unauthorized)?;
        let (org_id, key_id) = api_key(&state.db, key).await?.ok_or(AppError::Unauthorized)?;
        Ok(KeyAuth { org_id, key_id })
    }
}

/// The organization of a live API key.
pub async fn api_key_org(db: &PgPool, key: &str) -> sqlx::Result<Option<Uuid>> {
    Ok(api_key(db, key).await?.map(|(org_id, _)| org_id))
}

/// The organization and id of a live API key.
async fn api_key(db: &PgPool, key: &str) -> sqlx::Result<Option<(Uuid, Uuid)>> {
    sqlx::query_as("update api_keys set last_used_at = now() where key_hash = $1 and revoked_at is null returning org_id, id")
        .bind(sha256_hex(key))
        .fetch_optional(db)
        .await
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{reset_password, verify_password};

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn reset_password_replaces_the_hash_and_signs_the_user_out(db: PgPool) {
        let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ('ana@example.com', 'old') returning id").fetch_one(&db).await.unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ('t', $1, $2, now() + interval '1 day')")
            .bind(user)
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into login_failures (email) values ('ana@example.com')").execute(&db).await.unwrap();

        let password = reset_password(&db, "Ana@Example.com").await.unwrap().unwrap();

        let hash: String = sqlx::query_scalar("select password_hash from users where id = $1").bind(user).fetch_one(&db).await.unwrap();
        assert!(verify_password(&password, &hash));
        let left: i64 = sqlx::query_scalar("select (select count(*) from sessions) + (select count(*) from login_failures)").fetch_one(&db).await.unwrap();
        assert_eq!(left, 0);
        assert!(reset_password(&db, "nobody@example.com").await.unwrap().is_none());
    }
}
