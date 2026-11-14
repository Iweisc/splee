//! Customer stories: the page a customer approves their case study on (/story/{token}, which the frontend renders) with
//! its view, approval and decline, and the section in a won deal's window that writes it. See agent::stories.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use super::demo::client_ip;
use crate::agent::stories::{self, Approval, Public, SELECT, Story};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::visitors;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/story-pages/{token}", get(page))
        .route("/story-pages/{token}/view", post(view))
        .route("/story-pages/{token}/approve", post(approve))
        .route("/story-pages/{token}/decline", post(decline))
        .route("/deals/{id}/story", get(for_deal).post(write_now))
}

/// Views a minute from one address that get counted, and answers.
const VIEWS_PER_IP: u32 = 30;
const ANSWERS_PER_IP: u32 = 5;

fn allowed(state: &AppState, headers: &HeaderMap, peer: SocketAddr, what: &str, per_minute: u32) -> bool {
    let ip = client_ip(headers, peer).parse().ok();
    visitors::allow(&format!("story-{what}:{}", ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default()), per_minute)
}

fn too_many() -> AppError {
    AppError::TooManyRequests("Too many tries. Wait a minute and try again".into())
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    stories::public(&state.db, &token).await?.map(Json).ok_or(AppError::NotFound)
}

async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> AppResult<StatusCode> {
    if allowed(&state, &headers, peer, "view", VIEWS_PER_IP) {
        stories::viewed(&state.db, &token).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn approve(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(form): Json<Approval>,
) -> AppResult<StatusCode> {
    if form.name.trim().is_empty() || form.name.len() > 120 || form.title.len() > 120 || form.quote.len() > 2000 || form.results.len() > 4000 {
        return Err(AppError::BadRequest("Enter your name".into()));
    }
    if !allowed(&state, &headers, peer, "answer", ANSWERS_PER_IP) {
        return Err(too_many());
    }
    stories::approve(&state.db, &state.cfg.public_url, &token, &form).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn decline(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> AppResult<StatusCode> {
    if !allowed(&state, &headers, peer, "answer", ANSWERS_PER_IP) {
        return Err(too_many());
    }
    stories::decline(&state.db, &state.cfg.public_url, &token).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn current(state: &AppState, deal_id: Uuid) -> AppResult<Option<Story>> {
    Ok(sqlx::query_as::<_, Story>(&format!("{SELECT} where s.deal_id = $1")).bind(deal_id).fetch_optional(&state.db).await?)
}

/// The won deal's story, or None before one.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Story>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(current(&state, deal.id).await?))
}

/// Has the agent write the story and its email now. One the customer approved stays as they approved it.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Story>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a customer has a story to tell".into()));
    }
    if current(&state, deal.id).await?.is_some_and(|s| s.status == "approved") {
        return Err(AppError::BadRequest("They already approved their story. It's in Materials".into()));
    }
    let run = stories::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(current(&state, deal.id).await?))
}
