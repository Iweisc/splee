//! Funding and company news for the Companies page's News tab: the signals signals/ found, when the project's companies were
//! last checked, and whether a new Form D pings the team. Adding the companies to a campaign goes through /hiring/add.

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::signals::{self, KINDS, Listed};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/signals", get(overview)).route("/projects/{id}/signals/settings", patch(set_settings))
}

#[derive(Deserialize)]
pub struct SignalsQuery {
    /// One of signals::KINDS. Every kind without it.
    kind: Option<String>,
    /// Only signals dated in this many days. Every kept one without it.
    days: Option<i32>,
}

/// The project's signals for the query. The caller checks the project belongs to its organization.
pub async fn listed(db: &PgPool, project_id: Uuid, q: &SignalsQuery) -> AppResult<Vec<Listed>> {
    let kind = q.kind.as_deref().map(str::trim).filter(|k| !k.is_empty());
    if let Some(kind) = kind
        && !KINDS.contains(&kind)
    {
        return Err(AppError::BadRequest(format!("kind must be one of {}", KINDS.join(", "))));
    }
    Ok(signals::list(db, project_id, kind, q.days.filter(|d| *d > 0), None).await?)
}

#[derive(Serialize)]
struct Overview {
    signals: Vec<Listed>,
    /// Companies checked by any source, and when the last check was.
    checked: i64,
    last_checked_at: Option<Ts>,
    /// False when the server has no SEC_USER_AGENT, so only company feeds are read.
    sec: bool,
    funding_alerts: bool,
}

async fn overview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<SignalsQuery>) -> AppResult<Json<Overview>> {
    auth.project(&state.db, id).await?;
    let signals = listed(&state.db, id, &q).await?;
    let (checked, last_checked_at, funding_alerts): (i64, Option<Ts>, Option<bool>) = sqlx::query_as(
        "select count(distinct domain), max(updated_at), (select funding_alerts from project_settings where project_id = $1)
         from company_signal_checks where project_id = $1",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    let sec = state.cfg.sec_user_agent.is_some();
    Ok(Json(Overview { signals, checked, last_checked_at, sec, funding_alerts: funding_alerts.unwrap_or(false) }))
}

#[derive(Serialize, Deserialize)]
struct SettingsBody {
    funding_alerts: bool,
}

async fn set_settings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SettingsBody>) -> AppResult<Json<SettingsBody>> {
    auth.project(&state.db, id).await?;
    sqlx::query(
        "insert into project_settings (project_id, funding_alerts) values ($1, $2) on conflict (project_id) do update set funding_alerts = excluded.funding_alerts",
    )
    .bind(id)
    .bind(body.funding_alerts)
    .execute(&state.db)
    .await?;
    Ok(Json(body))
}
