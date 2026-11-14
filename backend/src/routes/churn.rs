//! Customers at risk: the Customers page's At risk tab, the section in a won deal's window, checking now and dropping one.
//! See agent::churn.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::churn::{self, Risk, SELECT};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/churn-risks", get(list))
        .route("/deals/{id}/churn-risk", get(for_deal).post(check_now))
        .route("/churn-risks/{id}", patch(update))
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    customers: i64,
    items: Vec<Risk>,
}

/// Each customer's latest check, on deals still won, newest first, without dropped ones, and how many customers there are.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let (enabled, customers): (bool, i64) = sqlx::query_as(
        "select coalesce((select watch_churn from project_settings where project_id = $1), true),
                (select count(*) from deals where project_id = $1 and status = 'won')",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let items = sqlx::query_as::<_, Risk>(&format!(
        "{SELECT} where c.project_id = $1 and c.dismissed_at is null and d.status = 'won'
           and not exists (select 1 from churn_risks n where n.deal_id = c.deal_id and n.created_at > c.created_at)
         order by c.created_at desc limit 300"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, customers, items }))
}

/// The deal's latest check, or None once it's dropped, rather than the one before it.
async fn latest(state: &AppState, deal_id: Uuid) -> AppResult<Option<Risk>> {
    Ok(sqlx::query_as::<_, Risk>(&format!(
        "{SELECT} where c.deal_id = $1 and c.dismissed_at is null
           and not exists (select 1 from churn_risks n where n.deal_id = c.deal_id and n.created_at > c.created_at)"
    ))
    .bind(deal_id)
    .fetch_optional(&state.db)
    .await?)
}

/// The won deal's latest check, or None before one or once it's dropped.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Risk>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

/// Has the agent check on the customer now.
async fn check_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Risk>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a customer can be at risk".into()));
    }
    let run = churn::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct Update {
    dismissed: bool,
}

/// Drops a check from the lists, or brings it back.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Option<Risk>>> {
    let deal_id: Uuid = sqlx::query_scalar(
        "update churn_risks c set dismissed_at = case when $3 then coalesce(c.dismissed_at, now()) end
         from projects p where p.id = c.project_id and c.id = $1 and p.org_id = $2 returning c.deal_id",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(body.dismissed)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(latest(&state, deal_id).await?))
}
