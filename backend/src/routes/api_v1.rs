use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::leads::{self, LeadDetail};
use super::runs;
use super::segments::{self, SegmentWithStats};
use super::stats::{self, Totals};
use super::suppressions;
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::util_input::{self, LEAD_STATUSES};
use super::util_scope;
use crate::agent::{self, kind};
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{AgentRun, EmailMessage, Lead, Project, Segment, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/suppressions", post(add_suppressions))
        .route("/v1/segments/{id}/write-emails", post(write_emails))
        .route("/v1/runs/{id}", get(get_run))
}

const MAX_LEADS_PER_REQUEST: usize = 1000;
const MAX_MESSAGES: i64 = 500;
const MAX_SUPPRESSIONS_PER_REQUEST: usize = 1000;

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

#[derive(Deserialize)]
struct FindQuery {
    #[serde(default)]
    email: String,
}

/// Calendars and CRMs know a person by email, not by lead id. The same person can be a lead in several projects, so this is a list.
async fn find_leads(State(state): State<AppState>, auth: ApiKeyAuth, Query(q): Query<FindQuery>) -> AppResult<Json<Vec<Lead>>> {
    let email = q.email.trim();
    if email.is_empty() {
        return Err(AppError::BadRequest("Add ?email= with the address to look up".into()));
    }
    let leads = sqlx::query_as::<_, Lead>(
        "select l.* from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and l.email = $2::citext order by l.created_at desc limit 100",
    )
    .bind(auth.org_id)
    .bind(email)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(leads))
}

async fn get_lead(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(leads::detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
struct LeadUpdateBody {
    status: Option<String>,
    notes: Option<String>,
}

/// Lets a CRM or calendar mark a lead, e.g. meeting_booked, or write its notes. Follow-ups only go to contacted leads,
/// so any other status stops them.
async fn update_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<LeadUpdateBody>,
) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    if body.status.is_none() && body.notes.is_none() {
        return Err(AppError::BadRequest("Send status, notes or both".into()));
    }
    if let Some(status) = &body.status {
        util_input::one_of("status", status, LEAD_STATUSES)?;
    }
    let notes = util_input::notes(body.notes.as_deref())?;
    let before = lead.status;
    let lead = sqlx::query_as::<_, Lead>("update leads set status = coalesce($2, status), notes = coalesce($3, notes) where id = $1 returning *")
        .bind(lead.id)
        .bind(&body.status)
        .bind(notes)
        .fetch_one(&state.db)
        .await?;
    // No meeting_booked webhook here: it would echo a booking straight back to the tool that sent it.
    if hooks::booked(&before, &lead) {
        hooks::booked_in_slack(&state, &lead);
    }
    Ok(Json(leads::detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
struct SinceQuery {
    since: Option<Ts>,
}

async fn replies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
) -> AppResult<Json<Vec<EmailMessage>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let messages = sqlx::query_as::<_, EmailMessage>(
        "select * from email_messages
         where project_id = $1 and direction = 'inbound' and ($2::timestamptz is null or created_at > $2)
         order by created_at limit $3",
    )
    .bind(id)
    .bind(q.since)
    .bind(MAX_MESSAGES)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(messages))
}

/// The polling twin of the email_sent webhook: what Splee sent, oldest first, for logging outreach in a CRM.
async fn sent_emails(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
) -> AppResult<Json<Vec<EmailMessage>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let messages = sqlx::query_as::<_, EmailMessage>(
        "select * from email_messages
         where project_id = $1 and direction = 'outbound' and status = 'sent' and ($2::timestamptz is null or sent_at > $2)
         order by sent_at limit $3",
    )
    .bind(id)
    .bind(q.since)
    .bind(MAX_MESSAGES)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(messages))
}

#[derive(Serialize)]
struct ProjectStats {
    totals: Totals,
    campaigns: Vec<SegmentWithStats>,
    steps: Vec<stats::StepStats>,
}

/// Lifetime funnel and real cost, for reporting tools. Same numbers as the overview and campaign pages.
async fn project_stats(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectStats>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(ProjectStats {
        totals: stats::totals(&state.db, id).await?,
        campaigns: segments::with_stats(&state.db, id, None).await?,
        steps: stats::steps(&state.db, id).await?,
    }))
}

#[derive(Deserialize)]
struct SuppressionsBody {
    values: Vec<String>,
}

/// For syncing do-not-contact lists from a CRM. The sender checks suppressions before every email, so queued emails to these stop too.
async fn add_suppressions(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<SuppressionsBody>) -> AppResult<Json<Value>> {
    if body.values.len() > MAX_SUPPRESSIONS_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_SUPPRESSIONS_PER_REQUEST} values per request")));
    }
    let added = suppressions::insert(&state.db, auth.org_id, body.values.iter().map(|v| v.trim()).filter(|v| !v.is_empty())).await?;
    Ok(Json(json!({ "added": added })))
}

/// Writes a first email for every lead in the campaign that has none, like "Write emails" in the app. Lets a CRM push
/// leads with segment_id and then get them written. In a campaign that's sending, the new emails go straight into the queue.
async fn write_emails(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    segments::ensure_unwritten_leads(&state.db, &segment).await?;
    let run_id = agent::enqueue(&state.db, kind::WRITE_EMAILS, Some(segment.project_id), Some(auth.org_id), json!({ "segment_id": id })).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// For waiting on a run started with write-emails: status goes queued, running, then succeeded, failed or cancelled.
async fn get_run(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<AgentRun>> {
    Ok(Json(runs::load(&state.db, auth.org_id, id).await?))
}
