use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::hubspot;
use crate::models::ProjectSettings;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/settings", get(get_settings).put(update_settings))
        .route("/projects/{id}/integrations/test", post(test_integration))
        .route("/projects/{id}/integrations/webhook-secret", post(replace_webhook_secret))
        .route("/projects/{id}/integrations/hubspot", put(connect_hubspot).delete(disconnect_hubspot))
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
                "company_name": "Northwind Studio", "company_domain": "northwind.example",
                "linkedin_url": "https://www.linkedin.com/in/sarah-chen-example", "x_url": "https://x.com/sarahchen_example",
            },
            "reply": {
                "id": Uuid::nil(), "subject": "Re: Northwind Studio x Splee", "body": "This is a test alert from Splee.",
                "classification": "hot", "reason": "Test",
            },
            "url": link,
        })
    };
    let alert = if body.target == "webhook" { hooks::Alert::Webhook } else { hooks::Alert::Slack };
    let secret = if body.target == "webhook" { Some(load_or_create(&state.db, project.id).await?.webhook_secret) } else { None };
    hooks::post_json(&body.url, &payload, secret.as_deref()).await.map_err(|e| AppError::BadRequest(format!("The test didn't go through: {e:#}")))?;
    // A passing test on the saved URL clears its last failure, so fixing an endpoint and testing it is enough. A failed test only shows in the toast.
    hooks::record(&state.db, project.id, alert, &body.url, &Ok(())).await;
    Ok(Json(json!({ "ok": true })))
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
struct HubSpotBody {
    token: String,
}

/// Saves a HubSpot access token once HubSpot accepts it, so a typo shows up here and not on the first hot lead.
async fn connect_hubspot(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<HubSpotBody>,
) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let token = body.token.trim();
    if token.is_empty() {
        return Err(AppError::BadRequest("Paste your HubSpot access token".into()));
    }
    hubspot::check(token).await.map_err(|e| AppError::BadRequest(format!("HubSpot didn't accept that token: {e:#}")))?;
    set_hubspot_token(&state.db, id, &crypto::encrypt(&state.cfg.app_secret, token)).await
}

async fn disconnect_hubspot(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectSettings>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    set_hubspot_token(&state.db, id, "").await
}

async fn set_hubspot_token(db: &PgPool, id: Uuid, enc: &str) -> AppResult<Json<ProjectSettings>> {
    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set hubspot_token_enc = $2, hubspot_error = null, hubspot_failed_at = null where project_id = $1 returning *",
    )
    .bind(id)
    .bind(enc)
    .fetch_one(db)
    .await?;
    Ok(Json(settings))
}
