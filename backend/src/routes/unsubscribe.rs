use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};
use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::Lead;
use crate::state::AppState;

// GET only looks the address up, because mail security scanners open links in a real browser and would unsubscribe people
// who never clicked. POST unsubscribes: the page's button, and the RFC 8058 one-click POST mail clients send to List-Unsubscribe.
pub fn router() -> Router<AppState> {
    Router::new().route("/unsubscribe/{token}", get(show).post(unsubscribe))
}

async fn find(db: &PgPool, token: &str) -> AppResult<Lead> {
    sqlx::query_as::<_, Lead>("select * from leads where unsubscribe_token = $1")
        .bind(token)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn show(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let lead = find(&state.db, &token).await?;
    Ok(Json(json!({"email": lead.email, "unsubscribed": lead.status == "unsubscribed"})))
}

async fn unsubscribe(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let lead = find(&state.db, &token).await?;
    let org_id: Option<uuid::Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;
    mail::suppress_lead(&state.db, &lead, org_id, "unsubscribed", "unsubscribe").await?;
    Ok(Json(json!({"ok": true, "email": lead.email})))
}
