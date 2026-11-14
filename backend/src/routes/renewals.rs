//! Renewals: the Customers page's Renewals tab, the section in a won deal's window, setting the date and prepping now.
//! See agent::renewals.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::renewals::{self, Renewal, SELECT};
use crate::auth::Auth;
use crate::deals::{self, Deal};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// How far ahead the tab looks, past the 90 days the agent preps in, so the team sees what's coming.
const HORIZON_DAYS: i32 = 180;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/renewals", get(list))
        .route("/deals/{id}/renewal", get(for_deal).post(prep_now).put(set_date))
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    ahead_days: i32,
    items: Vec<Renewal>,
}

/// Customers renewing within 180 days, soonest first.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select prep_renewals from project_settings where project_id = $1), true)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Renewal>(&format!(
        "{SELECT} where d.project_id = $1 and d.status = 'won' and n.renews_on <= current_date + $2 order by n.renews_on, d.value_cents desc limit 500"
    ))
    .bind(id)
    .bind(HORIZON_DAYS)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, ahead_days: renewals::AHEAD_DAYS, items }))
}

async fn latest(state: &AppState, deal_id: Uuid) -> AppResult<Option<Renewal>> {
    Ok(sqlx::query_as::<_, Renewal>(&format!("{SELECT} where d.id = $1 and d.status = 'won'")).bind(deal_id).fetch_optional(&state.db).await?)
}

async fn won(state: &AppState, auth: &Auth, id: Uuid) -> AppResult<Deal> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a customer renews".into()));
    }
    Ok(deal)
}

/// The won deal's next renewal and its prep, or None for a deal that isn't won.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Renewal>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

/// Has the agent prep the next renewal now, however far off it is.
async fn prep_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Renewal>>> {
    let deal = won(&state, &auth, id).await?;
    let run = renewals::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(latest(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct SetDate {
    /// Null goes back to the anniversary of the win.
    renews_on: Option<NaiveDate>,
}

/// Sets when the customer renews. A date that passes rolls on a year at a time.
async fn set_date(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SetDate>) -> AppResult<Json<Option<Renewal>>> {
    let deal = won(&state, &auth, id).await?;
    sqlx::query("update deals set renews_on = $2 where id = $1").bind(deal.id).bind(body.renews_on).execute(&state.db).await?;
    Ok(Json(latest(&state, deal.id).await?))
}
