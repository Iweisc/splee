//! The URLs a project sends to, each added and removed on its own: chats for its alerts (hooks::post_chat) and webhook
//! endpoints for its lead events (hooks::send_lead_event).

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
use crate::models::Endpoint;
use crate::state::AppState;

/// Plenty for a team chat, an ops chat and a few phones, or a CRM and a few automations, and it bounds the posts one
/// event makes.
const MAX_ENDPOINTS: i64 = 10;

#[derive(Clone, Copy)]
enum List {
    Chats,
    Webhooks,
}

impl List {
    /// Fixed names, never user input, so they can go into the SQL.
    fn table(self) -> &'static str {
        match self {
            List::Chats => "chat_channels",
            List::Webhooks => "webhook_endpoints",
        }
    }

    fn noun(self) -> &'static str {
        match self {
            List::Chats => "chat",
            List::Webhooks => "webhook endpoint",
        }
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/chat-channels", get(list_chats).post(add_chat))
        .route("/chat-channels/{id}", delete(remove_chat))
        .route("/projects/{id}/webhook-endpoints", get(list_webhooks).post(add_webhook))
        .route("/webhook-endpoints/{id}", delete(remove_webhook))
}

#[derive(Deserialize)]
struct UrlBody {
    url: String,
}

async fn list_chats(state: State<AppState>, auth: Auth, id: Path<Uuid>) -> AppResult<Json<Vec<Endpoint>>> {
    list(state, auth, id, List::Chats).await
}

async fn add_chat(state: State<AppState>, auth: Auth, id: Path<Uuid>, body: Json<UrlBody>) -> AppResult<Json<Endpoint>> {
    add(state, auth, id, body, List::Chats).await
}

async fn remove_chat(state: State<AppState>, auth: Auth, id: Path<Uuid>) -> AppResult<Json<Value>> {
    remove(state, auth, id, List::Chats).await
}

async fn list_webhooks(state: State<AppState>, auth: Auth, id: Path<Uuid>) -> AppResult<Json<Vec<Endpoint>>> {
    list(state, auth, id, List::Webhooks).await
}

async fn add_webhook(state: State<AppState>, auth: Auth, id: Path<Uuid>, body: Json<UrlBody>) -> AppResult<Json<Endpoint>> {
    add(state, auth, id, body, List::Webhooks).await
}

async fn remove_webhook(state: State<AppState>, auth: Auth, id: Path<Uuid>) -> AppResult<Json<Value>> {
    remove(state, auth, id, List::Webhooks).await
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, list: List) -> AppResult<Json<Vec<Endpoint>>> {
    auth.project(&state.db, id).await?;
    let sql = format!("select * from {} where project_id = $1 order by created_at", list.table());
    Ok(Json(sqlx::query_as::<_, Endpoint>(&sql).bind(id).fetch_all(&state.db).await?))
}

async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<UrlBody>, list: List) -> AppResult<Json<Endpoint>> {
    auth.project(&state.db, id).await?;
    let (table, noun) = (list.table(), list.noun());
    let url = body.url.trim();
    if !Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some()) {
        return Err(AppError::BadRequest(format!("The {noun} URL must be a full URL starting with https://")));
    }
    let count: i64 = sqlx::query_scalar(&format!("select count(*) from {table} where project_id = $1")).bind(id).fetch_one(&state.db).await?;
    if count >= MAX_ENDPOINTS {
        return Err(AppError::BadRequest(format!("A project can have up to {MAX_ENDPOINTS} of these")));
    }
    sqlx::query_as::<_, Endpoint>(&format!("insert into {table} (project_id, url) values ($1, $2) on conflict do nothing returning *"))
        .bind(id)
        .bind(url)
        .fetch_optional(&state.db)
        .await?
        .map(Json)
        .ok_or_else(|| AppError::BadRequest(format!("That {noun} is already added")))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, list: List) -> AppResult<Json<Value>> {
    let endpoint: Endpoint = util_scope::load(&state.db, list.table(), id, auth.org_id).await?;
    sqlx::query(&format!("delete from {} where id = $1", list.table())).bind(endpoint.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
