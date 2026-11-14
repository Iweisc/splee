//! Win-backs on lost deals: the Pipeline's Win-back tab, the section in a lost deal's window, writing one now, moving its
//! day and dropping it. See agent::win_backs.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::agent::win_backs::{self, SELECT, WinBack};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/win-backs", get(list))
        .route("/deals/{id}/win-back", get(for_deal).post(write_now))
        .route("/win-backs/{id}", patch(update))
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    items: Vec<WinBack>,
}

/// The project's win-backs for deals still lost the way they were planned for, soonest first. Dropped ones are left out.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select win_back_lost_deals from project_settings where project_id = $1), true)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, WinBack>(&format!(
        "{SELECT} where w.project_id = $1 and w.dismissed_at is null and d.status = 'lost' and d.closed_at = w.lost_at
         order by w.reach_out_on nulls first, w.lost_at desc limit 300"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, items }))
}

async fn current(state: &AppState, deal_id: Uuid) -> AppResult<Option<WinBack>> {
    Ok(sqlx::query_as::<_, WinBack>(&format!("{SELECT} where w.deal_id = $1 and d.closed_at = w.lost_at and w.dismissed_at is null"))
        .bind(deal_id)
        .fetch_optional(&state.db)
        .await?)
}

/// The lost deal's win-back for this loss, or None before it's planned or once it's dropped.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<WinBack>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(current(&state, deal.id).await?))
}

/// Has the agent write the win-back email now.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<WinBack>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    let Some(lost_at) = deal.closed_at.filter(|_| deal.status == "lost") else {
        return Err(AppError::BadRequest("Only a lost deal gets a win-back".into()));
    };
    let run = win_backs::request(&state.db, deal.id, deal.project_id, auth.org_id, lost_at).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(current(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct Update {
    reach_out_on: Option<NaiveDate>,
    dismissed: Option<bool>,
}

/// Moves the day to try again, which also plans one the agent skipped, or drops the win-back.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Option<WinBack>>> {
    let deal_id: Uuid = sqlx::query_scalar("select w.deal_id from deal_win_backs w join projects p on p.id = w.project_id where w.id = $1 and p.org_id = $2")
        .bind(id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    if let Some(day) = body.reach_out_on {
        let moved = sqlx::query(
            "update deal_win_backs w set reach_out_on = $2, skipped = '', written_at = null, run_id = null, email_id = null
             where w.id = $1
               and not exists (select 1 from email_messages e where e.id = w.email_id and e.status not in ('failed', 'cancelled'))
               and not exists (select 1 from agent_runs r where r.id = w.run_id and r.status in ('queued', 'running'))",
        )
        .bind(id)
        .bind(day)
        .execute(&state.db)
        .await?;
        if moved.rows_affected() == 0 {
            return Err(AppError::Conflict("The email is already written or being written".into()));
        }
    }
    if let Some(dismissed) = body.dismissed {
        sqlx::query("update deal_win_backs set dismissed_at = case when $2 then coalesce(dismissed_at, now()) end where id = $1")
            .bind(id)
            .bind(dismissed)
            .execute(&state.db)
            .await?;
    }
    Ok(Json(current(&state, deal_id).await?))
}
