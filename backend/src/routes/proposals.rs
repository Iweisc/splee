//! Proposals' routes: the deal's proposal in the app, and the buyer's page at /proposal/{token}, which the frontend renders.
//! See crate::proposals.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::proposals::{self, Acceptance, Changes, Proposal, Public};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deals/{id}/proposal", get(for_deal).post(write).put(update))
        .route("/proposals/{token}", get(page))
        .route("/proposals/{token}/view", post(view))
        .route("/proposals/{token}/accept", post(accept))
}

/// Loads and acceptances from one address a minute. Past it the page still loads, so a person never sees an error.
const PER_IP: u32 = 30;

async fn proposal(state: &AppState, deal_id: Uuid) -> AppResult<Json<Option<Proposal>>> {
    Ok(Json(proposals::for_deal(&state.db, &state.cfg.public_url, deal_id).await?))
}

async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Proposal>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    proposal(&state, deal.id).await
}

/// Has the agent write the proposal, or write its text again keeping the prices.
async fn write(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Proposal>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets a proposal".into()));
    }
    if proposals::for_deal(&state.db, &state.cfg.public_url, deal.id).await?.is_some_and(|p| p.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing this deal's proposal".into()));
    }
    let run = crate::agent::proposals::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    proposal(&state, deal.id).await
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Changes>) -> AppResult<Json<Option<Proposal>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    proposals::update(&state.db, deal.project_id, deal.id, body).await?;
    proposal(&state, deal.id).await
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    proposals::public(&state.db, &token).await?.map(Json).ok_or(AppError::NotFound)
}

fn allowed(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> bool {
    let ip = client_ip(headers, peer);
    let key = ip.parse().map(|ip| crate::visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or(ip);
    crate::visitors::allow(&format!("proposal:{key}"), PER_IP)
}

/// The page reports a load from the browser, so link checkers that never run scripts don't count as the buyer.
async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> StatusCode {
    if !allowed(&state, &headers, peer) {
        return StatusCode::NO_CONTENT;
    }
    tokio::spawn(async move {
        let result = async {
            if proposals::record_view(&state.db, &token).await? {
                proposals::announce_view(&state.db, &state.cfg.public_url, &token).await?;
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "couldn't record a proposal view");
        }
    });
    StatusCode::NO_CONTENT
}

async fn accept(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(body): Json<Acceptance>,
) -> AppResult<Json<Public>> {
    if !allowed(&state, &headers, peer) {
        return Err(AppError::TooManyRequests("Too many tries at once. Try again in a minute".into()));
    }
    proposals::accept(&state, &token, body).await?;
    page(State(state), Path(token)).await
}
