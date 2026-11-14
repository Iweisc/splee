//! Account expansion: the Pipeline's Expansion tab, the section in a won deal's window, looking now and dropping one.
//! See agent::expansions.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::expansions::{self, Expansion, SELECT};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/expansions", get(list))
        .route("/deals/{id}/expansion", get(for_deal).post(look_now))
        .route("/expansions/{id}", patch(update))
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    items: Vec<Expansion>,
}

/// The project's expansions on deals still won, newest first. Dropped ones are left out.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select grow_accounts from project_settings where project_id = $1), true)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Expansion>(&format!(
        "{SELECT} where x.project_id = $1 and x.dismissed_at is null and d.status = 'won' order by x.created_at desc limit 300"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, items }))
}

/// The deal's latest expansion, or None once it's dropped, rather than the one before it.
async fn latest(state: &AppState, deal_id: Uuid) -> AppResult<Option<Expansion>> {
    Ok(sqlx::query_as::<_, Expansion>(&format!(
        "{SELECT} where x.deal_id = $1 and x.dismissed_at is null
           and not exists (select 1 from deal_expansions n where n.deal_id = x.deal_id and n.created_at > x.created_at)"
    ))
    .bind(deal_id)
    .fetch_optional(&state.db)
    .await?)
}

/// The won deal's latest expansion, or None before the agent looked or once it's dropped.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Expansion>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

/// Has the agent look for room to grow the account now.
async fn look_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Expansion>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a won deal can grow".into()));
    }
    let run = expansions::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct Update {
    dismissed: bool,
}

/// Drops an expansion from the lists, or brings it back.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Option<Expansion>>> {
    let deal_id: Uuid = sqlx::query_scalar(
        "update deal_expansions x set dismissed_at = case when $3 then coalesce(x.dismissed_at, now()) end
         from projects p where p.id = x.project_id and x.id = $1 and p.org_id = $2 returning x.deal_id",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(body.dismissed)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(latest(&state, deal_id).await?))
}
