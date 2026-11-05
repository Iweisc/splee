use axum::extract::{Path, State};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use super::auth::ClaimBody;
use super::{settings, util_input, util_scope};
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::billing;
use crate::error::{AppError, AppResult};
use crate::models::{AgentRun, Competitor, Project, ProjectSettings};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects", get(list).post(create))
        .route("/projects/claim", post(claim_signed_in))
        .route("/projects/{id}", get(detail).patch(update).delete(remove))
        .route("/projects/{id}/research", post(research))
        .route("/projects/{id}/runs", get(runs))
        .route("/projects/{id}/rewrite-emails", post(rewrite_emails))
        .route("/projects/{id}/competitors", get(list_competitors).post(add_competitor))
        .route("/competitors/{id}", delete(remove_competitor))
        .route("/projects/{id}/start", post(start))
        .route("/projects/{id}/pause", post(pause))
}

/// Moves an anonymous demo project into an org. Returns false when the token is wrong or it was already claimed.
pub async fn claim(conn: &mut PgConnection, project_id: Uuid, token: &str, org_id: Uuid, user_id: Uuid) -> AppResult<bool> {
    let claimed = sqlx::query(
        "update projects set org_id = $1, created_by = $2, claim_token = null
         where id = $3 and org_id is null and claim_token = $4",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(project_id)
    .bind(token)
    .execute(&mut *conn)
    .await?
    .rows_affected()
        == 1;
    if claimed {
        sqlx::query("insert into project_settings (project_id) values ($1) on conflict do nothing")
            .bind(project_id)
            .execute(&mut *conn)
            .await?;
        // Demo runs were created without an org. Runs are org-scoped once the project has an owner.
        sqlx::query("update agent_runs set org_id = $1 where project_id = $2")
            .bind(org_id)
            .bind(project_id)
            .execute(&mut *conn)
            .await?;
    }
    Ok(claimed)
}

#[derive(Serialize, FromRow)]
struct ProjectSummary {
    #[serde(flatten)]
    #[sqlx(flatten)]
    project: Project,
    sent: i64,
    replies: i64,
    hot: i64,
    active_segments: i64,
    paused: bool,
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<ProjectSummary>>> {
    let projects = sqlx::query_as::<_, ProjectSummary>(
        "select p.*,
           (select count(*) from email_messages e
             where e.project_id = p.id and e.direction = 'outbound' and e.status = 'sent') as sent,
           (select count(*) from leads l where l.project_id = p.id
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound')) as replies,
           (select count(*) from leads l where l.project_id = p.id and l.status in ('hot', 'meeting_booked')) as hot,
           (select count(*) from segments s where s.project_id = p.id and s.status in ('working', 'scaling')) as active_segments,
           coalesce((select ps.paused from project_settings ps where ps.project_id = p.id), true) as paused
         from projects p where p.org_id = $1 order by p.created_at desc",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(projects))
}

#[derive(Deserialize)]
struct CreateBody {
    domain: Option<String>,
    description: Option<String>,
}

async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<CreateBody>) -> AppResult<Json<Project>> {
    let domain = match body.domain.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        Some(raw) => Some(util_input::domain(raw).ok_or_else(|| AppError::BadRequest("Enter a valid website, like acme.com".into()))?),
        None => None,
    };
    let description = body.description.as_deref().map(str::trim).filter(|d| !d.is_empty());
    if domain.is_none() && description.is_none() {
        return Err(AppError::BadRequest("Enter your website or describe your business".into()));
    }

    let mut tx = state.db.begin().await?;
    let project = sqlx::query_as::<_, Project>(
        "insert into projects (org_id, domain, business_description, name, created_by)
         values ($1, $2, $3, coalesce($2, ''), $4) returning *",
    )
    .bind(auth.org_id)
    .bind(&domain)
    .bind(description)
    .bind(auth.user_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("insert into project_settings (project_id) values ($1)").bind(project.id).execute(&mut *tx).await?;
    tx.commit().await?;

    agent::enqueue(&state.db, kind::EXPLORE, Some(project.id), Some(auth.org_id), json!({ "mode": "app" })).await?;
    Ok(Json(project))
}

async fn claim_signed_in(State(state): State<AppState>, auth: Auth, Json(body): Json<ClaimBody>) -> AppResult<Json<Project>> {
    let mut tx = state.db.begin().await?;
    if !claim(&mut tx, body.project_id, &body.token, auth.org_id, auth.user_id).await? {
        return Err(AppError::BadRequest("This demo can't be claimed. It may already belong to an account.".into()));
    }
    tx.commit().await?;
    Ok(Json(auth.project(&state.db, body.project_id).await?))
}

#[derive(Serialize)]
struct ProjectDetail {
    project: Project,
    settings: ProjectSettings,
    latest_run: Option<AgentRun>,
}

async fn load_detail(state: &AppState, project: Project) -> AppResult<ProjectDetail> {
    let settings = settings::load_or_create(&state.db, project.id).await?;
    let latest_run = sqlx::query_as::<_, AgentRun>(
        "select * from agent_runs where project_id = $1 and kind = 'explore' order by created_at desc limit 1",
    )
    .bind(project.id)
    .fetch_optional(&state.db)
    .await?;
    Ok(ProjectDetail { project, settings, latest_run })
}

async fn detail(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectDetail>> {
    let project = auth.project(&state.db, id).await?;
    Ok(Json(load_detail(&state, project).await?))
}

#[derive(Deserialize)]
struct UpdateBody {
    name: Option<String>,
    company: Option<Value>,
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateBody>,
) -> AppResult<Json<ProjectDetail>> {
    auth.project(&state.db, id).await?;
    if body.company.as_ref().is_some_and(|c| !c.is_object()) {
        return Err(AppError::BadRequest("company must be an object".into()));
    }
    // company is merged key by key so the UI can save one field without resending the whole profile.
    let project = sqlx::query_as::<_, Project>(
        "update projects set name = coalesce($2, name), company = company || coalesce($3, '{}'::jsonb)
         where id = $1 returning *",
    )
    .bind(id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.company)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(load_detail(&state, project).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    auth.project(&state.db, id).await?;
    sqlx::query("delete from projects where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize, Default)]
struct ResearchBody {
    note: Option<String>,
}

async fn research(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<ResearchBody>>,
) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let mut input = json!({ "mode": "app", "from_step": 1 });
    if let Some(note) = body.unwrap_or_default().0.note.filter(|n| !n.trim().is_empty()) {
        input["note"] = json!(note);
    }
    sqlx::query("update projects set status = 'researching' where id = $1").bind(id).execute(&state.db).await?;
    let run_id = agent::enqueue(&state.db, kind::EXPLORE, Some(id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// Rewrites every unsent email in the project with the current writing settings, e.g. after the templates changed.
async fn rewrite_emails(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let busy: bool = sqlx::query_scalar(
        "select exists (select 1 from agent_runs where project_id = $1 and kind = 'write_emails' and status in ('queued', 'running'))",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    if busy {
        return Err(AppError::Conflict("The agent is already writing emails for this project. Try again when it's done.".into()));
    }
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "select distinct lead_id from email_messages
         where project_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status in ('draft', 'approved', 'scheduled')",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    if lead_ids.is_empty() {
        return Err(AppError::Conflict("There are no unsent emails to rewrite.".into()));
    }
    let input = json!({ "lead_ids": lead_ids, "rewrite": true });
    let run_id = agent::enqueue(&state.db, kind::WRITE_EMAILS, Some(id), Some(auth.org_id), input).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

async fn runs(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<AgentRun>>> {
    auth.project(&state.db, id).await?;
    let runs = sqlx::query_as::<_, AgentRun>(
        "select * from agent_runs where project_id = $1 order by created_at desc limit 50",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(runs))
}

async fn list_competitors(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Competitor>>> {
    auth.project(&state.db, id).await?;
    let competitors = sqlx::query_as::<_, Competitor>("select * from competitors where project_id = $1 order by created_at")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(competitors))
}

#[derive(Deserialize)]
struct CompetitorBody {
    domain: String,
}

async fn add_competitor(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<CompetitorBody>,
) -> AppResult<Json<Competitor>> {
    auth.project(&state.db, id).await?;
    let domain = util_input::domain(&body.domain).ok_or_else(|| AppError::BadRequest("Enter a valid domain".into()))?;
    let competitor = sqlx::query_as::<_, Competitor>(
        "insert into competitors (project_id, domain, name) values ($1, $2, $2) returning *",
    )
    .bind(id)
    .bind(domain)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(competitor))
}

async fn remove_competitor(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let competitor: Competitor = util_scope::load(&state.db, "competitors", id, auth.org_id).await?;
    sqlx::query("delete from competitors where id = $1").bind(competitor.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    if billing::spendable(&state, auth.org_id).await? <= 0 {
        return Err(AppError::PaymentRequired("Your balance is empty. Top up your credits to start sending.".into()));
    }
    let has_mailbox: bool = sqlx::query_scalar("select exists(select 1 from mailboxes where org_id = $1 and status = 'active')")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    if !has_mailbox {
        return Err(AppError::BadRequest("Connect an active mailbox before starting. Emails are sent from your own mailboxes.".into()));
    }
    settings::load_or_create(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update project_settings set paused = false where project_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("update projects set status = 'active' where id = $1").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn pause(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    sqlx::query("update project_settings set paused = true where project_id = $1").bind(id).execute(&mut *tx).await?;
    sqlx::query("update projects set status = 'paused' where id = $1 and status = 'active'").bind(id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}
