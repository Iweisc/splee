//! The Pipeline page: the board, its stages, and creating, editing and moving deals. See deals.rs.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::Auth;
use crate::deals::{self, Board, Deal, DealChanges, Move, NewDeal, PipelineBody};
use crate::error::{AppError, AppResult};
use crate::ownership::OwnerFilter;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pipeline", get(board).put(save_stages))
        .route("/deals", post(create))
        .route("/deals/{id}", get(get_deal).patch(update).delete(remove))
        .route("/deals/{id}/move", post(move_deal))
        .route("/deals/{id}/nudge", post(nudge))
        .route("/deals/{id}/health", post(rate))
        .route("/deals/{id}/review", post(review))
        .route("/deals/{id}/qualify", post(qualify))
}

#[derive(Deserialize)]
struct BoardQuery {
    project_id: Option<Uuid>,
    /// Everyone's deals by default, or mine, unassigned or a member's id. See ownership::OwnerFilter.
    owner: Option<String>,
}

async fn board(State(state): State<AppState>, auth: Auth, Query(q): Query<BoardQuery>) -> AppResult<Json<Board>> {
    if let Some(project_id) = q.project_id {
        auth.project(&state.db, project_id).await?;
    }
    let owner = OwnerFilter::parse(q.owner.as_deref(), Some(auth.user_id))?;
    Ok(Json(deals::board(&state.db, auth.org_id, q.project_id, owner).await?))
}

/// Stages and the currency belong to the whole organization, so only admins change them, like the rest of its settings.
async fn save_stages(State(state): State<AppState>, auth: Auth, Json(body): Json<PipelineBody>) -> AppResult<Json<Board>> {
    auth.require_admin()?;
    deals::save_stages(&state, auth.org_id, body).await?;
    Ok(Json(deals::board(&state.db, auth.org_id, None, OwnerFilter::Everyone).await?))
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<NewDeal>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::create(&state, auth.org_id, Some(auth.user_id), "manual", body).await?))
}

async fn get_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::get(&state.db, auth.org_id, id).await?))
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<DealChanges>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::update(&state.db, auth.org_id, id, body).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    deals::delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "deleted": true })))
}

async fn move_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Move>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::move_to(&state, auth.org_id, id, body).await?))
}

/// Has the agent draft a nudge on the deal now. See agent::nudges.
async fn nudge(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Deal>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal can be nudged".into()));
    }
    if deal.nudge.as_ref().is_some_and(|n| n["status"] == "writing") {
        return Err(AppError::BadRequest("The agent is already writing a nudge for this deal".into()));
    }
    crate::agent::nudges::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    Ok(Json(deals::get(&state.db, auth.org_id, id).await?))
}

/// Has the agent rate the deal's health now. See agent::health.
async fn rate(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Deal>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets a health rating".into()));
    }
    if deal.health_writing {
        return Err(AppError::BadRequest("The agent is already rating this deal".into()));
    }
    let run = crate::agent::health::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(deals::get(&state.db, auth.org_id, id).await?))
}

/// Has the agent review why a closed deal was won or lost, again. See agent::reviews.
async fn review(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Deal>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status == "open" {
        return Err(AppError::BadRequest("Only a won or lost deal gets a review".into()));
    }
    if deal.review.as_ref().is_some_and(|r| r["writing"] == true) {
        return Err(AppError::BadRequest("The agent is already reviewing this deal".into()));
    }
    let run = crate::agent::reviews::request(&state.db, deal.id, deal.project_id, auth.org_id, &deal.status).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(deals::get(&state.db, auth.org_id, id).await?))
}

/// Has the agent fill in the deal's MEDDICC scorecard now. See agent::qualify.
async fn qualify(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Deal>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Only an open deal gets qualified".into()));
    }
    if deal.qualification_writing {
        return Err(AppError::BadRequest("The agent is already qualifying this deal".into()));
    }
    let run = crate::agent::qualify::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(deals::get(&state.db, auth.org_id, id).await?))
}
