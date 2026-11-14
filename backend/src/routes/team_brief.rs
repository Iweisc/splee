//! The team brief on the Targets tab: the latest, asking for one now, and whether a Monday one is emailed to owners and
//! admins. Owners and admins only. See agent::team_brief.

use axum::extract::State;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};

use crate::agent::team_brief::{self, Saved};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/team-brief", get(show).post(write)).route("/team-brief/weekly", put(weekly))
}

#[derive(Serialize)]
struct View {
    latest: Option<Saved>,
    /// Whether one is written and emailed every Monday.
    weekly: bool,
}

async fn view(state: &AppState, auth: &Auth) -> AppResult<Json<View>> {
    let weekly: bool = sqlx::query_scalar("select team_brief from organizations where id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    Ok(Json(View { latest: team_brief::latest(&state.db, auth.org_id).await?, weekly }))
}

async fn show(State(state): State<AppState>, auth: Auth) -> AppResult<Json<View>> {
    auth.require_admin()?;
    view(&state, &auth).await
}

async fn write(State(state): State<AppState>, auth: Auth) -> AppResult<Json<View>> {
    auth.require_admin()?;
    if team_brief::latest(&state.db, auth.org_id).await?.is_some_and(|l| l.status == "writing") {
        return Err(AppError::BadRequest("The agent is already writing the team brief".into()));
    }
    let run = team_brief::request(&state.db, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    view(&state, &auth).await
}

#[derive(Deserialize)]
struct Weekly {
    on: bool,
}

async fn weekly(State(state): State<AppState>, auth: Auth, Json(body): Json<Weekly>) -> AppResult<Json<View>> {
    auth.require_admin()?;
    sqlx::query("update organizations set team_brief = $2 where id = $1").bind(auth.org_id).bind(body.on).execute(&state.db).await?;
    view(&state, &auth).await
}
