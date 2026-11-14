//! A campaign's insights from its replies: the latest, asking for new ones and using a suggestion. See agent::insights.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::insights::{self, MIN_REPLIES, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Segment;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/segments/{id}/insights", get(show).post(ask)).route("/segments/{id}/insights/{insights_id}/apply", post(apply))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
    /// Replies there are to read now, and how many it takes.
    replies: i64,
    min_replies: i64,
}

async fn view(state: &AppState, segment_id: Uuid) -> AppResult<Json<View>> {
    Ok(Json(View {
        latest: insights::latest(&state.db, segment_id).await?,
        replies: insights::reply_count(&state.db, segment_id).await?,
        min_replies: MIN_REPLIES,
    }))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let segment: Segment = super::util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    view(&state, segment.id).await
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let segment: Segment = super::util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if insights::reply_count(&state.db, segment.id).await? < MIN_REPLIES {
        return Err(AppError::BadRequest(format!("The campaign needs at least {MIN_REPLIES} replies to read")));
    }
    if insights::latest(&state.db, segment.id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already reading this campaign's replies".into()));
    }
    insights::request(&state.db, segment.project_id, auth.org_id, segment.id).await?;
    view(&state, segment.id).await
}

#[derive(Deserialize)]
struct ApplyBody {
    index: usize,
}

async fn apply(State(state): State<AppState>, auth: Auth, Path((id, insights_id)): Path<(Uuid, Uuid)>, Json(body): Json<ApplyBody>) -> AppResult<Json<View>> {
    let segment: Segment = super::util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if !insights::apply(&state.db, segment.id, insights_id, body.index).await? {
        return Err(AppError::BadRequest("This suggestion was already used, or the campaign changed since it was written".into()));
    }
    view(&state, segment.id).await
}
