//! The Job changes tab on the Leads page: moves seen on LinkedIn, the watch list behind them, and adding people to it. See
//! jobchanges.rs.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::jobchanges::{self, AddBody, Added, Overview, Watched};
use crate::models::Segment;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/job-changes", get(overview))
        .route("/projects/{id}/job-changes/campaign", put(set_campaign))
        .route("/projects/{id}/job-changes/{change}/dismiss", post(dismiss))
        .route("/projects/{id}/job-changes/{change}/campaign", post(add_to_campaign))
        .route("/projects/{id}/watch-list", get(watch_list).post(add))
}

async fn overview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Overview>> {
    auth.project(&state.db, id).await?;
    Ok(Json(jobchanges::overview(&state.db, id).await?))
}

async fn watch_list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Watched>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(jobchanges::watch_list(&state.db, id).await?))
}

async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Added>> {
    auth.project(&state.db, id).await?;
    Ok(Json(jobchanges::add(&state.db, id, body).await?))
}

async fn dismiss(State(state): State<AppState>, auth: Auth, Path((id, change)): Path<(Uuid, Uuid)>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    jobchanges::dismiss(&state.db, id, change).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct CampaignBody {
    segment_id: Option<Uuid>,
}

/// The campaign must be the project's, since leads from its moves join it.
async fn project_segment(state: &AppState, auth: &Auth, project_id: Uuid, segment_id: Uuid) -> AppResult<Segment> {
    let segment: Segment = util_scope::load(&state.db, "segments", segment_id, auth.org_id).await?;
    if segment.project_id != project_id {
        return Err(AppError::BadRequest("That campaign isn't part of this project".into()));
    }
    Ok(segment)
}

async fn add_to_campaign(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, change)): Path<(Uuid, Uuid)>,
    Json(body): Json<CampaignBody>,
) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let segment_id = body.segment_id.ok_or_else(|| AppError::BadRequest("Pick a campaign".into()))?;
    let segment = project_segment(&state, &auth, id, segment_id).await?;
    jobchanges::add_to_campaign(&state.db, id, change, segment.id).await?;
    Ok(Json(json!({ "segment_id": segment.id, "segment_name": segment.name })))
}

async fn set_campaign(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<CampaignBody>) -> AppResult<Json<Overview>> {
    auth.project(&state.db, id).await?;
    if let Some(segment_id) = body.segment_id {
        project_segment(&state, &auth, id, segment_id).await?;
    }
    jobchanges::set_campaign(&state.db, id, body.segment_id).await?;
    Ok(Json(jobchanges::overview(&state.db, id).await?))
}
