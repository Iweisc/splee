use axum::extract::{Path, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::crypto::encrypt;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{Mailbox, MailboxStats};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mailboxes", get(list).post(create))
        .route("/mailboxes/stats", get(stats))
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

/// Bounces come back to the mailbox that sent the email, so counting them where they arrive pins them on the right
/// mailbox. Rates need sends, so mailboxes that sent nothing in the window are left out.
async fn stats(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<MailboxStats>>> {
    Ok(Json(org_stats(&state.db, auth.org_id).await?))
}

pub async fn org_stats(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<MailboxStats>> {
    let stats = sqlx::query_as::<_, MailboxStats>(&format!("{STATS} where m.org_id = $1 order by m.email")).bind(org_id).fetch_all(db).await?;
    Ok(stats.into_iter().filter(|s| s.emailed > 0).collect())
}

/// One mailbox's last 30 days, as org_stats counts them.
pub async fn mailbox_stats(db: &PgPool, mailbox_id: Uuid) -> sqlx::Result<Option<MailboxStats>> {
    sqlx::query_as::<_, MailboxStats>(&format!("{STATS} where m.id = $1")).bind(mailbox_id).fetch_optional(db).await
}

const STATS: &str = "select m.id as mailbox_id, m.email::text,
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.status = 'sent' and e.sent_at > now() - interval '30 days') as emailed,
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.direction = 'inbound' and e.classification = 'bounce'
               and e.created_at > now() - interval '30 days') as bounced,
           (select count(distinct e.lead_id) from email_messages e
             where e.mailbox_id = m.id and e.direction = 'inbound' and e.classification is distinct from 'bounce'
               and e.created_at > now() - interval '30 days') as replied
         from mailboxes m";

/// MailboxInput from types.ts. Every field is optional so PATCH can reuse it.
#[derive(Deserialize)]
struct MailboxBody {
    email: Option<String>,
    from_name: Option<String>,
    signature: Option<String>,
    booking_link: Option<String>,
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
        // Like isUrl in the form and the project's booking link, http is let through.
        if trimmed(&self.booking_link).is_some_and(|l| !Url::parse(l).is_ok_and(|u| matches!(u.scheme(), "https" | "http") && u.host_str().is_some())) {
            return Err(AppError::BadRequest("Booking link must be a full URL starting with https://".into()));
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
           imap_host, imap_port, imap_username, imap_password_enc, daily_limit, status, cost_per_email_micros, ramp_up, signature, booking_link)
         values ($1, $2, coalesce($3, ''), $4, coalesce($5, 587), coalesce($6, 'starttls'), $7, $8,
           $9, coalesce($10, 993), $11, $12, coalesce($13, 30), coalesce($14, 'active'), coalesce($15, 0), coalesce($16, true), coalesce($17, ''), coalesce($18, ''))
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
    .bind(body.signature.as_deref().map(str::trim))
    .bind(body.booking_link.as_deref().map(str::trim))
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
           -- Resuming clears why it was paused, like a bounce rate that stopped it.
           last_error = case when $14 = 'active' and status = 'paused' then '' else last_error end,
           cost_per_email_micros = coalesce($15, cost_per_email_micros),
           ramp_up = coalesce($16, ramp_up),
           signature = coalesce($17, signature),
           booking_link = coalesce($18, booking_link)
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
    .bind(body.signature.as_deref().map(str::trim))
    .bind(body.booking_link.as_deref().map(str::trim))
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
    // A test never changes a paused mailbox: only Resume does, and its last_error keeps why it's paused, like too many
    // bounces. Otherwise a passing test clears an error state. Either way a person has now seen the result, so the sender
    // stops testing it on its own.
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    let (status, last_error) = match &error {
        _ if mailbox.status == "paused" => ("paused", mailbox.last_error.as_str()),
        None => (if mailbox.status == "error" { "active" } else { mailbox.status.as_str() }, ""),
        Some(e) => ("error", e.as_str()),
    };
    sqlx::query("update mailboxes set status = $2, last_error = $3, recheck_at = null where id = $1")
        .bind(mailbox.id)
        .bind(status)
        .bind(last_error)
        .execute(&state.db)
        .await?;
    Ok(Json(match error {
        None => json!({ "ok": true }),
        Some(e) => json!({ "ok": false, "error": e }),
    }))
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

#[cfg(test)]
mod tests {
    use super::*;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn stats_count_leads_per_mailbox_over_the_last_30_days(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mailbox = |email: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.test', '', '') returning id",
            )
            .bind(org)
            .bind(email)
            .fetch_one(&db)
        };
        let (busy, idle) = (mailbox("a@acme.test").await.unwrap(), mailbox("b@acme.test").await.unwrap());
        let leads: Vec<Uuid> = sqlx::query_scalar("insert into leads (project_id, full_name) select $1, 'Lead ' || g from generate_series(1, 4) g returning id")
            .bind(project)
            .fetch_all(&db)
            .await
            .unwrap();
        let message = |lead: Uuid, direction: &'static str, status: &'static str, classification: Option<&'static str>, days_ago: i32| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, classification, sent_at, created_at)
                 values ($1, $2, $3, $4, case when $4 = 'inbound' then 'inbound' else 'initial' end, $5, $6,
                         case when $5 = 'sent' then now() - make_interval(days => $7) end, now() - make_interval(days => $7))",
            )
            .bind(project)
            .bind(lead)
            .bind(busy)
            .bind(direction)
            .bind(status)
            .bind(classification)
            .bind(days_ago)
            .execute(&db)
        };
        // Lead 0 got two emails and replied twice, lead 1 bounced, lead 2 was emailed 40 days ago and lead 3 only has a draft.
        // The idle mailbox only received a reply, to an email older than the window.
        message(leads[0], "outbound", "sent", None, 3).await.unwrap();
        message(leads[0], "outbound", "sent", None, 1).await.unwrap();
        message(leads[0], "inbound", "received", Some("question"), 1).await.unwrap();
        message(leads[0], "inbound", "received", None, 0).await.unwrap();
        message(leads[1], "outbound", "sent", None, 2).await.unwrap();
        message(leads[1], "inbound", "received", Some("bounce"), 2).await.unwrap();
        message(leads[2], "outbound", "sent", None, 40).await.unwrap();
        message(leads[3], "outbound", "draft", None, 0).await.unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status) values ($1, $2, $3, 'inbound', 'inbound', 'received')")
            .bind(project)
            .bind(leads[2])
            .bind(idle)
            .execute(&db)
            .await
            .unwrap();

        let stats = org_stats(&db, org).await.unwrap();
        assert_eq!(stats.len(), 1, "the mailbox that sent nothing is left out");
        let s = &stats[0];
        assert_eq!((s.mailbox_id, s.emailed, s.bounced, s.replied), (busy, 2, 1, 1));
        assert_ne!(s.mailbox_id, idle);
    }
}
