use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::NotificationPrefs;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/notification-preferences", get(get_prefs).put(update_prefs))
}

#[derive(Deserialize)]
struct PrefsBody {
    hot_lead_email: Option<bool>,
    daily_digest: Option<bool>,
    weekly_report: Option<bool>,
}

async fn get_prefs(State(state): State<AppState>, auth: Auth) -> AppResult<Json<NotificationPrefs>> {
    let prefs = sqlx::query_as::<_, NotificationPrefs>(
        "insert into notification_prefs (user_id) values ($1)
         on conflict (user_id) do update set user_id = excluded.user_id returning *",
    )
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(prefs))
}

async fn update_prefs(State(state): State<AppState>, auth: Auth, Json(body): Json<PrefsBody>) -> AppResult<Json<NotificationPrefs>> {
    let prefs = sqlx::query_as::<_, NotificationPrefs>(
        "insert into notification_prefs (user_id, hot_lead_email, daily_digest, weekly_report)
         values ($1, coalesce($2, true), coalesce($3, true), coalesce($4, false))
         on conflict (user_id) do update set
           hot_lead_email = coalesce($2, notification_prefs.hot_lead_email),
           daily_digest = coalesce($3, notification_prefs.daily_digest),
           weekly_report = coalesce($4, notification_prefs.weekly_report)
         returning *",
    )
    .bind(auth.user_id)
    .bind(body.hot_lead_email)
    .bind(body.daily_digest)
    .bind(body.weekly_report)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(prefs))
}
