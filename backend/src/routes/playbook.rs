//! A project's sales playbook: the latest, what there is to write one from, and asking for a new one. See agent::playbook.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::agent::playbook::{self, Counts, MIN_SOURCES, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/playbook", get(show).post(write))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
    /// What the project has to write one from now, and the least it needs.
    available: Counts,
    min_sources: i64,
}

async fn view(state: &AppState, id: Uuid) -> AppResult<View> {
    Ok(View { latest: playbook::latest(&state.db, id).await?, available: playbook::available(&state.db, id).await?, min_sources: MIN_SOURCES })
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    Ok(Json(view(&state, id).await?))
}

async fn write(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    if playbook::available(&state.db, id).await?.total() < MIN_SOURCES {
        return Err(AppError::BadRequest("There's not enough to write a playbook from yet".into()));
    }
    if playbook::latest(&state.db, id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing this playbook".into()));
    }
    let run = playbook::request(&state.db, id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(view(&state, id).await?))
}
