//! Battlecards on the Competitors page and in the window of a deal that mentions a competitor. See agent::battlecards.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::util_scope;
use crate::agent::battlecards::{self, ITEMS, Item};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::models::Competitor;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/battlecards", get(list))
        .route("/competitors/{id}/battlecard", post(write_now))
        .route("/deals/{id}/battlecards", get(for_deal))
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    items: Vec<Item>,
}

async fn items(state: &AppState, project_id: Uuid) -> AppResult<Vec<Item>> {
    Ok(sqlx::query_as::<_, Item>(ITEMS).bind(project_id).fetch_all(&state.db).await?)
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select write_battlecards from project_settings where project_id = $1), true)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(View { enabled, items: items(&state, id).await? }))
}

/// Has the agent write the competitor's battlecard now and returns it, writing.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Item>> {
    let competitor: Competitor = util_scope::load(&state.db, "competitors", id, auth.org_id).await?;
    let run = battlecards::request(&state.db, competitor.id, competitor.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    items(&state, competitor.project_id).await?.into_iter().find(|i| i.competitor_id == competitor.id).map(Json).ok_or(AppError::NotFound)
}

/// The competitors this deal is up against. See battlecards::up_against.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Item>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(battlecards::up_against(&state.db, deal.id, deal.project_id).await?))
}
