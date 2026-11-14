//! Close plans' routes: the deal's plan in the app, and the buyer's page at /plan/{token}, which the frontend renders. See
//! crate::close_plans.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::close_plans::{self, NewStep, Plan, Public, Seen, StepChanges};
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deals/{id}/plan", get(for_deal).post(write))
        .route("/deals/{id}/plan/steps", post(add_step))
        .route("/plan-steps/{id}", patch(update_step).delete(delete_step))
        .route("/plans/{token}", get(page))
        .route("/plans/{token}/view", post(view))
        .route("/plans/{token}/steps/{id}", post(tick))
}

/// Loads and ticks from one address a minute. Past it the page still answers, so a person never sees an error.
const PER_IP: u32 = 30;

async fn plan(state: &AppState, deal_id: Uuid) -> AppResult<Json<Option<Plan>>> {
    Ok(Json(close_plans::for_deal(&state.db, &state.cfg.public_url, deal_id).await?))
}

async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Plan>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    plan(&state, deal.id).await
}

/// Has the agent write the plan, or write it again keeping the steps already done.
async fn write(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Plan>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets a close plan".into()));
    }
    if close_plans::for_deal(&state.db, &state.cfg.public_url, deal.id).await?.is_some_and(|p| p.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing this deal's plan".into()));
    }
    let run = crate::agent::close_plans::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    plan(&state, deal.id).await
}

async fn add_step(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<NewStep>) -> AppResult<Json<Option<Plan>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    close_plans::add_step(&state.db, deal.project_id, deal.id, body).await?;
    plan(&state, deal.id).await
}

async fn update_step(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<StepChanges>) -> AppResult<Json<Option<Plan>>> {
    let deal_id = close_plans::deal_of_step(&state.db, auth.org_id, id).await?;
    close_plans::update_step(&state.db, id, body).await?;
    plan(&state, deal_id).await
}

async fn delete_step(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Plan>>> {
    let deal_id = close_plans::deal_of_step(&state.db, auth.org_id, id).await?;
    close_plans::delete_step(&state.db, id).await?;
    plan(&state, deal_id).await
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    close_plans::public(&state.db, &state.cfg.public_url, &token).await?.map(Json).ok_or(AppError::NotFound)
}

fn allowed(state: &AppState, headers: &HeaderMap, peer: SocketAddr) -> bool {
    let ip = client_ip(headers, peer);
    let key = ip.parse().map(|ip| crate::visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or(ip);
    crate::visitors::allow(&format!("plan:{key}"), PER_IP)
}

/// The page reports a load from the browser, so link checkers that never run scripts don't count as the buyer.
async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> StatusCode {
    if !allowed(&state, &headers, peer) {
        return StatusCode::NO_CONTENT;
    }
    tokio::spawn(async move {
        let result = async {
            if close_plans::record_view(&state.db, &token).await? {
                close_plans::announce(&state.db, &state.cfg.public_url, &token, Seen::Opened).await?;
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "couldn't record a close plan view");
        }
    });
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
struct Tick {
    done: bool,
}

/// The buyer ticks or unticks one of their steps. Finishing one tells the team.
async fn tick(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path((token, id)): Path<(String, Uuid)>,
    Json(body): Json<Tick>,
) -> AppResult<Json<Public>> {
    if !allowed(&state, &headers, peer) {
        return Err(AppError::TooManyRequests("Too many changes at once. Try again in a minute".into()));
    }
    if let Some(step) = close_plans::tick(&state.db, &token, id, body.done).await?
        && body.done
        && let Err(e) = close_plans::announce(&state.db, &state.cfg.public_url, &token, Seen::Finished(&step)).await
    {
        tracing::warn!(error = ?e, "couldn't announce a close plan step");
    }
    page(State(state), Path(token)).await
}
