use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::Lead;
use crate::state::AppState;

// POST is the RFC 8058 one-click unsubscribe that mail clients send to the List-Unsubscribe URL.
pub fn router() -> Router<AppState> {
    Router::new().route("/unsubscribe/{token}", get(unsubscribe).post(unsubscribe))
}

async fn unsubscribe(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Value>> {
    let lead = sqlx::query_as::<_, Lead>("select * from leads where unsubscribe_token = $1")
        .bind(&token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let org_id: Option<uuid::Uuid> = sqlx::query_scalar("select org_id from projects where id = $1").bind(lead.project_id).fetch_one(&state.db).await?;
    mail::suppress_lead(&state.db, &lead, org_id, "unsubscribed", "unsubscribe").await?;
    Ok(Json(json!({"ok": true, "email": lead.email})))
}
