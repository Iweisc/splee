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
use crate::verify::Verifier;

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
    webhook_url: Option<String>,
    slack_webhook_url: Option<String>,
    webhook_events: Option<Vec<String>>,
    chat_weekly_report: Option<bool>,
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
           webhook_url = coalesce($20, webhook_url),
           slack_webhook_url = coalesce($21, slack_webhook_url),
           webhook_events = coalesce($22, webhook_events),
           chat_weekly_report = coalesce($23, chat_weekly_report),
           -- The right-hand sides see the old row, so a new URL drops the old URL's failure.
           webhook_error = case when coalesce($20, webhook_url) = webhook_url then webhook_error end,
           webhook_failed_at = case when coalesce($20, webhook_url) = webhook_url then webhook_failed_at end,
           slack_error = case when coalesce($21, slack_webhook_url) = slack_webhook_url then slack_error end,
           slack_failed_at = case when coalesce($21, slack_webhook_url) = slack_webhook_url then slack_failed_at end
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
    .bind(body.webhook_url.as_deref().map(str::trim))
    .bind(body.slack_webhook_url.as_deref().map(str::trim))
    .bind(body.webhook_events)
    .bind(body.chat_weekly_report)
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
    let alert = if body.target == "webhook" { hooks::Alert::Webhook } else { hooks::Alert::Slack };
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

/// Posts a past delivery's payload again, signed afresh, to the webhook URL saved now. It's logged as a new delivery.
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
    let settings = load_or_create(&state.db, id).await?;
    if settings.webhook_url.trim().is_empty() {
        return Err(AppError::BadRequest("Add a webhook URL first".into()));
    }
    let result = hooks::post_json(&settings.webhook_url, &past.payload, Some(&settings.webhook_secret)).await;
    hooks::record(&state.db, id, hooks::Alert::Webhook, &settings.webhook_url, &result).await;
    Ok(Json(hooks::log_delivery(&state.db, id, &past.event, &past.payload, &result).await?))
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
    crm.check(token).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that token: {e:#}", crm.label())))?;
    set_crm_token(&state.db, id, crm, &crypto::encrypt(&state.cfg.app_secret, token)).await
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

#[derive(Deserialize)]
struct VerifierBody {
    provider: String,
    key: String,
}

/// Saves an email verification key once the service accepts it.
async fn connect_verifier(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<VerifierBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let verifier = Verifier::parse(&body.provider).ok_or_else(|| AppError::BadRequest("Pick NeverBounce, MillionVerifier, Hunter, Kickbox or Bouncer".into()))?;
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
