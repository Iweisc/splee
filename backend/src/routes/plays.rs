//! The Plays page: a project's plays, what each one did lately, and turning them on, off or changing them. See crate::plays.

use axum::extract::{Path, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::plays::{ACTIONS, MAX_PLAYS, TRIGGERS, fits};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/plays", get(list).post(create))
        .route("/plays/{id}", patch(update).delete(remove))
}

#[derive(Serialize, FromRow)]
pub struct PlayRow {
    id: Uuid,
    project_id: Uuid,
    name: String,
    trigger: String,
    action: String,
    segment_id: Option<Uuid>,
    segment_name: Option<String>,
    min_score: i32,
    daily_cap: i32,
    enabled: bool,
    checked_at: Ts,
    created_at: Ts,
    /// Times it fired in the last 24 hours and the last 30 days, skips included.
    fired_today: i64,
    fired_month: i64,
}

#[derive(Serialize, FromRow)]
pub struct Firing {
    id: Uuid,
    play_id: Uuid,
    play_name: String,
    domain: Option<String>,
    lead_id: Option<Uuid>,
    what: String,
    outcome: String,
    note: String,
    run_id: Option<Uuid>,
    created_at: Ts,
}

#[derive(Serialize)]
pub struct Plays {
    plays: Vec<PlayRow>,
    /// The 50 latest firings across the project's plays.
    firings: Vec<Firing>,
}

const ROWS: &str = "select p.id, p.project_id, p.name, p.trigger, p.action, p.segment_id, s.name as segment_name, p.min_score, p.daily_cap, p.enabled,
       p.checked_at, p.created_at,
       (select count(*) from play_firings f where f.play_id = p.id and f.created_at > now() - interval '1 day') as fired_today,
       (select count(*) from play_firings f where f.play_id = p.id and f.created_at > now() - interval '30 days') as fired_month
     from plays p left join segments s on s.id = p.segment_id";

pub async fn plays(db: &PgPool, project_id: Uuid) -> sqlx::Result<Plays> {
    let plays = sqlx::query_as::<_, PlayRow>(&format!("{ROWS} where p.project_id = $1 order by p.created_at")).bind(project_id).fetch_all(db).await?;
    let firings = sqlx::query_as::<_, Firing>(
        "select f.id, f.play_id, p.name as play_name, f.domain::text as domain, f.lead_id, f.what, f.outcome, f.note, f.run_id, f.created_at
         from play_firings f join plays p on p.id = f.play_id where f.project_id = $1 order by f.created_at desc limit 50",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    Ok(Plays { plays, firings })
}

async fn one(db: &PgPool, id: Uuid) -> sqlx::Result<PlayRow> {
    sqlx::query_as::<_, PlayRow>(&format!("{ROWS} where p.id = $1")).bind(id).fetch_one(db).await
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Plays>> {
    auth.project(&state.db, id).await?;
    Ok(Json(plays(&state.db, id).await?))
}

#[derive(Deserialize, Default)]
pub struct PlayBody {
    name: Option<String>,
    trigger: Option<String>,
    action: Option<String>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
    min_score: Option<i32>,
    daily_cap: Option<i32>,
    enabled: Option<bool>,
    /// On create: also act on what happened in the last this many days, up to 30. Only what happens from now on without it.
    backfill_days: Option<i32>,
}

/// A play as it would be saved.
#[derive(Clone, Copy)]
struct Rule<'a> {
    name: &'a str,
    trigger: &'a str,
    action: &'a str,
    segment_id: Option<Uuid>,
    min_score: i32,
    daily_cap: i32,
}

/// Checks the whole play as it would be saved: the trigger and action fit, a campaign play has a campaign of this project.
async fn check(db: &PgPool, project_id: Uuid, r: &Rule<'_>) -> AppResult<()> {
    let Rule { name, trigger, action, segment_id, min_score, daily_cap } = *r;
    if name.trim().is_empty() || name.chars().count() > 80 {
        return Err(AppError::BadRequest("Give the play a name of at most 80 characters".into()));
    }
    util_input::one_of("trigger", trigger, &TRIGGERS)?;
    util_input::one_of("action", action, &ACTIONS)?;
    if !fits(trigger, action) {
        let why = if action == "add_to_campaign" { "Only companies go into a campaign" } else { "Only leads get emails" };
        return Err(AppError::BadRequest(format!("{why}, so this trigger can't take that action")));
    }
    if !(0..=100).contains(&min_score) || !(1..=200).contains(&daily_cap) {
        return Err(AppError::BadRequest("min_score is 0 to 100 and daily_cap 1 to 200".into()));
    }
    match segment_id {
        Some(id) => {
            let ours: bool = sqlx::query_scalar("select exists (select 1 from segments where id = $1 and project_id = $2)").bind(id).bind(project_id).fetch_one(db).await?;
            if !ours {
                return Err(AppError::BadRequest("That campaign isn't part of this project".into()));
            }
        }
        None if action == "add_to_campaign" => return Err(AppError::BadRequest("Pick the campaign to add companies to".into())),
        None => {}
    }
    Ok(())
}

async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PlayBody>) -> AppResult<Json<PlayRow>> {
    auth.project(&state.db, id).await?;
    let (name, trigger, action) = (body.name.unwrap_or_default(), body.trigger.unwrap_or_default(), body.action.unwrap_or_default());
    let (min_score, daily_cap) = (body.min_score.unwrap_or(0), body.daily_cap.unwrap_or(20));
    check(&state.db, id, &Rule { name: &name, trigger: &trigger, action: &action, segment_id: body.segment_id, min_score, daily_cap }).await?;
    let count: i64 = sqlx::query_scalar("select count(*) from plays where project_id = $1").bind(id).fetch_one(&state.db).await?;
    if count >= MAX_PLAYS {
        return Err(AppError::BadRequest(format!("A project can have up to {MAX_PLAYS} plays")));
    }
    let backfill = body.backfill_days.unwrap_or(0).clamp(0, 30);
    let play: Uuid = sqlx::query_scalar(
        "insert into plays (project_id, name, trigger, action, segment_id, min_score, daily_cap, enabled, checked_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, now() - make_interval(days => $9)) returning id",
    )
    .bind(id)
    .bind(name.trim())
    .bind(&trigger)
    .bind(&action)
    .bind(body.segment_id)
    .bind(min_score)
    .bind(daily_cap)
    .bind(body.enabled.unwrap_or(true))
    .bind(backfill)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(one(&state.db, play).await?))
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PlayBody>) -> AppResult<Json<PlayRow>> {
    let play: crate::plays::Play = super::util_scope::load(&state.db, "plays", id, auth.org_id).await?;
    let name = body.name.unwrap_or(play.name);
    let segment_id = body.segment_id.or(play.segment_id);
    let (min_score, daily_cap) = (body.min_score.unwrap_or(play.min_score), body.daily_cap.unwrap_or(play.daily_cap));
    // The trigger and action stay: a different rule is a different play, with its own history.
    check(&state.db, play.project_id, &Rule { name: &name, trigger: &play.trigger, action: &play.action, segment_id, min_score, daily_cap }).await?;
    // Turning a play back on starts from now, so it doesn't act on everything that happened while it was off.
    sqlx::query(
        "update plays set name = $2, segment_id = $3, min_score = $4, daily_cap = $5, enabled = coalesce($6, enabled),
                          checked_at = case when $6 and not enabled then now() else checked_at end
         where id = $1",
    )
    .bind(id)
    .bind(name.trim())
    .bind(segment_id)
    .bind(min_score)
    .bind(daily_cap)
    .bind(body.enabled)
    .execute(&state.db)
    .await?;
    Ok(Json(one(&state.db, id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let play: crate::plays::Play = super::util_scope::load(&state.db, "plays", id, auth.org_id).await?;
    sqlx::query("delete from plays where id = $1").bind(play.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
