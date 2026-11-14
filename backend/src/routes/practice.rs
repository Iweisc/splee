//! Practice calls: starting one on an open deal, the rep's lines, scoring it, trying a failed step again, and the deal's
//! past sessions. See agent::practice.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use uuid::Uuid;

use crate::agent::practice::{self, MAX_LINE, MAX_TURNS, SCENARIOS, SELECT, Session, Turn};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deals/{id}/practice", get(for_deal).post(begin))
        .route("/practice/{id}", get(show))
        .route("/practice/{id}/turns", post(say))
        .route("/practice/{id}/score", post(score))
        .route("/practice/{id}/retry", post(retry))
}

/// The deal's sessions, newest first, as the rep sees them.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Session>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    let sessions = sqlx::query_as::<_, Session>(&format!("{SELECT} where s.deal_id = $1 order by s.created_at desc limit 20"))
        .bind(deal.id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(sessions.into_iter().map(Session::for_rep).collect()))
}

#[derive(Deserialize)]
struct Begin {
    scenario: String,
}

/// Starts a practice call on an open deal and has the agent get the buyer ready.
async fn begin(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Begin>) -> AppResult<Json<Session>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "open" {
        return Err(AppError::BadRequest("Practice the next call on an open deal".into()));
    }
    if !SCENARIOS.contains(&body.scenario.as_str()) {
        return Err(AppError::BadRequest("Pick what to practice".into()));
    }
    let mut tx = state.db.begin().await?;
    let session: Uuid = sqlx::query_scalar("insert into practice_sessions (project_id, deal_id, user_id, scenario) values ($1, $2, $3, $4) returning id")
        .bind(deal.project_id)
        .bind(deal.id)
        .bind(auth.user_id)
        .bind(&body.scenario)
        .fetch_one(&mut *tx)
        .await?;
    let run = practice::start(&mut tx, session, deal.project_id, auth.org_id, "setup").await?;
    tx.commit().await?;
    crate::agent::start_now(&state, run).await?;
    shown(&state, session).await
}

/// The session in the caller's organization.
async fn session(state: &AppState, auth: &Auth, id: Uuid) -> AppResult<(Session, Uuid)> {
    let project: Uuid = sqlx::query_scalar("select s.project_id from practice_sessions s join projects p on p.id = s.project_id where s.id = $1 and p.org_id = $2")
        .bind(id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok((practice::load(&state.db, id).await?.ok_or(AppError::NotFound)?, project))
}

async fn shown(state: &AppState, id: Uuid) -> AppResult<Json<Session>> {
    Ok(Json(practice::load(&state.db, id).await?.ok_or(AppError::NotFound)?.for_rep()))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Session>> {
    let (s, _) = session(&state, &auth, id).await?;
    Ok(Json(s.for_rep()))
}

/// Takes the next step when nothing else is running, under the session's lock.
async fn next(state: &AppState, auth: &Auth, id: Uuid, step: &str, line: Option<&str>) -> AppResult<Json<Session>> {
    let (_, project) = session(state, auth, id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("select 1 from practice_sessions where id = $1 for update").bind(id).execute(&mut *tx).await?;
    let s = sqlx::query_as::<_, Session>(&format!("{SELECT} where s.id = $1")).bind(id).fetch_one(&mut *tx).await?;
    if s.busy() {
        return Err(AppError::BadRequest("Wait for the answer first".into()));
    }
    if let Some(line) = line {
        match s.status.as_str() {
            "live" => {}
            "failed" => return Err(AppError::BadRequest("The last step failed. Try it again first".into())),
            _ => return Err(AppError::BadRequest("This call is over".into())),
        }
        let turn = serde_json::to_value(Turn { who: "rep".into(), text: line.to_string() }).map_err(anyhow::Error::from)?;
        sqlx::query("update practice_sessions set turns = turns || jsonb_build_array($2::jsonb) where id = $1").bind(id).bind(turn).execute(&mut *tx).await?;
    }
    if step == "score" {
        sqlx::query("update practice_sessions set ended = true where id = $1").bind(id).execute(&mut *tx).await?;
    }
    let run = practice::start(&mut tx, id, project, auth.org_id, step).await?;
    tx.commit().await?;
    crate::agent::start_now(state, run).await?;
    shown(state, id).await
}

#[derive(Deserialize)]
struct Say {
    text: String,
}

/// The rep's next line, which the buyer answers.
async fn say(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Say>) -> AppResult<Json<Session>> {
    let text = body.text.trim();
    if text.is_empty() || text.chars().count() > MAX_LINE {
        return Err(AppError::BadRequest(format!("Say something, up to {MAX_LINE} characters")));
    }
    let (s, _) = session(&state, &auth, id).await?;
    if s.turns.as_array().map_or(0, Vec::len) + 1 >= MAX_TURNS {
        return Err(AppError::BadRequest("That's as long as a practice call goes. Score it".into()));
    }
    next(&state, &auth, id, "reply", Some(text)).await
}

/// Ends the call and has the agent score it.
async fn score(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Session>> {
    let (s, _) = session(&state, &auth, id).await?;
    let said = s.turns.as_array().map_or(0, |t| t.iter().filter(|t| t["who"] == "rep").count());
    if said == 0 {
        return Err(AppError::BadRequest("Say something first".into()));
    }
    if s.score.is_some() {
        return Err(AppError::BadRequest("This call is already scored".into()));
    }
    next(&state, &auth, id, "score", None).await
}

/// Runs the step that failed again: getting the buyer ready, their answer, or the score.
async fn retry(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Session>> {
    let (s, _) = session(&state, &auth, id).await?;
    if s.status != "failed" {
        return Err(AppError::BadRequest("Nothing to try again".into()));
    }
    let step = if s.persona.is_none() { "setup" } else if s.ended { "score" } else { "reply" };
    next(&state, &auth, id, step, None).await
}
