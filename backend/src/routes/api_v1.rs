use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::directory::{self, Added, SearchQuery};
use super::emails::{self, EmailRow};
use super::inbox::{self, InboxThread};
use super::leads::{self, LeadDetail, LeadRow};
use super::runs;
use super::segments::{self, FindPeopleBody, SegmentBody, SegmentWithStats, WriteEmailsBody};
use super::stats::{self, Totals};
use super::suppressions;
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::util_input::{self, LEAD_STATUSES};
use super::util_scope;
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{AgentRun, EmailMessage, Lead, Project, Segment, Suppression, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/{id}/campaigns", get(list_campaigns).post(create_campaign))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/suppressions", get(list_suppressions).post(add_suppressions))
        .route("/v1/companies", get(search_companies))
        .route("/v1/segments/{id}/companies", post(add_companies))
        .route("/v1/segments/{id}/find-people", post(find_people))
        .route("/v1/segments/{id}/write-emails", post(write_emails))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/emails", get(list_emails))
        .route("/v1/emails/{id}/approve", post(approve_email))
        .route("/v1/emails/{id}/skip", post(skip_email))
        .route("/v1/inbox", get(list_inbox))
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

pub(super) async fn list_projects(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<Project>>> {
    let projects = sqlx::query_as::<_, Project>("select * from projects where org_id = $1 order by created_at desc")
        .bind(auth.org_id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(projects))
}

/// A campaign and its funnel, without the targeting the agent works from.
#[derive(Serialize)]
pub(super) struct Campaign {
    id: Uuid,
    name: String,
    description: String,
    status: String,
    companies: i64,
    leads: i64,
    sent: i64,
    contacted: i64,
    replies: i64,
    bounced: i64,
    hot: i64,
    spend_micros: i64,
    created_at: Ts,
}

impl From<SegmentWithStats> for Campaign {
    fn from(s: SegmentWithStats) -> Self {
        let SegmentWithStats { segment, companies, leads, sent, contacted, replies, bounced, hot, spend_micros, .. } = s;
        Self {
            id: segment.id,
            name: segment.name,
            description: segment.description,
            status: segment.status,
            companies,
            leads,
            sent,
            contacted,
            replies,
            bounced,
            hot,
            spend_micros,
            created_at: segment.created_at,
        }
    }
}

pub(super) async fn list_campaigns(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Campaign>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(segments::with_stats(&state.db, id, None).await?.into_iter().map(Campaign::from).collect()))
}

#[derive(Deserialize)]
pub(super) struct CampaignBody {
    #[serde(default)]
    prompt: String,
}

/// Like describing a campaign in the app: the agent names it and fills in its targeting from the prompt.
pub(super) async fn create_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<CampaignBody>,
) -> AppResult<Json<Value>> {
    check_project(&state.db, id, auth.org_id).await?;
    if body.prompt.trim().is_empty() {
        return Err(AppError::BadRequest("Describe the campaign in prompt, e.g. dental clinics in Leeds".into()));
    }
    let (segment, run_id) = segments::insert(&state.db, auth.org_id, id, SegmentBody::from_prompt(body.prompt)).await?;
    Ok(Json(json!({ "campaign_id": segment.id, "run_id": run_id })))
}

pub(super) async fn list_leads(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<leads::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<LeadRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(leads::page_of(&state.db, id, q, &page).await?))
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
        hooks::announce_booked(&state, &lead);
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
/// The org's do-not-contact list, oldest first, so a CRM or another sending tool can copy unsubscribes and bounces. Pages,
/// not a cursor: one paste or POST adds many entries with the same created_at, so `since` alone can't resume mid-batch.
async fn list_suppressions(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SinceQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Suppression>>> {
    let filter = "from suppressions where org_id = $1 and ($2::timestamptz is null or created_at > $2)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {filter}")).bind(auth.org_id).bind(q.since).fetch_one(&state.db).await?;
    let items = sqlx::query_as::<_, Suppression>(&format!("select * {filter} order by created_at, id limit $3 offset $4"))
        .bind(auth.org_id)
        .bind(q.since)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

async fn add_suppressions(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<SuppressionsBody>) -> AppResult<Json<Value>> {
    if body.values.len() > MAX_SUPPRESSIONS_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_SUPPRESSIONS_PER_REQUEST} values per request")));
    }
    let added = suppressions::insert(&state.db, auth.org_id, body.values.iter().map(|v| v.trim()).filter(|v| !v.is_empty())).await?;
    Ok(Json(json!({ "added": added })))
}

pub(super) async fn search_companies(
    State(state): State<AppState>,
    _auth: ApiKeyAuth,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<directory::Page>> {
    Ok(Json(directory::page_of(&state.db, &q, &page).await?))
}

#[derive(Deserialize)]
pub(super) struct AddCompaniesBody {
    #[serde(default)]
    domains: Vec<String>,
}

pub(super) async fn add_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddCompaniesBody>,
) -> AppResult<Json<Added>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(directory::add_domains(&state.db, auth.org_id, segment.project_id, segment.id, &body.domains).await?))
}

pub(super) async fn find_people(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    body: Option<Json<FindPeopleBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let run_id = segments::start_find_people(&state.db, auth.org_id, &segment, body.unwrap_or_default().0).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// Writes a first email for every lead in the campaign that has none, like "Write emails" in the app. Lets a CRM push
/// leads with segment_id and then get them written. In a campaign that's sending, the new emails go straight into the queue.
pub(super) async fn write_emails(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    body: Option<Json<WriteEmailsBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let run_id = segments::start_write_emails(&state.db, auth.org_id, &segment, body.unwrap_or_default().0).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Serialize)]
pub(super) struct RunStatus {
    #[serde(flatten)]
    run: AgentRun,
    /// The latest progress line, or why it failed.
    message: Option<String>,
}

/// For waiting on a run: status goes queued, running, then succeeded, failed or cancelled.
pub(super) async fn get_run(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<RunStatus>> {
    let run = runs::load(&state.db, auth.org_id, id).await?;
    let message = runs::last_message(&state.db, id).await?;
    Ok(Json(RunStatus { run, message }))
}

/// An outbound email with who it's for, without the sending details.
#[derive(Serialize)]
pub(super) struct Email {
    id: Uuid,
    project_id: Uuid,
    segment_id: Option<Uuid>,
    campaign: Option<String>,
    lead_id: Uuid,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    kind: String,
    step: i32,
    subject: String,
    body: String,
    status: String,
    scheduled_for: Option<Ts>,
    created_at: Ts,
}

impl From<EmailRow> for Email {
    fn from(r: EmailRow) -> Self {
        let e = r.email;
        Self {
            id: e.id,
            project_id: e.project_id,
            segment_id: e.segment_id,
            campaign: r.segment_name,
            lead_id: e.lead_id,
            lead_name: r.lead_name,
            lead_email: r.lead_email,
            company_name: r.company_name,
            kind: e.kind,
            step: e.step,
            subject: e.subject,
            body: e.body,
            status: e.status,
            scheduled_for: e.scheduled_for,
            created_at: e.created_at,
        }
    }
}

/// Drafts waiting for approval by default, oldest first, so a reviewer goes through them in the order they were written.
pub(super) async fn list_emails(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(mut q): Query<emails::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Email>>> {
    q.status = q.status.filter(|s| !s.is_empty()).or(Some("draft".into()));
    q.order = Some("soonest".into());
    let Paginated { items, total } = emails::page_of(&state.db, auth.org_id, q, &page).await?;
    Ok(Json(Paginated { items: items.into_iter().map(Email::from).collect(), total }))
}

/// The app's Approve button. In a campaign that isn't sending yet, the email waits in the queue until it starts.
pub(super) async fn approve_email(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Email>> {
    set_email_status(&state, auth, id, "approved").await
}

/// The app's Cancel button: the email never goes out.
pub(super) async fn skip_email(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Email>> {
    set_email_status(&state, auth, id, "cancelled").await
}

async fn set_email_status(state: &AppState, auth: ApiKeyAuth, id: Uuid, status: &str) -> AppResult<Json<Email>> {
    let body = emails::UpdateBody { status: Some(status.into()), ..Default::default() };
    Ok(Json(emails::update_one(&state.db, auth.org_id, id, body).await?.into()))
}

/// A reply thread: the lead and the latest message either way, cut short.
#[derive(Serialize)]
pub(super) struct Thread {
    lead_id: Uuid,
    project_id: Uuid,
    project_name: String,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    lead_status: String,
    classification: Option<String>,
    pending_auto_reply: bool,
    last_message: ThreadMessage,
}

#[derive(Serialize)]
struct ThreadMessage {
    id: Uuid,
    direction: String,
    subject: String,
    snippet: String,
    at: Ts,
}

const SNIPPET_CHARS: usize = 300;

impl From<InboxThread> for Thread {
    fn from(t: InboxThread) -> Self {
        let (lead, m) = (t.lead.lead, t.last_message);
        Self {
            lead_id: lead.id,
            project_id: lead.project_id,
            project_name: t.project_name,
            lead_name: lead.full_name,
            lead_email: lead.email,
            company_name: lead.company_name,
            lead_status: lead.status,
            classification: t.classification,
            pending_auto_reply: t.pending_auto_reply,
            last_message: ThreadMessage {
                id: m.id,
                direction: m.direction,
                subject: m.subject,
                snippet: m.body.trim().chars().take(SNIPPET_CHARS).collect(),
                at: m.sent_at.unwrap_or(m.created_at),
            },
        }
    }
}

/// The Inbox page's threads: filter is all, hot, needs_reply, auto_replied or other.
pub(super) async fn list_inbox(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<inbox::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Thread>>> {
    let Paginated { items, total } = inbox::page_of(&state.db, auth.org_id, q, &page).await?;
    Ok(Json(Paginated { items: items.into_iter().map(Thread::from).collect(), total }))
}
