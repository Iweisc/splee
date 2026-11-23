use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::audit::{self, Target};
use crate::auth::Auth;
use crate::crypto::encrypt;
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{Mailbox, MailboxStats, WarmupHealth};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/mailboxes", get(list).post(create))
        .route("/mailboxes/stats", get(stats))
        .route("/mailboxes/settings", get(settings))
        .route("/mailboxes/warmup", get(warmup))
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

/// How warming up is going for each of the org's mailboxes with warmup on. Only their own counts: the mailboxes they
/// trade emails with may belong to anyone on this Splee.
async fn warmup(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<WarmupHealth>>> {
    Ok(Json(mail::warmup::health(&state.db, auth.org_id).await?))
}

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
    min_gap_minutes: Option<i32>,
    /// Only for a provider that signs with a selector of its own. Empty clears it.
    dkim_selector: Option<String>,
    warmup: Option<bool>,
    warmup_daily_cap: Option<i32>,
    total_daily_limit: Option<i32>,
    hold_until_warm: Option<bool>,
    /// Whose mailbox it is, or a present null for nobody. A new one is whoever connects it, unless this says otherwise.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    owner_id: Option<Option<Uuid>>,
    /// Another of the org's mailboxes that replies should go to, or a present null for this one. Only on a PATCH.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    reply_to_mailbox_id: Option<Option<Uuid>>,
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
        if self.min_gap_minutes.is_some_and(|g| !(0..=60).contains(&g)) {
            return Err(AppError::BadRequest("Minutes between emails must be between 0 and 60".into()));
        }
        if self.warmup_daily_cap.is_some_and(|c| !(1..=200).contains(&c)) {
            return Err(AppError::BadRequest("Warmup emails a day must be between 1 and 200".into()));
        }
        if self.total_daily_limit.is_some_and(|l| !(1..=5000).contains(&l)) {
            return Err(AppError::BadRequest("Total emails a day must be between 1 and 5000".into()));
        }
        if self.cost_per_email_micros.is_some_and(|c| !(0..=1_000_000).contains(&c)) {
            return Err(AppError::BadRequest("Cost per email must be between $0 and $1".into()));
        }
        match self.email.as_deref() {
            Some(raw) => Ok(Some(util_input::email(raw).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?)),
            None => Ok(None),
        }
    }

    fn without_server_settings(self) -> Self {
        Self {
            email: None,
            smtp_host: None,
            smtp_port: None,
            smtp_security: None,
            smtp_username: None,
            smtp_password: None,
            imap_host: None,
            imap_port: None,
            imap_username: None,
            imap_password: None,
            ..self
        }
    }

    /// The DKIM selector as it goes into `{selector}._domainkey.{domain}`. A pasted "s1._domainkey.acme.com" is cut back to
    /// "s1".
    fn dkim_selector(&self) -> AppResult<Option<String>> {
        let Some(raw) = &self.dkim_selector else { return Ok(None) };
        let lower = raw.trim().to_lowercase();
        let selector = lower.split("._domainkey").next().unwrap_or_default().trim_end_matches('.');
        let label_ok = |l: &str| (1..=63).contains(&l.len()) && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        let valid = selector.is_empty() || (selector.len() <= 200 && selector.split('.').all(label_ok));
        if !valid {
            return Err(AppError::BadRequest("A DKIM selector has only letters, digits, dashes, underscores and dots, like s1 or k2".into()));
        }
        Ok(Some(selector.to_string()))
    }
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<MailboxBody>) -> AppResult<Json<Mailbox>> {
    auth.require_admin()?;
    let owner = body.owner_id.unwrap_or(Some(auth.user_id));
    crate::ownership::check_member(&state.db, auth.org_id, owner).await?;
    let email = body.validate()?.ok_or_else(|| AppError::BadRequest("Email is required".into()))?;
    let dkim_selector = body.dkim_selector()?;
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
           imap_host, imap_port, imap_username, imap_password_enc, daily_limit, status, cost_per_email_micros, ramp_up, signature, booking_link,
           dkim_selector, warmup, warmup_daily_cap, total_daily_limit, hold_until_warm, owner_id, min_gap_minutes)
         values ($1, $2, coalesce($3, ''), $4, coalesce($5, 587), coalesce($6, 'starttls'), $7, $8,
           $9, coalesce($10, 993), $11, $12, coalesce($13, 30), coalesce($14, 'active'), coalesce($15, 0), coalesce($16, true), coalesce($17, ''), coalesce($18, ''),
           coalesce($19, ''), coalesce($20, true), coalesce($21, 40), coalesce($22, 80), coalesce($23, true), $24, coalesce($25, 0))
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
    .bind(dkim_selector)
    .bind(body.warmup)
    .bind(body.warmup_daily_cap)
    .bind(body.total_daily_limit)
    .bind(body.hold_until_warm)
    .bind(owner)
    .bind(body.min_gap_minutes)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match AppError::from(e) {
        AppError::Conflict(_) => AppError::Conflict("This mailbox is already connected".into()),
        other => other,
    })?;
    audit::record(&state.db, &auth, "mailbox.connected", Target::new("mailbox", mailbox.id, &mailbox.email), json!({ "provider": "smtp" })).await;
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
    // The address and servers of a mailbox connected by signing in come from its provider, and it has no passwords.
    let body = if mailbox.provider == "password" { body } else { body.without_server_settings() };
    let email = body.validate()?;
    let dkim_selector = body.dkim_selector()?;
    if let Some(owner) = body.owner_id {
        crate::ownership::check_member(&state.db, auth.org_id, owner).await?;
    }
    // Replies have to land in a mailbox Splee reads, or it can't stop the sequence or show them in the inbox.
    if let Some(Some(target)) = body.reply_to_mailbox_id {
        let theirs: bool = sqlx::query_scalar("select exists (select 1 from mailboxes where id = $1 and org_id = $2 and imap_host <> '')")
            .bind(target)
            .bind(auth.org_id)
            .fetch_one(&state.db)
            .await?;
        if !theirs || target == mailbox.id {
            return Err(AppError::BadRequest("Replies can go to another of your mailboxes that Splee reads".into()));
        }
    }
    let secret = &state.cfg.app_secret;
    let encrypt_new = |p: &Option<String>| p.as_deref().filter(|p| !p.is_empty()).map(|p| encrypt(secret, p));
    // Resuming a mailbox the Deliverability check held sends through the problems it has now. Only a new one holds it again.
    let resumed_through = match (body.status.as_deref(), mailbox.health_held_at) {
        (Some("active"), Some(_)) => Some(crate::deliverability::at_risk_now(&state.db, auth.org_id, mailbox.id).await?),
        _ => None,
    };
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
           booking_link = coalesce($18, booking_link),
           dkim_selector = coalesce($19, dkim_selector),
           warmup = coalesce($20, warmup),
           -- Turned off and on again, warmup ramps up from the start.
           warmup_started_on = case when $20 = false then null else warmup_started_on end,
           warmup_next_at = case when $20 = false then null else warmup_next_at end,
           warmup_daily_cap = coalesce($21, warmup_daily_cap),
           total_daily_limit = coalesce($22, total_daily_limit),
           hold_until_warm = coalesce($23, hold_until_warm),
           health_ack = coalesce($24, health_ack),
           health_held_at = case when $14 = 'active' then null else health_held_at end,
           owner_id = case when $25 then $26 else owner_id end,
           min_gap_minutes = coalesce($27, min_gap_minutes),
           reply_to_mailbox_id = case when $28 then $29 else reply_to_mailbox_id end
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
    .bind(dkim_selector)
    .bind(body.warmup)
    .bind(body.warmup_daily_cap)
    .bind(body.total_daily_limit)
    .bind(body.hold_until_warm)
    .bind(resumed_through)
    .bind(body.owner_id.is_some())
    .bind(body.owner_id.flatten())
    .bind(body.min_gap_minutes)
    .bind(body.reply_to_mailbox_id.is_some())
    .bind(body.reply_to_mailbox_id.flatten())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(mailbox))
}

#[derive(Deserialize)]
struct SettingsQuery {
    email: String,
}

/// The SMTP and IMAP settings for an address, from the provider its domain's mail goes to, or null for one Splee
/// doesn't recognize. The mailbox form fills them in once the address is typed.
async fn settings(State(_): State<AppState>, _: Auth, Query(q): Query<SettingsQuery>) -> AppResult<Json<Option<mail::providers::Settings>>> {
    let domain = q.email.trim().rsplit_once('@').map(|(_, d)| d.trim_end_matches('.')).filter(|d| d.contains('.'));
    let Some(domain) = domain else { return Ok(Json(None)) };
    let resolver = mail::dns::resolver().map_err(AppError::Internal)?;
    Ok(Json(mail::providers::from_mx(&mail::dns::mx_exchanges(&resolver, domain).await)))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let mailbox = load(&state.db, id, auth.org_id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update project_settings set mailbox_ids = array_remove(mailbox_ids, $1) where $1 = any(mailbox_ids)")
        .bind(mailbox.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update segments set mailbox_ids = array_remove(mailbox_ids, $1) where $1 = any(mailbox_ids)")
        .bind(mailbox.id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from mailboxes where id = $1").bind(mailbox.id).execute(&mut *tx).await?;
    tx.commit().await?;
    audit::record(&state.db, &auth, "mailbox.removed", Target::new("mailbox", mailbox.id, &mailbox.email), json!({})).await;
    Ok(Json(json!({ "ok": true })))
}

async fn test(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let mailbox = load(&state.db, id, auth.org_id).await?;
    let result = mail::test_mailbox(&state, &mailbox).await;
    // A passing test clears an error state. Either way a person has now seen the result, so the sender stops testing it
    // on its own. A test never changes a paused mailbox: only Resume does, and its last_error keeps why it's paused. The
    // status comes from the row as it is now, since a bounce or Resume can change it while the test runs.
    let error = result.as_ref().err().map(|e| format!("{e:#}"));
    sqlx::query(
        "update mailboxes set status = case when $2::text is not null then 'error' when status = 'error' then 'active' else status end,
           last_error = coalesce($2, ''), recheck_at = null
         where id = $1 and status <> 'paused'",
    )
    .bind(mailbox.id)
    .bind(&error)
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
    let selector = Some(mailbox.dkim_selector.as_str()).filter(|s| !s.is_empty());
    let dns = mail::check_dns(domain, selector).await.map_err(|e| AppError::BadRequest(format!("DNS check failed: {e:#}")))?;
    let mailbox = sqlx::query_as::<_, Mailbox>("update mailboxes set dns = $2 where id = $1 returning *")
        .bind(mailbox.id)
        .bind(dns)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(mailbox))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::Config;
    use crate::crypto::decrypt;

    struct Seeded {
        state: AppState,
        org: Uuid,
        other_org: Uuid,
        /// The org's admin, and a member who isn't one.
        admin: Uuid,
        member: Uuid,
    }

    async fn seed(db: PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let other_org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
        let mut users = Vec::new();
        for (email, role) in [("dana@acme.example", "admin"), ("sam@acme.example", "member")] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ($1, $1, 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, $3)").bind(org).bind(user).bind(role).execute(&db).await.unwrap();
            users.push(user);
        }
        let cfg = Config { app_secret: "0123456789abcdef0123456789abcdef".into(), ..Config::test() };
        let state = AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Seeded { state, org, other_org, admin: users[0], member: users[1] }
    }

    fn as_user(user: Uuid, org_id: Uuid, role: &str) -> Auth {
        Auth { user_id: user, org_id, role: role.into(), token_hash: String::new() }
    }

    fn body(value: Value) -> Json<MailboxBody> {
        Json(serde_json::from_value(value).unwrap())
    }

    async fn connect(s: &Seeded, value: Value) -> AppResult<Mailbox> {
        create(State(s.state.clone()), as_user(s.admin, s.org, "admin"), body(value)).await.map(|Json(m)| m)
    }

    async fn edit(s: &Seeded, id: Uuid, value: Value) -> AppResult<Mailbox> {
        update(State(s.state.clone()), as_user(s.admin, s.org, "admin"), Path(id), body(value)).await.map(|Json(m)| m)
    }

    async fn saved(s: &Seeded, id: Uuid) -> Value {
        let mailbox: Mailbox = sqlx::query_as("select * from mailboxes where id = $1").bind(id).fetch_one(&s.state.db).await.unwrap();
        // The passwords aren't serialized, so they're added to tell a change apart.
        let mut value = serde_json::to_value(&mailbox).unwrap();
        value["smtp_password_enc"] = json!(mailbox.smtp_password_enc);
        value["imap_password_enc"] = json!(mailbox.imap_password_enc);
        value
    }

    fn message<T: std::fmt::Debug>(result: AppResult<T>) -> String {
        match result {
            Err(AppError::BadRequest(m) | AppError::Conflict(m)) => m,
            other => panic!("{other:?}"),
        }
    }

    const MINIMAL: &str = r#"{ "email": " Dana@Acme.example ", "smtp_host": " smtp.acme.example ", "smtp_password": "smtp-secret" }"#;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn connecting_a_mailbox_refuses_bad_settings_and_fills_in_the_rest(db: PgPool) {
        let s = seed(db).await;
        let good: Value = serde_json::from_str(MINIMAL).unwrap();
        let with = |change: Value| {
            let mut value = good.clone();
            value.as_object_mut().unwrap().extend(change.as_object().unwrap().clone());
            value
        };
        let refused = [
            (json!({ "smtp_host": "smtp.acme.example", "smtp_password": "x" }), "Email is required"),
            (with(json!({ "email": "not an address" })), "Enter a valid email address"),
            (with(json!({ "smtp_host": "  " })), "SMTP host is required"),
            (with(json!({ "smtp_password": "" })), "SMTP password is required"),
            (with(json!({ "smtp_security": "ssl" })), "smtp_security must be one of: tls, starttls, none"),
            (with(json!({ "status": "error" })), "status must be one of: active, paused"),
            (with(json!({ "smtp_port": 0 })), "Ports must be between 1 and 65535"),
            (with(json!({ "imap_port": 65536 })), "Ports must be between 1 and 65535"),
            (with(json!({ "daily_limit": 2001 })), "Daily limit must be between 1 and 2000"),
            (with(json!({ "booking_link": "cal.example/dana" })), "Booking link must be a full URL starting with https://"),
            (with(json!({ "booking_link": "ftp://cal.example/dana" })), "Booking link must be a full URL starting with https://"),
            (with(json!({ "min_gap_minutes": 61 })), "Minutes between emails must be between 0 and 60"),
            (with(json!({ "warmup_daily_cap": 0 })), "Warmup emails a day must be between 1 and 200"),
            (with(json!({ "total_daily_limit": 5001 })), "Total emails a day must be between 1 and 5000"),
            (with(json!({ "cost_per_email_micros": 1_000_001 })), "Cost per email must be between $0 and $1"),
            (with(json!({ "dkim_selector": "s1 k2" })), "A DKIM selector has only letters, digits, dashes, underscores and dots, like s1 or k2"),
            (with(json!({ "owner_id": Uuid::new_v4() })), "The owner has to be on your team"),
        ];
        for (value, want) in refused {
            assert_eq!(message(connect(&s, value.clone()).await), want, "{value}");
        }
        assert!(matches!(create(State(s.state.clone()), as_user(s.member, s.org, "member"), body(good.clone())).await, Err(AppError::Forbidden(_))));
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from mailboxes").fetch_one(&s.state.db).await.unwrap(), 0);

        // Left out, a mailbox sends over STARTTLS on 587 with its address as the login, reads no inbox, and belongs to who
        // connected it.
        let plain = connect(&s, good.clone()).await.unwrap();
        assert_eq!((plain.email.as_str(), plain.smtp_host.as_str(), plain.smtp_port, plain.smtp_security.as_str()), ("dana@acme.example", "smtp.acme.example", 587, "starttls"));
        assert_eq!((plain.smtp_username.as_str(), plain.imap_host.as_str(), plain.imap_username.as_str(), plain.imap_password_enc.as_str()), ("dana@acme.example", "", "", ""));
        assert_eq!((plain.daily_limit, plain.status.as_str(), plain.owner_id, plain.provider.as_str()), (30, "active", Some(s.admin), "password"));
        assert_eq!(decrypt(&s.state.cfg.app_secret, &plain.smtp_password_enc).unwrap(), "smtp-secret");
        assert_ne!(plain.smtp_password_enc, "smtp-secret");
        assert_eq!(message(connect(&s, with(json!({ "email": "DANA@acme.example" }))).await), "This mailbox is already connected");

        // An inbox with no login of its own reads with the SMTP one. A pasted DKIM record name is cut to its selector.
        let inbox = connect(&s, with(json!({ "email": "sam@acme.example", "smtp_username": "sam-login", "imap_host": "imap.acme.example", "dkim_selector": "S1._domainkey.acme.example", "owner_id": s.member })))
            .await
            .unwrap();
        assert_eq!((inbox.imap_host.as_str(), inbox.imap_port, inbox.imap_username.as_str(), inbox.dkim_selector.as_str()), ("imap.acme.example", 993, "sam-login", "s1"));
        assert_eq!((decrypt(&s.state.cfg.app_secret, &inbox.imap_password_enc).unwrap(), inbox.owner_id), ("smtp-secret".to_string(), Some(s.member)));
        let own = connect(&s, with(json!({ "email": "ops@acme.example", "imap_host": "imap.acme.example", "imap_username": "ops-imap", "imap_password": "imap-secret", "owner_id": null })))
            .await
            .unwrap();
        assert_eq!((own.imap_username.as_str(), decrypt(&s.state.cfg.app_secret, &own.imap_password_enc).unwrap(), own.owner_id), ("ops-imap", "imap-secret".to_string(), None));

        let ends = [
            ("high@acme.example", 65535, 2000, 60, 200, 5000, 1_000_000),
            ("low@acme.example", 1, 1, 0, 1, 1, 0),
        ];
        for (email, port, daily, gap, warmup, total, cost) in ends {
            let value = with(json!({ "email": email, "smtp_port": port, "imap_host": "imap.acme.example", "imap_port": port, "daily_limit": daily, "min_gap_minutes": gap,
                                     "warmup_daily_cap": warmup, "total_daily_limit": total, "cost_per_email_micros": cost, "booking_link": "http://cal.example/edge" }));
            let m = connect(&s, value).await.unwrap();
            assert_eq!(
                (m.smtp_port, m.imap_port, m.daily_limit, m.min_gap_minutes, m.warmup_daily_cap, m.total_daily_limit, m.cost_per_email_micros),
                (port, port, daily, gap, warmup, total, cost)
            );
        }

        let audit: Vec<String> = sqlx::query_scalar("select target_label from audit_events where action = 'mailbox.connected' order by target_label").fetch_all(&s.state.db).await.unwrap();
        assert_eq!(audit, ["dana@acme.example", "high@acme.example", "low@acme.example", "ops@acme.example", "sam@acme.example"]);
        let Json(listed) = list(State(s.state.clone()), as_user(s.member, s.org, "member")).await.unwrap();
        assert_eq!(listed.len(), 5);
        let Json(theirs) = list(State(s.state.clone()), as_user(s.admin, s.other_org, "admin")).await.unwrap();
        assert!(theirs.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn editing_a_mailbox_checks_whats_sent_and_starts_over_what_changed(db: PgPool) {
        let s = seed(db).await;
        let mailbox = connect(&s, json!({ "email": "dana@acme.example", "smtp_host": "smtp.acme.example", "smtp_password": "smtp-secret", "imap_host": "imap.acme.example", "dkim_selector": "s1" }))
            .await
            .unwrap();
        let reader = connect(&s, json!({ "email": "inbox@acme.example", "smtp_host": "smtp.acme.example", "smtp_password": "x", "imap_host": "imap.acme.example" }))
            .await
            .unwrap();
        let sender_only = connect(&s, json!({ "email": "bulk@acme.example", "smtp_host": "smtp.acme.example", "smtp_password": "x" })).await.unwrap();
        let elsewhere: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, imap_host) values ($1, 'inbox@other.example', 'smtp.other.example', '', '', 'imap.other.example') returning id",
        )
        .bind(s.other_org)
        .fetch_one(&s.state.db)
        .await
        .unwrap();
        let outsider: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('x@other.example', 'X', 'x') returning id").fetch_one(&s.state.db).await.unwrap();
        let before = saved(&s, mailbox.id).await;

        let refused = [
            (json!({ "daily_limit": 0 }), "Daily limit must be between 1 and 2000"),
            (json!({ "email": "nope" }), "Enter a valid email address"),
            (json!({ "owner_id": outsider }), "The owner has to be on your team"),
            (json!({ "reply_to_mailbox_id": mailbox.id }), "Replies can go to another of your mailboxes that Splee reads"),
            (json!({ "reply_to_mailbox_id": sender_only.id }), "Replies can go to another of your mailboxes that Splee reads"),
            (json!({ "reply_to_mailbox_id": Uuid::new_v4() }), "Replies can go to another of your mailboxes that Splee reads"),
            (json!({ "reply_to_mailbox_id": elsewhere }), "Replies can go to another of your mailboxes that Splee reads"),
        ];
        for (value, want) in refused {
            assert_eq!(message(edit(&s, mailbox.id, value.clone()).await), want, "{value}");
            assert_eq!(saved(&s, mailbox.id).await, before, "{value}");
        }
        assert!(matches!(update(State(s.state.clone()), as_user(s.member, s.org, "member"), Path(mailbox.id), body(json!({ "daily_limit": 5 }))).await, Err(AppError::Forbidden(_))));
        assert!(matches!(update(State(s.state.clone()), as_user(s.admin, s.other_org, "admin"), Path(mailbox.id), body(json!({ "daily_limit": 5 }))).await, Err(AppError::NotFound)));
        assert_eq!(saved(&s, mailbox.id).await, before);

        // A blank password keeps the saved one, a new one replaces it, and reply-to and owner can be set and cleared.
        let kept = edit(&s, mailbox.id, json!({ "smtp_password": "", "from_name": "  Dana Reyes ", "reply_to_mailbox_id": reader.id, "owner_id": s.member })).await.unwrap();
        assert_eq!((kept.smtp_password_enc.as_str(), kept.from_name.as_str(), kept.reply_to_mailbox_id, kept.owner_id), (mailbox.smtp_password_enc.as_str(), "Dana Reyes", Some(reader.id), Some(s.member)));
        let changed = edit(&s, mailbox.id, json!({ "smtp_password": "new-secret", "reply_to_mailbox_id": null, "owner_id": null })).await.unwrap();
        assert_eq!((decrypt(&s.state.cfg.app_secret, &changed.smtp_password_enc).unwrap(), changed.reply_to_mailbox_id, changed.owner_id), ("new-secret".to_string(), None, None));
        assert_eq!(saved(&s, mailbox.id).await["dkim_selector"], "s1");
        assert_eq!(edit(&s, mailbox.id, json!({ "dkim_selector": "" })).await.unwrap().dkim_selector, "", "empty clears it");

        // Another IMAP server starts reading from the top. The same one keeps its place.
        sqlx::query("update mailboxes set imap_last_uid = 812, imap_uid_validity = 7 where id = $1").bind(mailbox.id).execute(&s.state.db).await.unwrap();
        let same = edit(&s, mailbox.id, json!({ "imap_host": " imap.acme.example " })).await.unwrap();
        assert_eq!((same.imap_last_uid, same.imap_uid_validity), (812, Some(7)));
        let moved = edit(&s, mailbox.id, json!({ "imap_host": "imap.newhost.example" })).await.unwrap();
        assert_eq!((moved.imap_host.as_str(), moved.imap_last_uid, moved.imap_uid_validity), ("imap.newhost.example", -1, None));

        // Warmup turned off starts from scratch when it's turned on again.
        let warming: (Option<chrono::NaiveDate>, Option<crate::models::Ts>) =
            sqlx::query_as("update mailboxes set warmup = true, warmup_started_on = current_date - 9, warmup_next_at = now() where id = $1 returning warmup_started_on, warmup_next_at")
                .bind(mailbox.id)
                .fetch_one(&s.state.db)
                .await
                .unwrap();
        let still = edit(&s, mailbox.id, json!({ "warmup": true })).await.unwrap();
        assert_eq!((still.warmup_started_on, still.warmup_next_at), warming, "left on, it keeps its place in the ramp");
        let off = edit(&s, mailbox.id, json!({ "warmup": false })).await.unwrap();
        assert_eq!((off.warmup, off.warmup_started_on, off.warmup_next_at), (false, None, None));
        let on = edit(&s, mailbox.id, json!({ "warmup": true })).await.unwrap();
        assert_eq!((on.warmup, on.warmup_started_on, on.warmup_next_at), (true, None, None), "the warmup worker starts it again from day one");

        // Resuming a mailbox clears why it was paused, and the Deliverability check's hold.
        sqlx::query("update mailboxes set status = 'paused', last_error = 'Bounce rate 9%', health_held_at = now() where id = $1").bind(mailbox.id).execute(&s.state.db).await.unwrap();
        let still_paused = edit(&s, mailbox.id, json!({ "daily_limit": 10 })).await.unwrap();
        assert_eq!((still_paused.status.as_str(), still_paused.last_error.as_str(), still_paused.health_held_at.is_some()), ("paused", "Bounce rate 9%", true));
        let resumed = edit(&s, mailbox.id, json!({ "status": "active" })).await.unwrap();
        assert_eq!((resumed.status.as_str(), resumed.last_error.as_str(), resumed.health_held_at), ("active", "", None));
        let ack: Vec<String> = sqlx::query_scalar("select health_ack from mailboxes where id = $1").bind(mailbox.id).fetch_one(&s.state.db).await.unwrap();
        assert!(ack.is_empty(), "{ack:?}: nothing is at risk on a mailbox that hasn't sent");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_signed_in_mailbox_keeps_its_address_and_servers(db: PgPool) {
        let s = seed(db).await;
        let google: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, provider) values ($1, 'dana@gmail.example', 'smtp.gmail.com', '', '', 'google') returning id",
        )
        .bind(s.org)
        .fetch_one(&s.state.db)
        .await
        .unwrap();
        let edited = edit(&s, google, json!({ "email": "other@gmail.example", "smtp_host": "evil.example", "smtp_password": "x", "imap_host": "imap.evil.example", "daily_limit": 12 }))
            .await
            .unwrap();
        assert_eq!((edited.email.as_str(), edited.smtp_host.as_str(), edited.imap_host.as_str(), edited.smtp_password_enc.as_str()), ("dana@gmail.example", "smtp.gmail.com", "", ""));
        assert_eq!(edited.daily_limit, 12, "the rest is saved");
        // A bad address is ignored with the rest of the server settings rather than refused.
        assert!(edit(&s, google, json!({ "email": "not an address" })).await.is_ok());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn removing_a_mailbox_takes_it_out_of_each_projects_send_from_pick(db: PgPool) {
        let s = seed(db).await;
        let base = |email: &str| json!({ "email": email, "smtp_host": "smtp.acme.example", "smtp_password": "x" });
        let (gone, kept) = (connect(&s, base("gone@acme.example")).await.unwrap(), connect(&s, base("kept@acme.example")).await.unwrap());
        let mut projects = Vec::new();
        for picked in [Some(vec![gone.id, kept.id]), Some(vec![kept.id]), Some(vec![gone.id]), None] {
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'P') returning id").bind(s.org).fetch_one(&s.state.db).await.unwrap();
            sqlx::query("insert into project_settings (project_id, mailbox_ids) values ($1, $2)").bind(project).bind(picked).execute(&s.state.db).await.unwrap();
            projects.push(project);
        }
        let campaign: Uuid = sqlx::query_scalar("insert into segments (project_id, name, mailbox_ids) values ($1, 'C', $2) returning id")
            .bind(projects[0])
            .bind(vec![gone.id, kept.id])
            .fetch_one(&s.state.db)
            .await
            .unwrap();

        assert!(matches!(remove(State(s.state.clone()), as_user(s.member, s.org, "member"), Path(gone.id)).await, Err(AppError::Forbidden(_))));
        assert!(matches!(remove(State(s.state.clone()), as_user(s.admin, s.other_org, "admin"), Path(gone.id)).await, Err(AppError::NotFound)));
        assert_eq!(remove(State(s.state.clone()), as_user(s.admin, s.org, "admin"), Path(gone.id)).await.unwrap().0, json!({ "ok": true }));

        let mut picks = Vec::new();
        for project in &projects {
            let ids: Option<Vec<Uuid>> = sqlx::query_scalar("select mailbox_ids from project_settings where project_id = $1").bind(project).fetch_one(&s.state.db).await.unwrap();
            picks.push(ids);
        }
        // A project that picked only the removed mailbox is left with none picked, so its first emails wait until someone picks
        // another, rather than going out from mailboxes it chose not to use. All mailboxes (null) stays all.
        assert_eq!(picks, [Some(vec![kept.id]), Some(vec![kept.id]), Some(vec![]), None]);
        let own: Vec<Uuid> = sqlx::query_scalar("select mailbox_ids from segments where id = $1").bind(campaign).fetch_one(&s.state.db).await.unwrap();
        assert_eq!(own, [kept.id], "a campaign's own pick loses it too");
        let left: Vec<Uuid> = sqlx::query_scalar("select id from mailboxes").fetch_all(&s.state.db).await.unwrap();
        assert_eq!(left, [kept.id]);
        let audit: Vec<String> = sqlx::query_scalar("select target_label from audit_events where action = 'mailbox.removed'").fetch_all(&s.state.db).await.unwrap();
        assert_eq!(audit, ["gone@acme.example"]);
    }

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
