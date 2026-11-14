//! The buying committee of a lead's company on the lead's window: the people there, their part in buying, and asking the agent
//! to map it and find who's missing. See agent::committee.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use sqlx::FromRow;
use uuid::Uuid;

use crate::agent::committee::{self, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/leads/{id}/committee", get(show).post(ask))
}

#[derive(Serialize, FromRow)]
struct Person {
    id: Uuid,
    full_name: String,
    title: String,
    status: String,
    created_at: Ts,
}

#[derive(Serialize)]
struct View {
    /// Everyone in the project at the lead's company, the lead included, oldest first.
    people: Vec<Person>,
    latest: Option<Saved>,
}

async fn view(state: &AppState, lead: &Lead) -> AppResult<Json<View>> {
    let people = sqlx::query_as(
        "select l.id, l.full_name, l.title, l.status, l.created_at from leads l
         where l.project_id = $1 and (l.id = $2 or l.company_id = $3 or (l.company_domain <> '' and l.company_domain = $4))
         order by l.created_at",
    )
    .bind(lead.project_id)
    .bind(lead.id)
    .bind(lead.company_id)
    .bind(&lead.company_domain)
    .fetch_all(&state.db)
    .await?;
    let latest = match committee::company_of(&state.db, lead).await? {
        Some(company) => committee::latest(&state.db, company).await?,
        None => None,
    };
    Ok(Json(View { people, latest }))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let lead: Lead = super::util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    view(&state, &lead).await
}

async fn ask(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    let lead: Lead = super::util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    if lead.segment_id.is_none() {
        return Err(AppError::BadRequest("Add the lead to a campaign first, so the people found have one to join".into()));
    }
    if lead.company_id.is_none() && lead.company_domain.as_deref().is_none_or(str::is_empty) {
        return Err(AppError::BadRequest("The lead has no company website to search".into()));
    }
    if let Some(company) = committee::company_of(&state.db, &lead).await?
        && committee::latest(&state.db, company).await?.is_some_and(|l| l.status == "working")
    {
        return Err(AppError::BadRequest("The agent is already working on this company".into()));
    }
    committee::request(&state.db, &lead, auth.org_id).await?;
    view(&state, &lead).await
}
