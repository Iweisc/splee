//! The chats a project posts its alerts to. hooks::post_chat sends each alert to all of them.

use axum::extract::{Path, State};
use axum::routing::{delete, get};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::ChatChannel;
use crate::state::AppState;

/// Plenty for a team chat, an ops chat and a few phones, and it bounds the posts one alert makes.
const MAX_CHANNELS: i64 = 10;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/chat-channels", get(list).post(create))
        .route("/chat-channels/{id}", delete(remove))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<ChatChannel>>> {
    auth.project(&state.db, id).await?;
    let channels = sqlx::query_as::<_, ChatChannel>("select * from chat_channels where project_id = $1 order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(channels))
}

#[derive(Deserialize)]
struct ChannelBody {
    url: String,
}

async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ChannelBody>) -> AppResult<Json<ChatChannel>> {
    auth.project(&state.db, id).await?;
    let url = body.url.trim();
    if !Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some()) {
        return Err(AppError::BadRequest("The chat URL must be a full URL starting with https://".into()));
    }
    let count: i64 = sqlx::query_scalar("select count(*) from chat_channels where project_id = $1").bind(id).fetch_one(&state.db).await?;
    if count >= MAX_CHANNELS {
        return Err(AppError::BadRequest(format!("A project can post to up to {MAX_CHANNELS} chats")));
    }
    sqlx::query_as::<_, ChatChannel>("insert into chat_channels (project_id, url) values ($1, $2) on conflict do nothing returning *")
        .bind(id)
        .bind(url)
        .fetch_optional(&state.db)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::BadRequest("That chat is already added".into()))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let channel: ChatChannel = util_scope::load(&state.db, "chat_channels", id, auth.org_id).await?;
    sqlx::query("delete from chat_channels where id = $1").bind(channel.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
