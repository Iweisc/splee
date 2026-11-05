use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::ProjectSettings;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/settings", get(get_settings).put(update_settings))
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

// `paused` is left out on purpose: unpausing goes through POST /projects/:id/start, which checks balance and mailboxes.
#[derive(Deserialize)]
struct SettingsBody {
    daily_budget_cents: Option<i32>,
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

    if body.daily_budget_cents.is_some_and(|c| c < 0) {
        return bad("Daily budget can't be negative");
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

    let settings = sqlx::query_as::<_, ProjectSettings>(
        "update project_settings set
           daily_budget_cents = coalesce($2, daily_budget_cents),
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
           slack_webhook_url = coalesce($21, slack_webhook_url)
         where project_id = $1 returning *",
    )
    .bind(id)
    .bind(body.daily_budget_cents)
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
    .fetch_one(&state.db)
    .await?;
    Ok(Json(settings))
}
