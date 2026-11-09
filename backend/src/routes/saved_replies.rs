//! Answers a user writes once and inserts into inbox replies. The inbox fills in {first_name} and {company}.

use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::SavedReply;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/saved-replies", get(list).post(create))
        .route("/saved-replies/{id}", delete(remove))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SavedReply>>> {
    auth.project(&state.db, id).await?;
    let replies = sqlx::query_as::<_, SavedReply>("select * from saved_replies where project_id = $1 order by lower(title), created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(replies))
}

#[derive(Deserialize)]
struct SavedReplyBody {
    title: String,
    body: String,
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SavedReplyBody>,
) -> AppResult<Json<SavedReply>> {
    auth.project(&state.db, id).await?;
    let (title, text) = (body.title.trim(), body.body.trim());
    if title.is_empty() || title.chars().count() > 80 {
        return Err(AppError::BadRequest("Give the reply a name of up to 80 characters".into()));
    }
    if text.is_empty() || text.chars().count() > 5000 {
        return Err(AppError::BadRequest("Write the reply, up to 5,000 characters".into()));
    }
    let reply = sqlx::query_as::<_, SavedReply>("insert into saved_replies (project_id, title, body) values ($1, $2, $3) returning *")
        .bind(id)
        .bind(title)
        .bind(text)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(reply))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let reply: SavedReply = util_scope::load(&state.db, "saved_replies", id, auth.org_id).await?;
    sqlx::query("delete from saved_replies where id = $1").bind(reply.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
