//! The Pipeline page: the board, its stages, and creating, editing and moving deals. See deals.rs.

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::Auth;
use crate::deals::{self, Board, Deal, DealChanges, Move, NewDeal, PipelineBody};
use crate::error::AppResult;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/pipeline", get(board).put(save_stages))
        .route("/deals", post(create))
        .route("/deals/{id}", get(get_deal).patch(update).delete(remove))
        .route("/deals/{id}/move", post(move_deal))
}

#[derive(Deserialize)]
struct BoardQuery {
    project_id: Option<Uuid>,
}

async fn board(State(state): State<AppState>, auth: Auth, Query(q): Query<BoardQuery>) -> AppResult<Json<Board>> {
    if let Some(project_id) = q.project_id {
        auth.project(&state.db, project_id).await?;
    }
    Ok(Json(deals::board(&state.db, auth.org_id, q.project_id).await?))
}

/// Stages and the currency belong to the whole organization, so only admins change them, like the rest of its settings.
async fn save_stages(State(state): State<AppState>, auth: Auth, Json(body): Json<PipelineBody>) -> AppResult<Json<Board>> {
    auth.require_admin()?;
    deals::save_stages(&state, auth.org_id, body).await?;
    Ok(Json(deals::board(&state.db, auth.org_id, None).await?))
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
