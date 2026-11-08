use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::leads::{self, LeadDetail};
use super::segments::{self, SegmentWithStats};
use super::stats::{self, Totals};
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::models::{EmailMessage, Lead, Project, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/leads/{id}", get(get_lead))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/stats", get(project_stats))
}

const MAX_LEADS_PER_REQUEST: usize = 1000;
const MAX_REPLIES: i64 = 500;

async fn check_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<()> {
    sqlx::query("select 1 from projects where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(())
}

async fn list_projects(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<Project>>> {
    let projects = sqlx::query_as::<_, Project>("select * from projects where org_id = $1 order by created_at desc")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(projects))
}

#[derive(Deserialize)]
struct LeadsQuery {
    status: Option<String>,
}

async fn list_leads(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<LeadsQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Lead>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let status = q.status.filter(|s| !s.is_empty());
    let filter = "from leads where project_id = $1 and ($2::text is null or status = $2)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {filter}"))
        .bind(id)
        .bind(&status)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Lead>(&format!("select * {filter} order by created_at desc limit $3 offset $4"))
        .bind(id)
        .bind(&status)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

#[derive(Deserialize)]
struct AddLeadsBody {
    leads: Vec<NewLead>,
}

async fn add_leads(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddLeadsBody>,
) -> AppResult<Json<Value>> {
    check_project(&state.db, id, auth.org_id).await?;
    if body.leads.len() > MAX_LEADS_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_LEADS_PER_REQUEST} leads per request")));
    }
    let rows = body.leads.into_iter().enumerate().collect();
    let outcome = util_lead_import::insert(&state.db, auth.org_id, id, "api", rows).await?;
    Ok(Json(json!({ "imported": outcome.imported, "skipped": outcome.skipped, "errors": outcome.errors })))
}

async fn get_lead(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(leads::detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
struct RepliesQuery {
    since: Option<Ts>,
}

async fn replies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<RepliesQuery>,
) -> AppResult<Json<Vec<EmailMessage>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let messages = sqlx::query_as::<_, EmailMessage>(
        "select * from email_messages
         where project_id = $1 and direction = 'inbound' and ($2::timestamptz is null or created_at > $2)
         order by created_at limit $3",
    )
    .bind(id)
    .bind(q.since)
    .bind(MAX_REPLIES)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(messages))
}

#[derive(Serialize)]
struct ProjectStats {
    totals: Totals,
    campaigns: Vec<SegmentWithStats>,
}

/// Lifetime funnel and real cost, for reporting tools. Same numbers as the overview and campaign pages.
async fn project_stats(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectStats>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(ProjectStats { totals: stats::totals(&state.db, id).await?, campaigns: segments::with_stats(&state.db, id, None).await? }))
}
