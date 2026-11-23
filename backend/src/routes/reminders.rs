//! Reminders a member sets themselves on a lead: a time to come back to it and a note. The lead window lists the open ones
//! for the whole team, and Today lists yours once they're due (routes/today.rs). Only whoever set one marks it done.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;

const MAX_NOTE: usize = 500;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/leads/{id}/reminders", get(list).post(create))
        .route("/reminders/{id}/done", post(done))
}

#[derive(Serialize, FromRow)]
struct Reminder {
    id: Uuid,
    lead_id: Uuid,
    user_id: Uuid,
    /// Who set it, their name or else their email.
    user_name: String,
    due_at: Ts,
    note: String,
    created_at: Ts,
}

const SELECT: &str = "select r.id, r.lead_id, r.user_id, coalesce(nullif(u.name, ''), u.email::text) as user_name, r.due_at, r.note, r.created_at
                      from lead_reminders r join users u on u.id = r.user_id";

#[derive(Deserialize)]
struct NewReminder {
    due_at: Ts,
    #[serde(default)]
    note: String,
}

/// The lead's open reminders, everyone's, soonest first. 404 for another organization's lead.
async fn list(State(state): State<AppState>, auth: Auth, Path(lead): Path<Uuid>) -> AppResult<Json<Vec<Reminder>>> {
    check_lead(&state, &auth, lead).await?;
    let reminders = sqlx::query_as(&format!("{SELECT} where r.lead_id = $1 and r.done_at is null order by r.due_at"))
        .bind(lead)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(reminders))
}

async fn create(State(state): State<AppState>, auth: Auth, Path(lead): Path<Uuid>, Json(body): Json<NewReminder>) -> AppResult<Json<Reminder>> {
    check_lead(&state, &auth, lead).await?;
    let note = body.note.trim();
    if note.chars().count() > MAX_NOTE {
        return Err(AppError::BadRequest(format!("A reminder's note can be up to {MAX_NOTE} characters")));
    }
    if body.due_at > Utc::now() + Duration::days(366) {
        return Err(AppError::BadRequest("Pick a day within the next year".into()));
    }
    let id: Uuid = sqlx::query_scalar("insert into lead_reminders (lead_id, user_id, due_at, note) values ($1, $2, $3, $4) returning id")
        .bind(lead)
        .bind(auth.user_id)
        .bind(body.due_at)
        .bind(note)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(sqlx::query_as(&format!("{SELECT} where r.id = $1")).bind(id).fetch_one(&state.db).await?))
}

/// Marks your own reminder done. 404 for someone else's, so they can't clear what you meant to come back to.
async fn done(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<serde_json::Value>> {
    let updated = sqlx::query("update lead_reminders set done_at = now() where id = $1 and user_id = $2 and done_at is null")
        .bind(id)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

async fn check_lead(state: &AppState, auth: &Auth, lead: Uuid) -> AppResult<()> {
    let theirs: bool =
        sqlx::query_scalar("select exists (select 1 from leads l join projects p on p.id = l.project_id where l.id = $1 and p.org_id = $2)")
            .bind(lead)
            .bind(auth.org_id)
            .fetch_one(&state.db)
            .await?;
    if theirs { Ok(()) } else { Err(AppError::NotFound) }
}
