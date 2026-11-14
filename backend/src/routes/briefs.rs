//! A lead's meeting brief on their window: the latest one, and writing one now. See agent::briefs.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use uuid::Uuid;

use crate::agent::briefs::{self, Saved};
use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::Lead;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/leads/{id}/brief", get(latest).post(write))
}

async fn latest(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Saved>>> {
    let lead: Lead = super::util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(briefs::latest(&state.db, lead.id).await?))
}

async fn write(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Saved>>> {
    let lead: Lead = super::util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    briefs::request(&state.db, &lead, auth.org_id).await?;
    Ok(Json(briefs::latest(&state.db, lead.id).await?))
}
