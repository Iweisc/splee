use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::http::header::USER_AGENT;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{NotificationPrefs, PushDevice};
use crate::push::{self, Sent};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/notification-preferences", get(get_prefs).put(update_prefs))
        .route("/push/key", get(push_key))
        .route("/push/subscriptions", get(list_devices).post(subscribe))
        .route("/push/subscriptions/{id}", delete(remove_device))
        .route("/push/test", post(send_test))
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

/// Each member can have push on in this many browsers, so one account can't make the server post to endless URLs.
const DEVICES_PER_MEMBER: i64 = 10;

async fn push_key(State(state): State<AppState>, _auth: Auth) -> AppResult<Json<Value>> {
    Ok(Json(json!({ "public_key": push::public_key(&state).await? })))
}

async fn list_devices(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<PushDevice>>> {
    let devices = sqlx::query_as::<_, PushDevice>(
        "select id, user_agent, created_at, last_success_at, failures from push_subscriptions where user_id = $1 order by created_at",
    )
    .bind(auth.user_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(devices))
}

#[derive(Deserialize)]
struct SubscriptionKeys {
    p256dh: String,
    auth: String,
}

/// What PushSubscription.toJSON() gives.
#[derive(Deserialize)]
struct SubscribeBody {
    endpoint: String,
    keys: SubscriptionKeys,
}

/// Saves this browser's subscription for the member, or updates it. The app sends it again on each visit, so changed keys are
/// picked up, and a browser someone else signed in to before moves to whoever subscribed last.
async fn subscribe(State(state): State<AppState>, auth: Auth, headers: HeaderMap, Json(body): Json<SubscribeBody>) -> AppResult<Json<PushDevice>> {
    let endpoint = body.endpoint.trim();
    if let Some(why) = push::invalid(endpoint, &body.keys.p256dh, &body.keys.auth) {
        return Err(AppError::BadRequest(why.into()));
    }
    let others: i64 = sqlx::query_scalar("select count(*) from push_subscriptions where user_id = $1 and endpoint <> $2")
        .bind(auth.user_id)
        .bind(endpoint)
        .fetch_one(&state.db)
        .await?;
    if others >= DEVICES_PER_MEMBER {
        return Err(AppError::BadRequest(format!("Push is on in {DEVICES_PER_MEMBER} of your browsers. Remove one to add this one.")));
    }
    let label = push::device_label(headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default());
    let device = sqlx::query_as::<_, PushDevice>(
        "insert into push_subscriptions (user_id, endpoint, p256dh, auth, user_agent) values ($1, $2, $3, $4, $5)
         on conflict (endpoint) do update set
           user_id = excluded.user_id, p256dh = excluded.p256dh, auth = excluded.auth, user_agent = excluded.user_agent,
           failures = case when push_subscriptions.user_id = excluded.user_id then push_subscriptions.failures else 0 end
         returning id, user_agent, created_at, last_success_at, failures",
    )
    .bind(auth.user_id)
    .bind(endpoint)
    .bind(body.keys.p256dh.trim())
    .bind(body.keys.auth.trim())
    .bind(label)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(device))
}

async fn remove_device(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let removed = sqlx::query("delete from push_subscriptions where id = $1 and user_id = $2").bind(id).bind(auth.user_id).execute(&state.db).await?;
    if removed.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(json!({ "ok": true })))
}

/// Pushes a test to each of the member's browsers and says how many got it.
async fn send_test(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    let sent = push::send_test(&state, auth.user_id).await?;
    let count = |s: Sent| sent.iter().filter(|&&x| x == s).count();
    Ok(Json(json!({ "delivered": count(Sent::Delivered), "gone": count(Sent::Gone), "failed": count(Sent::Failed) })))
}
