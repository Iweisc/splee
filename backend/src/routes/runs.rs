use std::convert::Infallible;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderName, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio::sync::broadcast::error::RecvError;
use uuid::Uuid;

use crate::agent;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::events;
use crate::models::{AgentEvent, AgentRun};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/runs/{id}", get(detail))
        .route("/runs/{id}/events", get(stream))
        .route("/runs/{id}/cancel", post(cancel))
        .route("/runs/{id}/retry", post(retry))
}

/// A run of the organization, including runs for one of its projects.
pub(super) async fn load(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<AgentRun> {
    sqlx::query_as::<_, AgentRun>(
        "select r.* from agent_runs r left join projects p on p.id = r.project_id
         where r.id = $1 and (r.org_id = $2 or p.org_id = $2)",
    )
    .bind(id)
    .bind(org_id)
    .fetch_optional(db)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn run_events(state: &AppState, run_id: Uuid) -> AppResult<Vec<AgentEvent>> {
    Ok(sqlx::query_as::<_, AgentEvent>("select * from agent_events where run_id = $1 order by id").bind(run_id).fetch_all(&state.db).await?)
}

async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let run = load(&state.db, auth.org_id, id).await?;
    let events = run_events(&state, id).await?;
    Ok(Json(json!({"run": run, "events": events})))
}

#[derive(Deserialize)]
pub struct AfterQuery {
    pub after: Option<i64>,
}

async fn stream(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<AfterQuery>,
) -> AppResult<Response> {
    load(&state.db, auth.org_id, id).await?;
    event_stream(state, id, q.after.unwrap_or(0)).await
}

async fn cancel(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let run = load(&state.db, auth.org_id, id).await?;
    let updated = sqlx::query("update agent_runs set status = 'cancelled', finished_at = now() where id = $1 and status in ('queued', 'running')")
        .bind(id)
        .execute(&state.db)
        .await?;
    // A running run gets its error event from the worker when it notices. A queued one never reaches the worker.
    if updated.rows_affected() > 0 {
        // Now rather than when the worker notices, so the refresh the app does right after this request sees it.
        agent::settle_research(&state.db, &run).await;
        if run.status == "queued" {
            events::emit(&state, id, run.step, "error", "Run cancelled", json!({"cancelled": true})).await?;
        }
    }
    Ok(Json(json!({"ok": true})))
}

/// Queues a failed or cancelled project run again with the same input. Classifying and drafting replies have no other
/// button, so this is the only way to redo them after, say, hitting a Claude usage limit.
async fn retry(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let run = load(&state.db, auth.org_id, id).await?;
    if !matches!(run.status.as_str(), "failed" | "cancelled") {
        return Err(AppError::Conflict("Only a failed or cancelled run can run again.".into()));
    }
    // Explorer runs belong to the public tool, not a project.
    let Some(project_id) = run.project_id.filter(|_| run.kind != agent::kind::EXPLORER) else {
        return Err(AppError::BadRequest("This run can't run again from here.".into()));
    };
    if run.kind == agent::kind::EXPLORE {
        // Like POST /projects/:id/research, so the project shows as researching straight away.
        sqlx::query("update projects set status = 'researching' where id = $1").bind(project_id).execute(&state.db).await?;
    }
    let run_id = agent::enqueue(&state.db, &run.kind, Some(project_id), Some(auth.org_id), run.input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

fn is_final(e: &AgentEvent) -> bool {
    e.kind == "done" || e.kind == "error"
}

fn to_sse(e: &AgentEvent) -> Event {
    Event::default().id(e.id.to_string()).data(serde_json::to_string(e).unwrap_or_default())
}

/// SSE for one run: events after `after` from the database, then live ones. Ends after `done` or `error`.
pub async fn event_stream(state: AppState, run_id: Uuid, after: i64) -> AppResult<Response> {
    // Subscribe before reading the backlog so an event emitted in between is not lost. Ids dedupe the overlap.
    let mut rx = state.events.subscribe();
    let backlog = sqlx::query_as::<_, AgentEvent>("select * from agent_events where run_id = $1 and id > $2 order by id")
        .bind(run_id)
        .bind(after)
        .fetch_all(&state.db)
        .await?;
    let status: String = sqlx::query_scalar("select status from agent_runs where id = $1").bind(run_id).fetch_one(&state.db).await?;
    let run_over = matches!(status.as_str(), "succeeded" | "failed" | "cancelled");

    let stream = async_stream::stream! {
        let mut last = after;
        for e in &backlog {
            last = e.id;
            yield Ok::<_, Infallible>(to_sse(e));
            if is_final(e) {
                return;
            }
        }
        // The run can be marked finished a moment before its final event is written. Wait briefly for it.
        let deadline = run_over.then(|| tokio::time::Instant::now() + Duration::from_secs(3));
        loop {
            let received = match deadline {
                Some(at) => match tokio::time::timeout_at(at, rx.recv()).await {
                    Ok(r) => r,
                    Err(_) => return,
                },
                None => rx.recv().await,
            };
            let fresh: Vec<AgentEvent> = match received {
                Ok(e) if e.run_id == run_id && e.id > last => vec![e],
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => {
                    match sqlx::query_as::<_, AgentEvent>("select * from agent_events where run_id = $1 and id > $2 order by id")
                        .bind(run_id)
                        .bind(last)
                        .fetch_all(&state.db)
                        .await
                    {
                        Ok(rows) => rows,
                        Err(_) => return,
                    }
                }
                Err(RecvError::Closed) => return,
            };
            for e in &fresh {
                last = e.id;
                yield Ok(to_sse(e));
                if is_final(e) {
                    return;
                }
            }
        }
    };
    // no-transform stops gzip middleware (Next's dev proxy, nginx) from buffering the stream until it ends.
    let headers = [(header::CACHE_CONTROL, "no-cache, no-transform"), (HeaderName::from_static("x-accel-buffering"), "no")];
    Ok((headers, Sse::new(stream).keep_alive(KeepAlive::default())).into_response())
}
