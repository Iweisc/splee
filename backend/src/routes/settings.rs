use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::crm::Crm;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{ProjectSettings, WebhookDelivery};
use crate::state::AppState;
use crate::unipile;
use crate::verify::{Finder, Verifier};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/settings", get(get_settings).put(update_settings))
        .route("/projects/{id}/integrations/test", post(test_integration))
        .route("/projects/{id}/integrations/webhook-secret", post(replace_webhook_secret))
        .route("/projects/{id}/integrations/webhook-deliveries", get(list_deliveries))
        .route("/projects/{id}/integrations/webhook-deliveries/{delivery_id}/resend", post(resend_delivery))
        .route("/projects/{id}/integrations/{crm}", put(connect_crm).delete(disconnect_crm))
        // Static, so it wins over {crm} above.
        .route("/projects/{id}/integrations/verifier", put(connect_verifier).delete(disconnect_verifier))
        .route("/projects/{id}/integrations/finder", put(connect_finder).delete(disconnect_finder))
        .route("/projects/{id}/integrations/unipile", put(connect_unipile).delete(disconnect_unipile))
}

/// Settings rows are created with the project, but older demo projects may not have one yet.
pub async fn load_or_create(db: &PgPool, project_id: Uuid) -> AppResult<ProjectSettings> {
    Ok(sqlx::query_as::<_, ProjectSettings>(
        "insert into project_settings (project_id) values ($1)
         on conflict (project_id) do update set project_id = excluded.project_id returning *",
    )
    .bind(project_id)
    .fetch_one(db)
    .await?)
}

async fn get_settings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    Ok(Json(load_or_create(&state.db, id).await?))
}

// `paused` is left out on purpose: unpausing goes through POST /projects/:id/start, which checks for an active mailbox.
#[derive(Deserialize)]
struct SettingsBody {
    daily_email_limit: Option<i32>,
    timezone: Option<String>,
    send_days: Option<Vec<i32>>,
    send_start_hour: Option<i32>,
    send_end_hour: Option<i32>,
    follow_ups: Option<Value>,
    booking_link: Option<String>,
    auto_reply_mode: Option<String>,
    auto_reply_instructions: Option<String>,
    sender_name: Option<String>,
    signature: Option<String>,
    tone: Option<String>,
    email_language: Option<String>,
    email_instructions: Option<String>,
    cta: Option<String>,
    max_words: Option<i32>,
    channels: Option<Value>,
    targeting: Option<Value>,
    webhook_events: Option<Vec<String>>,
    chat_weekly_report: Option<bool>,
    /// Empty turns it off.
    bcc_email: Option<String>,
    autopilot: Option<bool>,
    autopilot_budget_micros: Option<i64>,
    autopilot_lead_cap: Option<i32>,
    autopilot_auto_send: Option<bool>,
}

async fn update_settings(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SettingsBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    let current = load_or_create(&state.db, id).await?;
    let bad = |m: &str| Err(AppError::BadRequest(m.into()));

    if body.daily_email_limit.is_some_and(|n| !(1..=5000).contains(&n)) {
        return bad("Daily email limit must be between 1 and 5,000");
    }
    if let Some(tz) = &body.timezone {
        let known: bool = sqlx::query_scalar("select exists(select 1 from pg_timezone_names where name = $1)")
            .bind(tz)
            .fetch_one(&state.db)
            .await?;
        if !known {
            return bad("Unknown timezone");
        }
    }
    if body.send_days.as_ref().is_some_and(|days| days.iter().any(|d| !(1..=7).contains(d))) {
        return bad("Send days must be between 1 (Monday) and 7 (Sunday)");
    }
    let start = body.send_start_hour.unwrap_or(current.send_start_hour);
    let end = body.send_end_hour.unwrap_or(current.send_end_hour);
    if !(0..=23).contains(&start) || !(1..=24).contains(&end) || start >= end {
        return bad("Send window must start before it ends, within 0-24h");
    }
    if body.follow_ups.as_ref().is_some_and(|f| !f.is_array()) {
        return bad("follow_ups must be a list");
    }
    if body.channels.as_ref().is_some_and(|c| !c.is_object()) || body.targeting.as_ref().is_some_and(|t| !t.is_object()) {
        return bad("channels and targeting must be objects");
    }
    if let Some(mode) = &body.auto_reply_mode {
        util_input::one_of("auto_reply_mode", mode, &["off", "approve", "auto"])?;
    }
    if body.max_words.is_some_and(|w| !(20..=500).contains(&w)) {
        return bad("Max words must be between 20 and 500");
    }
    for event in body.webhook_events.iter().flatten() {
        util_input::one_of("webhook_events", event, &hooks::EVENTS)?;
    }
    if body.autopilot_budget_micros.is_some_and(|m| !(0..=100_000_000_000).contains(&m)) {
        return bad("Autopilot budget must be between $0 and $100,000 a month");
    }
    if body.autopilot_lead_cap.is_some_and(|n| !(0..=100_000).contains(&n)) {
        return bad("Autopilot lead cap must be between 0 and 100,000 a month");
    }
    let bcc_email = match body.bcc_email.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        Some(raw) => match util_input::email(raw) {
            Some(email) => Some(email),
            None => return bad("The BCC address isn't a valid email address"),
        },
        None => None,
    };

    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set
           daily_email_limit = coalesce($2, daily_email_limit),
           timezone = coalesce($3, timezone),
           send_days = coalesce($4, send_days),
           send_start_hour = $5,
           send_end_hour = $6,
           follow_ups = coalesce($7, follow_ups),
           booking_link = coalesce($8, booking_link),
           auto_reply_mode = coalesce($9, auto_reply_mode),
           auto_reply_instructions = coalesce($10, auto_reply_instructions),
           sender_name = coalesce($11, sender_name),
           signature = coalesce($12, signature),
           tone = coalesce($13, tone),
           email_language = coalesce($14, email_language),
           email_instructions = coalesce($15, email_instructions),
           cta = coalesce($16, cta),
           max_words = coalesce($17, max_words),
           channels = coalesce($18, channels),
           targeting = coalesce($19, targeting),
           webhook_events = coalesce($20, webhook_events),
           chat_weekly_report = coalesce($21, chat_weekly_report),
           bcc_error = case when $22::text <> bcc_email then null else bcc_error end,
           bcc_failed_at = case when $22::text <> bcc_email then null else bcc_failed_at end,
           bcc_email = coalesce($22, bcc_email),
           autopilot = coalesce($23, autopilot),
           autopilot_budget_micros = coalesce($24, autopilot_budget_micros),
           autopilot_lead_cap = coalesce($25, autopilot_lead_cap),
           autopilot_auto_send = coalesce($26, autopilot_auto_send)
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(body.daily_email_limit)
    .bind(body.timezone)
    .bind(body.send_days)
    .bind(start)
    .bind(end)
    .bind(body.follow_ups)
    .bind(body.booking_link.as_deref().map(str::trim))
    .bind(body.auto_reply_mode)
    .bind(body.auto_reply_instructions)
    .bind(body.sender_name.as_deref().map(str::trim))
    .bind(body.signature)
    .bind(body.tone)
    .bind(body.email_language)
    .bind(body.email_instructions)
    .bind(body.cta)
    .bind(body.max_words)
    .bind(body.channels)
    .bind(body.targeting)
    .bind(body.webhook_events)
    .bind(body.chat_weekly_report)
    .bind(bcc_email)
    .bind(body.autopilot)
    .bind(body.autopilot_budget_micros)
    .bind(body.autopilot_lead_cap)
    .bind(body.autopilot_auto_send)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct TestBody {
    target: String,
    url: String,
}

/// Posts a sample alert to the URL in the form, saved or not, so a team can check the hookup without waiting for a hot lead.
async fn test_integration(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<TestBody>) -> AppResult<Json<Value>> {
    let project = auth.project(&state.db, id).await?;
    util_input::one_of("target", &body.target, &["webhook", "slack"])?;
    let name = if project.name.is_empty() { project.domain.clone().unwrap_or_default() } else { project.name.clone() };
    let link = format!("{}/app/p/{}/inbox", state.cfg.public_url, project.id);
    // Same shape as a hot_lead event from hooks::lead_event, so tools like Zapier can map fields from it.
    let payload = if body.target == "slack" {
        let summary = format!("hot-lead alerts for {name} will post in this channel.");
        hooks::chat_payload(&body.url, &hooks::ChatAlert { label: "Splee test", summary: &summary, quote: None, link: &link, link_text: "Open the inbox" })
    } else {
        json!({
            "event": "test",
            "project": {"id": project.id, "name": name},
            "lead": {
                "id": Uuid::nil(), "full_name": "Sarah Chen", "email": "sarah@northwind.example", "title": "Founder",
                "status": "hot", "company_name": "Northwind Studio", "company_domain": "northwind.example",
                "linkedin_url": "https://www.linkedin.com/in/sarah-chen-example", "x_url": "https://x.com/sarahchen_example",
            },
            "reply": {
                "id": Uuid::nil(), "subject": "Re: Northwind Studio x Splee", "body": "This is a test alert from Splee.",
                "classification": "hot", "reason": "Test",
            },
            "url": link,
        })
    };
    let target = if body.target == "slack" { hooks::chat_endpoint(&body.url) } else { body.url.clone() };
    let alert = if body.target == "webhook" { hooks::Alert::Webhook } else { hooks::Alert::Chat };
    let secret = if body.target == "webhook" { Some(load_or_create(&state.db, project.id).await?.webhook_secret) } else { None };
    hooks::post_json(&target, &payload, secret.as_deref()).await.map_err(|e| AppError::BadRequest(format!("The test didn't go through: {e:#}")))?;
    // A passing test on the saved URL clears its last failure, so fixing an endpoint and testing it is enough. A failed test only shows in the toast.
    hooks::record(&state.db, project.id, alert, &body.url, &Ok(())).await;
    Ok(Json(json!({ "ok": true })))
}

async fn list_deliveries(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<WebhookDelivery>>> {
    auth.project(&state.db, id).await?;
    let deliveries = sqlx::query_as::<_, WebhookDelivery>("select * from webhook_deliveries where project_id = $1 order by created_at desc")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(deliveries))
}

/// Posts a past delivery's payload again, signed afresh, to the endpoint it went to. It's logged as a new delivery.
async fn resend_delivery(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, delivery_id)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<WebhookDelivery>> {
    auth.project(&state.db, id).await?;
    let past = sqlx::query_as::<_, WebhookDelivery>("select * from webhook_deliveries where id = $1 and project_id = $2")
        .bind(delivery_id)
        .bind(id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let listed: bool = sqlx::query_scalar("select exists (select 1 from webhook_endpoints where project_id = $1 and url = $2)")
        .bind(id)
        .bind(&past.url)
        .fetch_one(&state.db)
        .await?;
    if !listed {
        return Err(AppError::BadRequest("That endpoint isn't in the list any more".into()));
    }
    let settings = load_or_create(&state.db, id).await?;
    let result = hooks::post_json(&past.url, &past.payload, Some(&settings.webhook_secret)).await;
    hooks::record(&state.db, id, hooks::Alert::Webhook, &past.url, &result).await;
    Ok(Json(hooks::log_delivery(&state.db, id, &past.event, &past.url, &past.payload, &result).await?))
}

/// For a secret that leaked. Requests signed with the old one stop verifying right away.
async fn replace_webhook_secret(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set webhook_secret = encode(gen_random_bytes(24), 'hex') where project_id = $1 returning *",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}

#[derive(Deserialize)]
struct CrmBody {
    token: String,
    /// For a CRM with Crm::account.
    #[serde(default)]
    account: String,
}

fn crm(name: &str) -> AppResult<Crm> {
    Crm::parse(name).ok_or(AppError::NotFound)
}

/// Saves a CRM token once the CRM accepts it, so a typo shows up here and not on the first hot lead.
async fn connect_crm(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<CrmBody>,
) -> AppResult<Json<ProjectSettings>> {
    let crm = crm(&name)?;
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} token", crm.label())));
    }
    let token = match crm.account() {
        Some(what) => {
            let account = body.account.trim();
            if account.is_empty() {
                return Err(AppError::BadRequest(format!("Paste your {} {what}", crm.label())));
            }
            format!("{account}\n{token}")
        }
        None => token.to_string(),
    };
    crm.check(&token).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that: {e:#}", crm.label())))?;
    set_crm_token(&state.db, id, crm, &crypto::encrypt(&state.cfg.app_secret, &token)).await
}

async fn disconnect_crm(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<ProjectSettings>> {
    let crm = crm(&name)?;
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_crm_token(&state.db, id, crm, "").await
}

async fn set_crm_token(db: &PgPool, id: Uuid, crm: Crm, enc: &str) -> AppResult<Json<ProjectSettings>> {
    // crm.name() is a fixed string, never user input.
    let n = crm.name();
    let settings = sqlx::query_as::<_, ProjectSettings>(&format!(
        "update project_settings set {n}_token_enc = $2, {n}_error = null, {n}_failed_at = null where project_id = $1 returning *"
    ))
    .bind(id)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

/// A service and its API key, for Email verification or Email finder.
#[derive(Deserialize)]
struct ServiceKey {
    provider: String,
    key: String,
}

/// Saves an email verification key once the service accepts it.
async fn connect_verifier(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<ServiceKey>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let verifier = Verifier::parse(&body.provider).ok_or_else(|| AppError::BadRequest("Pick a verification service".into()))?;
    let key = body.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} API key", verifier.label())));
    }
    verifier.check_key(key).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that key: {e:#}", verifier.label())))?;
    set_verifier(&state.db, id, &body.provider, &crypto::encrypt(&state.cfg.app_secret, key)).await
}

async fn disconnect_verifier(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_verifier(&state.db, id, "", "").await
}

/// Saves an email finder key once the service accepts it.
async fn connect_finder(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<ServiceKey>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let finder = Finder::parse(&body.provider).ok_or_else(|| AppError::BadRequest("Pick an email finder".into()))?;
    let key = body.key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} API key", finder.label())));
    }
    finder.check_key(key).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that key: {e:#}", finder.label())))?;
    set_finder(&state.db, id, &body.provider, &crypto::encrypt(&state.cfg.app_secret, key)).await
}

async fn disconnect_finder(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_finder(&state.db, id, "", "").await
}

#[derive(Deserialize)]
struct UnipileBody {
    dsn: String,
    token: String,
}

/// Saves a Unipile token with the LinkedIn account it opens, once Unipile accepts it.
async fn connect_unipile(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<UnipileBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let base = unipile::base_url(&body.dsn)
        .ok_or_else(|| AppError::BadRequest("Paste the DSN from your Unipile dashboard, like api8.unipile.com:13851".into()))?;
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste your Unipile access token".into()));
    }
    let (account, name) =
        unipile::find_account(&base, token).await.map_err(|e| AppError::BadRequest(format!("Unipile didn't accept that: {e:#}")))?;
    remove_unipile_webhooks(&state, id).await?;
    let hook_token: String =
        sqlx::query_scalar("select unipile_webhook_token from project_settings where project_id = $1").bind(id).fetch_one(&state.db).await?;
    let url = format!("{}/api/unipile/{hook_token}", state.cfg.public_url.trim_end_matches('/'));
    let hooks = unipile::add_webhooks(&base, token, &account, &url)
        .await
        .map_err(|e| AppError::BadRequest(format!("Unipile didn't take Splee's webhook for LinkedIn replies: {e:#}")))?;
    set_unipile(&state.db, id, &base, &crypto::encrypt(&state.cfg.app_secret, token), &account, &name, &hooks).await
}

async fn disconnect_unipile(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    remove_unipile_webhooks(&state, id).await?;
    set_unipile(&state.db, id, "", "", "", "", &[]).await
}

/// The webhooks a previous connection made, so a reconnect or disconnect doesn't leave Unipile posting to Splee.
async fn remove_unipile_webhooks(state: &AppState, id: Uuid) -> AppResult<()> {
    let (base, enc, ids): (String, String, Vec<String>) =
        sqlx::query_as("select unipile_dsn, unipile_token_enc, unipile_webhook_ids from project_settings where project_id = $1")
            .bind(id)
            .fetch_one(&state.db)
            .await?;
    if let (false, Ok(token)) = (ids.is_empty(), crypto::decrypt(&state.cfg.app_secret, &enc)) {
        unipile::remove_webhooks(&base, &token, &ids).await;
    }
    Ok(())
}

async fn set_unipile(db: &PgPool, id: Uuid, base: &str, enc: &str, account: &str, name: &str, hooks: &[String]) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set unipile_dsn = $2, unipile_token_enc = $3, unipile_account_id = $4, unipile_account_name = $5,
           unipile_webhook_ids = $6, unipile_error = null, unipile_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(base)
    .bind(enc)
    .bind(account)
    .bind(name)
    .bind(hooks)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

async fn set_finder(db: &PgPool, id: Uuid, provider: &str, enc: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set finder = $2, finder_key_enc = $3, finder_error = null, finder_failed_at = null where project_id = $1 returning *",
    )
    .bind(id)
    .bind(provider)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}

async fn set_verifier(db: &PgPool, id: Uuid, provider: &str, enc: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set verifier = $2, verifier_key_enc = $3, verifier_error = null, verifier_failed_at = null
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(provider)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}
