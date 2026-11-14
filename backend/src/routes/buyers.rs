//! A project's buyer profile: the latest, and asking for a new one. See agent::buyers.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::agent::buyers::{self, MIN_WON, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/buyers", get(show).post(ask))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
    /// Won deals there are to read now, and how many it takes.
    won: i64,
    min_won: i64,
}

async fn view(state: &AppState, project_id: Uuid) -> AppResult<Json<View>> {
    Ok(Json(View { latest: buyers::latest(&state.db, project_id).await?, won: buyers::won_count(&state.db, project_id).await?, min_won: MIN_WON }))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    view(&state, id).await
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    if buyers::won_count(&state.db, id).await? < MIN_WON {
        return Err(AppError::BadRequest(format!("The project needs at least {MIN_WON} won deals to compare")));
    }
    if buyers::latest(&state.db, id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already reading this project's won deals".into()));
    }
    let run = buyers::request(&state.db, id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    view(&state, id).await
}
