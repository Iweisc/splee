//! Deal rooms: the buyer's page (/room/{token}, which the frontend renders) with its view and questions, and the section
//! in a deal's window that writes it and answers them. See deal_rooms.rs.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::deal_rooms::{self, Ask, Public, Room};
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::visitors;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/room-pages/{token}", get(page))
        .route("/room-pages/{token}/view", post(view))
        .route("/room-pages/{token}/questions", post(ask))
        .route("/deals/{id}/room", get(for_deal).post(write_now))
        .route("/room-questions/{id}", post(answer).delete(remove))
}

/// Views a minute from one address that get counted, and questions.
const VIEWS_PER_IP: u32 = 30;
const QUESTIONS_PER_IP: u32 = 5;

fn allowed(state: &AppState, headers: &HeaderMap, peer: SocketAddr, what: &str, per_minute: u32) -> bool {
    let ip = client_ip(headers, peer).parse().ok();
    visitors::allow(&format!("room-{what}:{}", ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default()), per_minute)
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    deal_rooms::public(&state.db, &state.cfg.public_url, &token).await?.map(Json).ok_or(AppError::NotFound)
}

async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> AppResult<StatusCode> {
    if allowed(&state, &headers, peer, "view", VIEWS_PER_IP) {
        deal_rooms::record_view(&state.db, &state.cfg.public_url, &token).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn ask(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(form): Json<Ask>,
) -> AppResult<StatusCode> {
    if form.name.trim().is_empty() || form.name.len() > 120 || form.question.trim().is_empty() || form.question.len() > 2000 {
        return Err(AppError::BadRequest("Enter your name and a question".into()));
    }
    if !allowed(&state, &headers, peer, "question", QUESTIONS_PER_IP) {
        return Err(AppError::TooManyRequests("Too many questions. Wait a minute and try again".into()));
    }
    let run = deal_rooms::ask(&state.db, &state.cfg.public_url, &token, &form).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn current(state: &AppState, deal_id: Uuid) -> AppResult<Option<Room>> {
    Ok(deal_rooms::for_deal(&state.db, &state.cfg.public_url, deal_id).await?)
}

/// The deal's room, or None before one.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Room>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(current(&state, deal.id).await?))
}

/// Has the agent write the room's welcome now, keeping its link and questions.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Room>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets a deal room".into()));
    }
    let run = deal_rooms::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(current(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct Answer {
    answer: String,
}

/// Publishes the answer on the page, or takes it down when it's empty.
async fn answer(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Answer>) -> AppResult<Json<Option<Room>>> {
    if body.answer.len() > 4000 {
        return Err(AppError::BadRequest("That answer is too long".into()));
    }
    let deal_id = deal_rooms::deal_of_question(&state.db, auth.org_id, id).await?;
    deal_rooms::answer(&state.db, id, auth.user_id, &body.answer).await?;
    Ok(Json(current(&state, deal_id).await?))
}

/// Deletes a question, like spam, answered or not.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Room>>> {
    let deal_id = deal_rooms::deal_of_question(&state.db, auth.org_id, id).await?;
    sqlx::query("delete from deal_room_questions where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(current(&state, deal_id).await?))
}
