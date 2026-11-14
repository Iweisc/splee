//! A project's pipeline forecast: the latest, and asking for a new one. See agent::forecast.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use crate::agent::forecast::{self, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/forecast", get(show).post(ask))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    Ok(Json(View { latest: forecast::latest(&state.db, id).await? }))
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let open: bool = sqlx::query_scalar("select exists (select 1 from deals where project_id = $1 and status = 'open')").bind(id).fetch_one(&state.db).await?;
    if !open {
        return Err(AppError::BadRequest("The project has no open deals to forecast".into()));
    }
    if forecast::latest(&state.db, id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing a forecast for this project".into()));
    }
    let run = forecast::request(&state.db, id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(View { latest: forecast::latest(&state.db, id).await? }))
}
