use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::runs::{event_stream, run_events};
use super::util_input;
use crate::agent::{self, kind};
use crate::crypto::random_token;
use crate::error::{AppError, AppResult};
use crate::models::{AgentEvent, AgentRun, Company, Competitor, EmailMessage, Lead, Project, Segment};
use crate::state::AppState;

const DEMO_RUNS_PER_HOUR: i64 = 10;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/demo", post(create))
        .route("/demo/{id}", get(show))
        .route("/demo/{id}/events", get(stream))
        .route("/demo/{id}/correct", post(correct))
}

/// The visitor's address: the first X-Forwarded-For hop (set by the Next.js proxy), else the TCP peer.
pub fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|ip| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
        .unwrap_or_else(|| peer.ip().to_string())
}

/// Public runs store the requester's IP in `input.ip` so they can be counted here.
pub async fn check_rate(state: &AppState, run_kind: &str, ip: &str, per_hour: i64) -> AppResult<()> {
    let recent: i64 = sqlx::query_scalar(
        "select count(*) from agent_runs where kind = $1 and input ->> 'ip' = $2 and created_at > now() - interval '1 hour'",
    )
    .bind(run_kind)
    .bind(ip)
    .fetch_one(&state.db)
    .await?;
    if recent >= per_hour {
        return Err(AppError::TooManyRequests("Too many requests from your network. Try again in an hour.".into()));
    }
    Ok(())
}

#[derive(Deserialize)]
struct CreateBody {
    domain: Option<String>,
    description: Option<String>,
}

async fn create(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<CreateBody>,
) -> AppResult<Json<Value>> {
    let domain = body.domain.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let description = body.description.as_deref().map(str::trim).filter(|d| !d.is_empty());
    let (domain, description) = match (domain, description) {
        (Some(d), None) => (Some(util_input::domain(d).ok_or_else(|| AppError::BadRequest("That doesn't look like a website".into()))?), None),
        (None, Some(d)) if d.chars().count() <= 2000 => (None, Some(d.to_string())),
        (None, Some(_)) => return Err(AppError::BadRequest("Keep the description under 2000 characters".into())),
        _ => return Err(AppError::BadRequest("Send either a website or a description".into())),
    };

    let mut tx = state.db.begin().await?;
    if let Some(domain) = &domain {
        // Two requests for the same domain (a double click, React StrictMode in dev) must share one project and one run.
        sqlx::query("select pg_advisory_xact_lock(hashtext('demo:' || $1))").bind(domain).execute(&mut *tx).await?;
        let existing: Option<(Uuid, Option<String>)> = sqlx::query_as(
            "select p.id, p.claim_token from projects p
             where p.org_id is null and p.domain = $1 and p.claim_token is not null and p.created_at > now() - interval '24 hours'
               and coalesce((select r.status from agent_runs r where r.project_id = p.id and r.kind = 'explore'
                             order by r.created_at desc limit 1), 'queued') <> 'failed'
             order by p.created_at desc limit 1",
        )
        .bind(domain)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some((project_id, Some(token))) = existing {
            return Ok(Json(json!({"project_id": project_id, "token": token})));
        }
    }

    let ip = client_ip(&headers, peer);
    check_rate(&state, kind::EXPLORE, &ip, DEMO_RUNS_PER_HOUR).await?;
    let token = random_token();
    let project_id: Uuid = sqlx::query_scalar(
        "insert into projects (org_id, claim_token, domain, business_description, name) values (null, $1, $2, $3, $4) returning id",
    )
    .bind(&token)
    .bind(&domain)
    .bind(&description)
    .bind(domain.clone().unwrap_or_default())
    .fetch_one(&mut *tx)
    .await?;
    agent::enqueue(&mut *tx, kind::EXPLORE, Some(project_id), None, json!({"mode": "demo", "ip": ip})).await?;
    tx.commit().await?;
    Ok(Json(json!({"project_id": project_id, "token": token})))
}

#[derive(Deserialize)]
struct TokenQuery {
    // A missing token is just a wrong one, so it gets the same 404.
    #[serde(default)]
    token: String,
    after: Option<i64>,
}

async fn demo_project(state: &AppState, id: Uuid, token: &str) -> AppResult<Project> {
    sqlx::query_as::<_, Project>("select * from projects where id = $1 and org_id is null and claim_token = $2")
        .bind(id)
        .bind(token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)
}

async fn latest_explore(state: &AppState, project_id: Uuid) -> AppResult<Option<AgentRun>> {
    Ok(sqlx::query_as::<_, AgentRun>("select * from agent_runs where project_id = $1 and kind = 'explore' order by created_at desc limit 1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await?)
}

#[derive(Serialize)]
struct DemoState {
    project: Project,
    competitors: Vec<Competitor>,
    segments: Vec<Segment>,
    companies: Vec<Company>,
    leads: Vec<Lead>,
    emails: Vec<EmailMessage>,
    run: Option<AgentRun>,
    events: Vec<AgentEvent>,
}

async fn show(State(state): State<AppState>, Path(id): Path<Uuid>, Query(q): Query<TokenQuery>) -> AppResult<Json<DemoState>> {
    let project = demo_project(&state, id, &q.token).await?;
    let competitors = sqlx::query_as::<_, Competitor>("select * from competitors where project_id = $1 order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let segments = sqlx::query_as::<_, Segment>("select * from segments where project_id = $1 order by position, created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let companies = sqlx::query_as::<_, Company>("select * from companies where project_id = $1 order by fit_score desc, created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let leads = sqlx::query_as::<_, Lead>("select * from leads where project_id = $1 order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let emails = sqlx::query_as::<_, EmailMessage>("select * from email_messages where project_id = $1 and direction = 'outbound' order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    let mut run = latest_explore(&state, id).await?;
    let events = match &run {
        Some(r) => run_events(&state, r.id).await?,
        None => vec![],
    };
    if let Some(r) = run.as_mut() {
        strip_ip(&mut r.input);
    }
    Ok(Json(DemoState { project, competitors, segments, companies, leads, emails, run, events }))
}

/// Run inputs of public runs hold the visitor's IP for rate limiting. It's not shown back.
pub fn strip_ip(input: &mut Value) {
    if let Some(obj) = input.as_object_mut() {
        obj.remove("ip");
    }
}

async fn stream(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
) -> AppResult<Response> {
    demo_project(&state, id, &q.token).await?;
    let run = latest_explore(&state, id).await?.ok_or(AppError::NotFound)?;
    event_stream(state, run.id, q.after.unwrap_or(0)).await
}

#[derive(Deserialize)]
struct CorrectBody {
    note: String,
}

async fn correct(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(q): Query<TokenQuery>,
    Json(body): Json<CorrectBody>,
) -> AppResult<Json<Value>> {
    demo_project(&state, id, &q.token).await?;
    // An empty note is the "Try again" button after a failed run: a plain re-run.
    let note = body.note.trim();
    if note.chars().count() > 1000 {
        return Err(AppError::BadRequest("Describe what's wrong in under 1000 characters".into()));
    }
    let ip = client_ip(&headers, peer);
    check_rate(&state, kind::EXPLORE, &ip, DEMO_RUNS_PER_HOUR).await?;
    // The old run is researching outdated facts. Stop it so the new one starts right away.
    sqlx::query("update agent_runs set status = 'cancelled', finished_at = now() where project_id = $1 and kind = 'explore' and status in ('queued', 'running')")
        .bind(id)
        .execute(&state.db)
        .await?;
    agent::enqueue(&state.db, kind::EXPLORE, Some(id), None, json!({"mode": "demo", "from_step": 1, "note": note, "ip": ip})).await?;
    Ok(Json(json!({"ok": true})))
}
