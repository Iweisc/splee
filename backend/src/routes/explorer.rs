use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::demo::{check_rate, client_ip, strip_ip};
use crate::agent::{self, kind};
use crate::error::{AppError, AppResult};
use crate::models::AgentRun;
use crate::state::AppState;

const SEARCHES_PER_HOUR: i64 = 10;

pub fn router() -> Router<AppState> {
    Router::new().route("/explorer", post(create)).route("/explorer/{run_id}", get(show))
}

#[derive(Deserialize)]
struct SearchBody {
    query: String,
    country: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<SearchBody>,
) -> AppResult<Json<Value>> {
    let query = body.query.trim();
    if query.is_empty() || query.chars().count() > 300 {
        return Err(AppError::BadRequest("Describe the companies you're looking for in under 300 characters".into()));
    }
    let country = body.country.as_deref().map(str::trim).filter(|c| !c.is_empty() && c.chars().count() <= 60);
    let ip = client_ip(&headers, peer);
    check_rate(&state, kind::EXPLORER, &ip, SEARCHES_PER_HOUR).await?;
    let run_id = agent::enqueue(&state.db, kind::EXPLORER, None, None, json!({"query": query, "country": country, "ip": ip})).await?;
    Ok(Json(json!({"run_id": run_id})))
}

async fn show(State(state): State<AppState>, Path(run_id): Path<Uuid>) -> AppResult<Json<Value>> {
    let mut run = sqlx::query_as::<_, AgentRun>("select * from agent_runs where id = $1 and kind = 'explorer'")
        .bind(run_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    strip_ip(&mut run.input);
    let results = run.output.get("results").cloned().unwrap_or_else(|| json!([]));
    // The explorer page polls instead of streaming, so it gets the latest progress line here.
    let log: Option<String> = sqlx::query_scalar("select message from agent_events where run_id = $1 and kind = 'log' order by id desc limit 1")
        .bind(run_id)
        .fetch_optional(&state.db)
        .await?;
    Ok(Json(json!({"run": run, "results": results, "log": log})))
}
