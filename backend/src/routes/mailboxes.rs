use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::crypto::encrypt;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::Mailbox;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mailboxes", get(list).post(create))
        .route("/mailboxes/{id}", patch(update).delete(remove))
        .route("/mailboxes/{id}/test", post(test))
        .route("/mailboxes/{id}/check-dns", post(check_dns))
}

async fn load(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<Mailbox> {
    sqlx::query_as::<_, Mailbox>("select * from mailboxes where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<Mailbox>>> {
    let mailboxes = sqlx::query_as::<_, Mailbox>("select * from mailboxes where org_id = $1 order by created_at")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(mailboxes))
}

/// MailboxInput from types.ts. Every field is optional so PATCH can reuse it.
#[derive(Deserialize)]
struct MailboxBody {
    email: Option<String>,
    from_name: Option<String>,
    smtp_host: Option<String>,
    smtp_port: Option<i32>,
    smtp_security: Option<String>,
    smtp_username: Option<String>,
    smtp_password: Option<String>,
    imap_host: Option<String>,
    imap_port: Option<i32>,
    imap_username: Option<String>,
    imap_password: Option<String>,
    daily_limit: Option<i32>,
    /// What the email provider charges per email, so spend stats can include it. Flat-rate mailboxes leave it at 0.
    cost_per_email_micros: Option<i64>,
    status: Option<String>,
    ramp_up: Option<bool>,
}

fn trimmed(value: &Option<String>) -> Option<&str> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty())
}

impl MailboxBody {
    fn validate(&self) -> AppResult<Option<String>> {
        if let Some(security) = &self.smtp_security {
            util_input::one_of("smtp_security", security, &["tls", "starttls", "none"])?;
        }
        if let Some(status) = &self.status {
            util_input::one_of("status", status, &["active", "paused"])?;
        }
        let port_ok = |p: Option<i32>| p.is_none_or(|p| (1..=65535).contains(&p));
        if !port_ok(self.smtp_port) || !port_ok(self.imap_port) {
            return Err(AppError::BadRequest("Ports must be between 1 and 65535".into()));
        }
        if self.daily_limit.is_some_and(|l| !(1..=2000).contains(&l)) {
            return Err(AppError::BadRequest("Daily limit must be between 1 and 2000".into()));
        }
        if self.cost_per_email_micros.is_some_and(|c| !(0..=1_000_000).contains(&c)) {
            return Err(AppError::BadRequest("Cost per email must be between $0 and $1".into()));
        }
        match self.email.as_deref() {
            Some(raw) => Ok(Some(util_input::email(raw).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?)),
            None => Ok(None),
        }
    }
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<MailboxBody>) -> AppResult<Json<Mailbox>> {
    auth.require_admin()?;
    let email = body.validate()?.ok_or_else(|| AppError::BadRequest("Email is required".into()))?;
    let smtp_host = trimmed(&body.smtp_host).ok_or_else(|| AppError::BadRequest("SMTP host is required".into()))?;
    let smtp_password = body.smtp_password.as_deref().filter(|p| !p.is_empty()).ok_or_else(|| AppError::BadRequest("SMTP password is required".into()))?;
    let smtp_username = trimmed(&body.smtp_username).unwrap_or(&email);
    // Most providers use the same login for SMTP and IMAP, so blank IMAP credentials fall back to the SMTP ones.
    let imap_host = trimmed(&body.imap_host).unwrap_or("");
    let (imap_username, imap_password) = if imap_host.is_empty() {
        ("", "")
    } else {
        (
            trimmed(&body.imap_username).unwrap_or(smtp_username),
            body.imap_password.as_deref().filter(|p| !p.is_empty()).unwrap_or(smtp_password),
        )
    };
    let secret = &state.cfg.app_secret;
    let mailbox = sqlx::query_as::<_, Mailbox>(
        "insert into mailboxes (org_id, email, from_name, smtp_host, smtp_port, smtp_security, smtp_username, smtp_password_enc,
           imap_host, imap_port, imap_username, imap_password_enc, daily_limit, status, cost_per_email_micros, ramp_up)
         values ($1, $2, coalesce($3, ''), $4, coalesce($5, 587), coalesce($6, 'starttls'), $7, $8,
           $9, coalesce($10, 993), $11, $12, coalesce($13, 30), coalesce($14, 'active'), coalesce($15, 0), coalesce($16, true))
         returning *",
    )
    .bind(auth.org_id)
    .bind(&email)
    .bind(body.from_name.as_deref().map(str::trim))
    .bind(smtp_host)
    .bind(body.smtp_port)
    .bind(&body.smtp_security)
    .bind(smtp_username)
    .bind(encrypt(secret, smtp_password))
    .bind(imap_host)
    .bind(body.imap_port)
    .bind(imap_username)
    .bind(if imap_password.is_empty() { String::new() } else { encrypt(secret, imap_password) })
    .bind(body.daily_limit)
    .bind(&body.status)
    .bind(body.cost_per_email_micros)
    .bind(body.ramp_up)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match AppError::from(e) {
        AppError::Conflict(_) => AppError::Conflict("This mailbox is already connected".into()),
        other => other,
    })?;
    Ok(Json(mailbox))
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<MailboxBody>,
) -> AppResult<Json<Mailbox>> {
    auth.require_admin()?;
    let mailbox = load(&state.db, id, auth.org_id).await?;
    let email = body.validate()?;
    let secret = &state.cfg.app_secret;
    let encrypt_new = |p: &Option<String>| p.as_deref().filter(|p| !p.is_empty()).map(|p| encrypt(secret, p));
    let mailbox = sqlx::query_as::<_, Mailbox>(
        "update mailboxes set
           email = coalesce($2, email),
           from_name = coalesce($3, from_name),
           smtp_host = coalesce($4, smtp_host),
           smtp_port = coalesce($5, smtp_port),
           smtp_security = coalesce($6, smtp_security),
           smtp_username = coalesce($7, smtp_username),
           smtp_password_enc = coalesce($8, smtp_password_enc),
           imap_host = coalesce($9, imap_host),
           -- Another IMAP server, or IMAP added to a mailbox that has been sending, starts over like a new mailbox.
           imap_last_uid = case when coalesce($9, imap_host) <> imap_host then -1 else imap_last_uid end,
           imap_uid_validity = case when coalesce($9, imap_host) <> imap_host then null else imap_uid_validity end,
           imap_port = coalesce($10, imap_port),
           imap_username = coalesce($11, imap_username),
           imap_password_enc = coalesce($12, imap_password_enc),
           daily_limit = coalesce($13, daily_limit),
           status = coalesce($14, status),
           cost_per_email_micros = coalesce($15, cost_per_email_micros),
           ramp_up = coalesce($16, ramp_up)
         where id = $1 returning *",
    )
    .bind(mailbox.id)
    .bind(email)
    .bind(body.from_name.as_deref().map(str::trim))
    .bind(trimmed(&body.smtp_host))
    .bind(body.smtp_port)
    .bind(&body.smtp_security)
    .bind(trimmed(&body.smtp_username))
    .bind(encrypt_new(&body.smtp_password))
    .bind(body.imap_host.as_deref().map(str::trim))
    .bind(body.imap_port)
    .bind(body.imap_username.as_deref().map(str::trim))
    .bind(encrypt_new(&body.imap_password))
    .bind(body.daily_limit)
    .bind(&body.status)
    .bind(body.cost_per_email_micros)
    .bind(body.ramp_up)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(mailbox))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let mailbox = load(&state.db, id, auth.org_id).await?;
    sqlx::query("delete from mailboxes where id = $1").bind(mailbox.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn test(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let mailbox = load(&state.db, id, auth.org_id).await?;
    let result = mail::test_mailbox(&state, &mailbox).await;
    // A passing test clears an error state but never unpauses a mailbox the user paused. Either way a person has now seen
    // the result, so the sender stops testing it on its own.
    let (status, error) = match &result {
        Ok(()) => (if mailbox.status == "error" { "active" } else { mailbox.status.as_str() }, String::new()),
        Err(e) => ("error", format!("{e:#}")),
    };
    sqlx::query("update mailboxes set status = $2, last_error = $3, recheck_at = null where id = $1")
        .bind(mailbox.id)
        .bind(status)
        .bind(&error)
        .execute(&state.db)
        .await?;
    Ok(Json(if result.is_ok() { json!({ "ok": true }) } else { json!({ "ok": false, "error": error }) }))
}

async fn check_dns(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Mailbox>> {
    let mailbox = load(&state.db, id, auth.org_id).await?;
    let domain = mailbox.email.rsplit('@').next().unwrap_or_default();
    let dns = mail::check_dns(domain).await.map_err(|e| AppError::BadRequest(format!("DNS check failed: {e:#}")))?;
    let mailbox = sqlx::query_as::<_, Mailbox>("update mailboxes set dns = $2 where id = $1 returning *")
        .bind(mailbox.id)
        .bind(dns)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(mailbox))
}
