use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::analytics::{self, Analytics, AnalyticsQuery};
use super::calls::{self, CallRow, LogBody};
use super::directory::{self, Added, SearchQuery};
use super::emails::{self, EmailRow};
use super::inbox::{self, InboxThread};
use super::leads::{self, CompanyRow, LeadDetail, LeadRow};
use super::meetings::{self, MeetingRow};
use super::runs;
use super::segments::{self, FindPeopleBody, SegmentBody, SegmentWithStats, WriteEmailsBody};
use super::sheets::{self, ImportedNow};
use super::stats::{self, Totals};
use super::suppressions;
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::util_input::{self, LEAD_STATUSES};
use super::util_scope;
use crate::ab_test::{self, AbTest};
use crate::auth::ApiKeyAuth;
use crate::deals::{self, Deal, DealFilter, Move, NewDeal, Stage};
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::local_businesses::{self, AddOutcome, Results};
use crate::models::{AgentRun, EmailMessage, Lead, Project, Segment, Suppression, Ts};
use crate::ownership;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/{id}/campaigns", get(list_campaigns).post(create_campaign))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/projects/{id}/leads/upsert", post(upsert_lead))
        .route("/v1/projects/{id}/companies", get(list_companies))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/leads/{id}/assign", post(assign_lead))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/projects/{id}/analytics", get(project_analytics))
        .route("/v1/suppressions", get(list_suppressions).post(add_suppressions))
        .route("/v1/companies", get(search_companies))
        .route("/v1/segments/{id}/companies", post(add_companies))
        .route("/v1/segments/{id}/find-people", post(find_people))
        .route("/v1/local-businesses", get(search_local_businesses))
        .route("/v1/segments/{id}/local-businesses", post(add_local_businesses))
        .route("/v1/segments/{id}/write-emails", post(write_emails))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/emails", get(list_emails))
        .route("/v1/emails/{id}/approve", post(approve_email))
        .route("/v1/emails/{id}/skip", post(skip_email))
        .route("/v1/inbox", get(list_inbox))
        .route("/v1/deals", get(list_deals).post(create_deal))
        .route("/v1/deals/{id}/move", post(move_deal))
        .route("/v1/deal-stages", get(list_stages))
        .route("/v1/projects/{id}/calls", get(list_calls))
        .route("/v1/leads/{id}/calls", post(log_call))
        .route("/v1/leads/{id}/meetings", get(list_meetings))
        .route("/v1/projects/{id}/sheets", get(list_sheets))
        .route("/v1/projects/{id}/sheets/{conn}/import", post(import_sheet))
}

const MAX_LEADS_PER_REQUEST: usize = 1000;
const MAX_MESSAGES: i64 = 500;
const MAX_SUPPRESSIONS_PER_REQUEST: usize = 1000;

pub(super) async fn check_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<()> {
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
    /// The campaign's A/B test of email angles, with results per variant. Null when it has no variants.
    ab_test: Option<AbTest>,
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
            ab_test: None,
        }
    }
}

pub(super) async fn list_campaigns(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Campaign>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let rows = segments::with_stats(&state.db, id, None).await?;
    let mut tests = ab_test::load(&state.db, &rows.iter().map(|r| &r.segment).collect::<Vec<_>>()).await?;
    let campaigns = rows.into_iter().map(|r| {
        let ab_test = tests.remove(&r.segment.id);
        Campaign { ab_test, ..Campaign::from(r) }
    });
    Ok(Json(campaigns.collect()))
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
    Query(mut q): Query<leads::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<LeadRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    q.owner = owner_by_email(&state.db, auth.org_id, q.owner).await?;
    Ok(Json(leads::page_of(&state.db, id, None, q, &page).await?))
}

/// An owner filter may name the member by email here, since a CRM or calendar knows people by their address.
async fn owner_by_email(db: &PgPool, org_id: Uuid, owner: Option<String>) -> AppResult<Option<String>> {
    match owner {
        Some(email) if email.contains('@') => Ok(ownership::resolve_owner(db, org_id, &json!(email)).await?.map(|id| id.to_string())),
        other => Ok(other),
    }
}

/// The project's own companies with their scores. GET /v1/companies searches the company database instead.
pub(super) async fn list_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<leads::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<CompanyRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(leads::companies_page(&state.db, id, q, &page).await?))
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

#[derive(Serialize)]
struct Upserted {
    /// False when a lead with the email was already in the project and got updated.
    created: bool,
    #[serde(flatten)]
    lead: Lead,
}

/// Adds one lead by email, or updates the project's lead with that email, for Zapier, Make and n8n steps. Empty fields keep
/// what the lead has, and segment_id moves it into that campaign.
async fn upsert_lead(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<NewLead>) -> AppResult<Json<Upserted>> {
    check_project(&state.db, id, auth.org_id).await?;
    let Some(email) = util_input::email(&body.email) else {
        let error = if body.email.trim().is_empty() { "Send the lead's email".to_string() } else { format!("Invalid email: {}", body.email.trim()) };
        return Err(AppError::BadRequest(error));
    };
    if let Some(segment_id) = body.segment_id {
        sqlx::query("select 1 from segments where id = $1 and project_id = $2")
            .bind(segment_id)
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("That campaign isn't in this project".into()))?;
    }
    if let Some(lead) = update_by_email(&state.db, id, &email, &body).await? {
        return Ok(Json(Upserted { created: false, lead }));
    }
    let outcome = util_lead_import::insert(&state.db, auth.org_id, id, "api", vec![(1, body)]).await?;
    match (outcome.ids.first(), outcome.errors.into_iter().next()) {
        (Some(&lead_id), _) => {
            let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(&state.db).await?;
            Ok(Json(Upserted { created: true, lead }))
        }
        (None, Some(e)) => Err(AppError::BadRequest(e.error)),
        // Another request added the same email in between, so the lead was updated by neither.
        (None, None) => Err(AppError::Conflict("The lead was added by another request at the same moment. Try again".into())),
    }
}

/// Updates the fields the body fills in on the project's lead with this email, or None when there's no such lead.
async fn update_by_email(db: &PgPool, project_id: Uuid, email: &str, body: &NewLead) -> AppResult<Option<Lead>> {
    let country: Option<String> = sqlx::query_scalar(
        "select coalesce(nullif(l.country_code, ''), p.company ->> 'country_code') from leads l join projects p on p.id = l.project_id
         where l.project_id = $1 and l.email = $2::citext",
    )
    .bind(project_id)
    .bind(email)
    .fetch_optional(db)
    .await?
    .flatten();
    let supplied = util_lead_import::country_code(&body.country_code);
    let phone = crate::phone::normalize(&body.phone, if supplied.is_empty() { country.as_deref().unwrap_or_default() } else { &supplied });
    let text = |v: &str| v.trim().to_string();
    Ok(sqlx::query_as::<_, Lead>(
        "update leads set
           full_name = coalesce(nullif($3, ''), full_name),
           first_name = coalesce(nullif($4, ''), first_name), last_name = coalesce(nullif($5, ''), last_name),
           title = coalesce(nullif($6, ''), title), company_name = coalesce(nullif($7, ''), company_name),
           company_domain = coalesce($8, company_domain), linkedin_url = coalesce(nullif($9, ''), linkedin_url),
           city = coalesce(nullif($10, ''), city), phone = coalesce($11, phone), segment_id = coalesce($12, segment_id),
           country_code = coalesce(nullif($13, ''), country_code)
         where project_id = $1 and email = $2::citext returning *",
    )
    .bind(project_id)
    .bind(email)
    .bind(text(&body.full_name))
    .bind(text(&body.first_name))
    .bind(text(&body.last_name))
    .bind(text(&body.title))
    .bind(text(&body.company_name))
    .bind(util_input::domain(&body.company_domain))
    .bind(text(&body.linkedin_url))
    .bind(text(&body.city))
    .bind(phone)
    .bind(body.segment_id)
    .bind(&supplied)
    .fetch_optional(db)
    .await?)
}

#[derive(Deserialize)]
struct FindQuery {
    #[serde(default)]
    email: String,
    /// Only this project's lead.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// Calendars and CRMs know a person by email, not by lead id. The same person can be a lead in several projects, so this is a list.
async fn find_leads(State(state): State<AppState>, auth: ApiKeyAuth, Query(q): Query<FindQuery>) -> AppResult<Json<Vec<Lead>>> {
    let email = q.email.trim();
    if email.is_empty() {
        return Err(AppError::BadRequest("Add ?email= with the address to look up".into()));
    }
    let leads = sqlx::query_as::<_, Lead>(
        "select l.* from leads l join projects p on p.id = l.project_id
         where p.org_id = $1 and l.email = $2::citext and ($3::uuid is null or l.project_id = $3) order by l.created_at desc limit 100",
    )
    .bind(auth.org_id)
    .bind(email)
    .bind(q.project_id)
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
        ownership::claim(&state.db, lead.id).await;
        crate::deals::booked(&state, &lead).await;
        hooks::announce_booked(&state, &lead);
    }
    Ok(Json(leads::detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
pub(super) struct AssignBody {
    /// A team member's user id or email, or null for nobody.
    #[serde(default)]
    owner: Value,
}

/// Gives the lead to someone on the team, or takes it back with null. A rule never moves it afterwards.
pub(super) async fn assign_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AssignBody>,
) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let owner = ownership::resolve_owner(&state.db, auth.org_id, &body.owner).await?;
    ownership::assign(&state.db, auth.org_id, &[lead.id], owner).await?;
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
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

/// The Analytics page's numbers for a date range: totals against the period before, a daily or weekly series, and
/// breakdowns by campaign, mailbox, step, send time and channel.
pub(super) async fn project_analytics(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<AnalyticsQuery>,
) -> AppResult<Json<Analytics>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(analytics::analytics(&state.db, id, &q).await?))
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

/// Google Maps through the organization's key, like the Companies page's Local businesses tab. With project_id, says which
/// businesses that project already has.
pub(super) async fn search_local_businesses(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<local_businesses::SearchQuery>,
) -> AppResult<Json<Results>> {
    if let Some(project) = q.project_id {
        check_project(&state.db, project, auth.org_id).await?;
    }
    Ok(Json(local_businesses::search(&state, auth.org_id, &q).await?))
}

#[derive(Deserialize)]
pub(super) struct AddLocalBody {
    #[serde(default)]
    place_ids: Vec<String>,
}

pub(super) async fn add_local_businesses(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddLocalBody>,
) -> AppResult<Json<AddOutcome>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(local_businesses::add(&state, auth.org_id, segment.project_id, segment.id, &body.place_ids).await?))
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
    /// The A/B test variant it was written for. Follow-ups keep the first email's.
    variant_id: Option<Uuid>,
    variant: Option<String>,
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
            variant_id: e.variant_id,
            variant: r.variant_name,
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
    owner_id: Option<Uuid>,
    owner_email: Option<String>,
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
        let (owner_email, lead, m) = (t.lead.owner_email, t.lead.lead, t.last_message);
        Self {
            lead_id: lead.id,
            project_id: lead.project_id,
            project_name: t.project_name,
            lead_name: lead.full_name,
            lead_email: lead.email,
            company_name: lead.company_name,
            lead_status: lead.status,
            owner_id: lead.owner_id,
            owner_email,
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
    Query(mut q): Query<inbox::ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Thread>>> {
    q.owner = owner_by_email(&state.db, auth.org_id, q.owner).await?;
    let Paginated { items, total } = inbox::page_of(&state.db, auth.org_id, None, q, &page).await?;
    Ok(Json(Paginated { items: items.into_iter().map(Thread::from).collect(), total }))
}

/// The pipeline's stages in board order, with how many deals each holds.
pub(super) async fn list_stages(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<Stage>>> {
    Ok(Json(deals::stages(&state.db, auth.org_id).await?))
}

/// The organization's deals, the latest first: status is open, won or lost.
pub(super) async fn list_deals(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(f): Query<DealFilter>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Deal>>> {
    if let Some(project_id) = f.project_id {
        check_project(&state.db, project_id, auth.org_id).await?;
    }
    let (items, total) = deals::list(&state.db, auth.org_id, &f, page.limit(), page.offset()).await?;
    Ok(Json(Paginated { items, total }))
}

/// A lead has at most one open deal, so a second one for it is a 409.
pub(super) async fn create_deal(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<NewDeal>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::create(&state, auth.org_id, None, "api", body).await?))
}

/// Moves a deal to a stage by stage_id or stage name. A won stage takes the final value, a lost one the reason.
pub(super) async fn move_deal(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<Move>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::move_to(&state, auth.org_id, id, body).await?))
}

/// Open calls and the ones made in the last 14 days, for a dialer or CRM to work from. `status` is open or done, `owner`
/// a member's id or email, or unassigned.
pub(super) async fn list_calls(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(mut q): Query<calls::ListQuery>,
) -> AppResult<Json<Vec<CallRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    if let Some(owner) = q.owner.as_deref().filter(|o| o.contains('@')) {
        q.owner = ownership::resolve_owner(&state.db, auth.org_id, &json!(owner)).await?.map(|u| u.to_string());
    }
    Ok(Json(calls::rows(&state.db, id, None, &q).await?))
}

/// Logs a call made with the lead, from a dialer or by hand. It closes their open call step if they have one, which moves
/// their sequence on, and the outcome updates the lead like it does in the app.
pub(super) async fn log_call(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<LogBody>) -> AppResult<Json<Value>> {
    Ok(Json(calls::log_for_lead(&state, auth.org_id, id, &body).await?))
}

/// The lead's sales meetings from Fireflies, Fathom, Gong or Zoom, oldest first, with the agent's summary, next steps and
/// suggested stage. Transcripts stay in Splee.
pub(super) async fn list_meetings(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<MeetingRow>>> {
    Ok(Json(meetings::of_lead(&state.db, auth.org_id, id).await?))
}

/// The project's Google Sheets: the tabs leads import from and the ones lead events go to, with how each last went.
pub(super) async fn list_sheets(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<crate::sheets::Connection>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(crate::sheets::Connection::list(&state.db, id).await?))
}

/// Reads an import tab now and adds its new rows as leads, like "Import now" on the integrations page.
pub(super) async fn import_sheet(State(state): State<AppState>, auth: ApiKeyAuth, Path((id, conn)): Path<(Uuid, Uuid)>) -> AppResult<Json<ImportedNow>> {
    Ok(Json(sheets::import_for(&state, auth.org_id, id, conn).await?))
}
