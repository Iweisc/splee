//! Battlecards on the Competitors page and in the window of a deal that mentions a competitor. See agent::battlecards.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::util_scope;
use crate::agent::battlecards::{self, ITEMS, Item, TRACKED_AS};
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

/// The competitors this deal is up against: the one its review names, and those its notes, lost reason or the buyer's
/// emails and calls mention.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Item>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    let reviewed: Vec<Uuid> = sqlx::query_scalar(&format!(
        "select c.id from competitors c join deal_reviews rv on rv.deal_id = $1 and lower(rv.competitor) = lower({TRACKED_AS}) where c.project_id = $2"
    ))
    .bind(deal.id)
    .bind(deal.project_id)
    .fetch_all(&state.db)
    .await?;
    let said: Vec<String> = sqlx::query_scalar(
        "(select body from email_messages where lead_id = $1 and direction = 'inbound' order by created_at desc limit 30)
         union all (select transcript from meetings where lead_id = $1 order by started_at desc limit 10)
         union all (select transcript from calls where lead_id = $1 and transcript <> '' order by created_at desc limit 10)",
    )
    .bind(deal.lead_id)
    .fetch_all(&state.db)
    .await?;
    let text = [deal.notes.as_str(), deal.lost_reason.as_str()].into_iter().chain(said.iter().map(String::as_str)).collect::<Vec<_>>().join("\n");
    let up_against = items(&state, deal.project_id)
        .await?
        .into_iter()
        .filter(|i| reviewed.contains(&i.competitor_id) || battlecards::find(&text, &battlecards::words(&i.name, &i.domain)).is_some())
        .collect();
    Ok(Json(up_against))
}
