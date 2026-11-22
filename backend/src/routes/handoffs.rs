//! Handoffs: the section in a won deal's window with what the agent wrote for whoever takes the customer on, and handing
//! them to a teammate. See handoffs.rs.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::handoffs::{self, Handoff};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/deals/{id}/handoff", get(for_deal).post(write_again)).route("/deals/{id}/handoff/owner", post(hand))
}

async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Handoff>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(handoffs::for_deal(&state.db, deal.id).await?))
}

/// Has the agent write the handoff, or write it again keeping the onboarding steps already done.
async fn write_again(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Handoff>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a won deal gets a handoff".into()));
    }
    let run = handoffs::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(handoffs::for_deal(&state.db, deal.id).await?))
}

#[derive(Deserialize)]
struct Hand {
    owner_id: Uuid,
}

/// Hands the customer to a teammate, who gets the handoff by email.
async fn hand(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Hand>) -> AppResult<Json<Option<Handoff>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    handoffs::hand(&state, auth.org_id, deal.id, body.owner_id, auth.user_id).await?;
    Ok(Json(handoffs::for_deal(&state.db, deal.id).await?))
}
