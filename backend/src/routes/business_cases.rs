//! Business cases: the champion's page (/case/{token}, which the frontend renders) with its view and their numbers, and the
//! section in a deal's window that writes it. See business_cases.rs.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::business_cases::{self, Case, Edit, Public};
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;
use crate::visitors;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/case-pages/{token}", get(page))
        .route("/case-pages/{token}/view", post(view))
        .route("/case-pages/{token}/numbers", post(numbers))
        .route("/deals/{id}/case", get(for_deal).post(write_now))
}

/// Views a minute from one address that get counted, and saves of their numbers.
const VIEWS_PER_IP: u32 = 30;
const EDITS_PER_IP: u32 = 10;

fn allowed(state: &AppState, headers: &HeaderMap, peer: SocketAddr, what: &str, per_minute: u32) -> bool {
    let ip = client_ip(headers, peer).parse().ok();
    visitors::allow(&format!("case-{what}:{}", ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default()), per_minute)
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    business_cases::public(&state.db, &token).await?.map(Json).ok_or(AppError::NotFound)
}

async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> AppResult<StatusCode> {
    if allowed(&state, &headers, peer, "view", VIEWS_PER_IP) {
        business_cases::record_view(&state.db, &state.cfg.public_url, &token).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn numbers(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(edit): Json<Edit>,
) -> AppResult<StatusCode> {
    if edit.name.trim().is_empty() || edit.name.len() > 120 {
        return Err(AppError::BadRequest("Enter your name".into()));
    }
    if !allowed(&state, &headers, peer, "numbers", EDITS_PER_IP) {
        return Err(AppError::TooManyRequests("Too many saves. Wait a minute and try again".into()));
    }
    business_cases::edit(&state.db, &state.cfg.public_url, &token, &edit).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn current(state: &AppState, deal_id: Uuid) -> AppResult<Option<Case>> {
    Ok(business_cases::for_deal(&state.db, &state.cfg.public_url, deal_id).await?)
}

/// The deal's business case, or None before one.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Case>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(current(&state, deal.id).await?))
}

/// Has the agent write the business case now, keeping its link.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Case>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets a business case".into()));
    }
    let run = business_cases::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(current(&state, deal.id).await?))
}
