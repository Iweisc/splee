use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::analytics::{self, Analytics, AnalyticsQuery};
use super::attachments;
use super::calls::{self, CallRow, LogBody};
use super::directory::{self, Added, SearchQuery};
use super::emails::{self, EmailRow};
use super::inbox::{self, InboxThread, ReplyExtras, ThreadMessage as WithFiles};
use super::leads::{self, CompanyRow, LeadDetail, LeadRow};
use super::lookalikes::{self as lookalike_routes, AddBody as AddLookalikesBody};
use super::meetings::{self, MeetingRow};
use super::research;
use super::runs;
use super::segments::{self, FindPeopleBody, SegmentBody, SegmentWithStats, WriteEmailsBody};
use super::sheets::{self, ImportedNow};
use super::stats::{self, Totals};
use super::suppression_lists::{self as lists, Kind, ScopeBody};
use super::suppressions;
use super::util_lead_import::{self, NewLead};
use super::util_pagination::{PageQuery, Paginated};
use super::util_input::{self, LEAD_STATUSES};
use super::util_idempotency;
use super::util_scope;
use crate::ab_test::{self, AbTest};
use crate::auth::{self, ApiKeyAuth, KeyAuth};
use crate::deals::{self, Deal, DealFilter, Move, NewDeal, Stage};
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::jobchanges::{self, AddBody, Overview};
use crate::local_businesses::{self, AddOutcome, Results};
use crate::lookalikes::{self, Lookalikes};
use crate::models::{AgentRun, EmailAttachment, EmailMessage, Lead, Project, Segment, Suppression, SuppressionList, Ts};
use crate::ownership;
use crate::state::AppState;

pub fn router(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects))
        .route("/v1/projects/{id}/campaigns", get(list_campaigns).post(create_campaign))
        .route("/v1/campaigns/{id}", patch(update_campaign))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/projects/{id}/leads/upsert", post(upsert_lead))
        .route("/v1/projects/{id}/companies", get(list_companies))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/leads/{id}/assign", post(assign_lead))
        .route("/v1/leads/{id}/thread", get(lead_thread))
        .route("/v1/leads/{id}/reply", post(reply_to_lead))
        .route("/v1/attachments", post(upload_attachment).layer(DefaultBodyLimit::max(attachments::UPLOAD_BODY_LIMIT)))
        .route("/v1/attachments/{id}", get(download_attachment))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/email-events", get(email_events))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/projects/{id}/analytics", get(project_analytics))
        .route("/v1/suppressions", get(list_suppressions).post(add_suppressions))
        .route("/v1/suppression-lists", get(list_suppression_lists))
        .route("/v1/suppression-lists/people", post(add_people_to_list))
        .route("/v1/suppression-lists/companies", post(add_companies_to_list))
        .route("/v1/suppression-lists/{id}", patch(rescope_suppression_list).delete(delete_suppression_list))
        .route("/v1/suppression-lists/{id}/entries", get(suppression_list_entries))
        .route("/v1/companies", get(search_companies))
        .route("/v1/segments/{id}/companies", post(add_companies))
        .route("/v1/projects/{id}/lookalikes", get(list_lookalikes).post(add_lookalikes))
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
        .route("/v1/projects/{id}/job-changes", get(list_job_changes))
        .route("/v1/projects/{id}/signals", get(list_signals))
        .route("/v1/projects/{id}/research", get(list_research))
        .route("/v1/projects/{id}/research/questions", post(add_research_question))
        .route("/v1/projects/{id}/research/run", post(start_research))
        .route("/v1/projects/{id}/watch-list", post(add_to_watch_list))
        .route("/v1/exports", get(list_exports))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard))
}

/// Every /v1 request counts against its organization's rate limit, and a write with an Idempotency-Key runs at most once.
/// A request without a live key goes through untouched, for its handler to answer 401.
async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let found = match auth::bearer(req.headers()) {
        Some(key) => KeyAuth::find(&state.db, key).await,
        None => Ok(None),
    };
    let key = match found {
        Ok(Some(key)) => key,
        Ok(None) => return next.run(req).await,
        Err(e) => return AppError::from(e).into_response(),
    };
    let permit = match state.rate_limits.acquire(key.org_id) {
        Ok(permit) => permit,
        Err(refused) => return refused.into_response(),
    };
    let org_id = key.org_id;
    req.extensions_mut().insert(key);
    let mut res = util_idempotency::run(&state.db, org_id, req, next).await;
    permit.quota.stamp(res.headers_mut());
    res
}

const MAX_LEADS_PER_REQUEST: usize = 1000;
const MAX_MESSAGES: i64 = 500;
const MAX_SUPPRESSIONS_PER_REQUEST: usize = 1000;
const MAX_LIST_ENTRIES_PER_REQUEST: usize = 10_000;

pub(super) async fn check_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<()> {
    sqlx::query("select 1 from projects where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(())
}

#[derive(Serialize, FromRow)]
pub(super) struct ApiProject {
    #[serde(flatten)]
    #[sqlx(flatten)]
    project: Project,
    /// Hours a draft waits for review before it goes out on its own, or null when drafts wait for a person.
    review_window_hours: Option<i32>,
}

pub(super) async fn list_projects(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<ApiProject>>> {
    let projects = sqlx::query_as::<_, ApiProject>(
        "select p.*, s.review_window_hours from projects p left join project_settings s on s.project_id = p.id
         where p.org_id = $1 order by p.created_at desc",
    )
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
    /// "lead" (the default) for each lead's own language, or a code from language::LANGUAGES.
    language: String,
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
            language: segment.language,
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
    language: Option<String>,
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
    let (segment, run_id) = segments::insert(&state.db, auth.org_id, id, SegmentBody::from_prompt(body.prompt, body.language)).await?;
    Ok(Json(json!({ "campaign_id": segment.id, "run_id": run_id })))
}

#[derive(Deserialize)]
pub(super) struct CampaignUpdate {
    language: String,
}

/// Sets the language the campaign writes in. Emails already written keep theirs until they're rewritten.
pub(super) async fn update_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<CampaignUpdate>,
) -> AppResult<Json<Campaign>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let language = body.language.trim();
    segments::check_language(language)?;
    sqlx::query("update segments set language = $2 where id = $1").bind(segment.id).bind(language).execute(&state.db).await?;
    let row = segments::with_stats(&state.db, segment.project_id, Some(segment.id)).await?.pop().ok_or(AppError::NotFound)?;
    Ok(Json(Campaign::from(row)))
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
           country_code = coalesce(nullif($13, ''), country_code), language = coalesce(nullif($14, ''), language)
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
    .bind(crate::language::parse(&body.language).map_or("", |l| l.code))
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
pub(super) struct LeadUpdateBody {
    status: Option<String>,
    notes: Option<String>,
    /// A code from language::LANGUAGES or its name, or "" to go back to their country and site.
    language: Option<String>,
}

/// Lets a CRM or calendar mark a lead, e.g. meeting_booked, write its notes or set its language. Follow-ups only go to
/// contacted leads, so any other status stops them.
pub(super) async fn update_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<LeadUpdateBody>,
) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    if body.status.is_none() && body.notes.is_none() && body.language.is_none() {
        return Err(AppError::BadRequest("Send status, notes, language or a mix".into()));
    }
    if let Some(status) = &body.status {
        util_input::one_of("status", status, LEAD_STATUSES)?;
    }
    let notes = util_input::notes(body.notes.as_deref())?;
    let language = body.language.as_deref().map(crate::language::lead_setting).transpose().map_err(AppError::BadRequest)?;
    let before = lead.status;
    let lead = sqlx::query_as::<_, Lead>(
        "update leads set status = coalesce($2, status), notes = coalesce($3, notes), language = coalesce($4, language) where id = $1 returning *",
    )
    .bind(lead.id)
    .bind(&body.status)
    .bind(notes)
    .bind(language)
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
pub(super) struct SinceQuery {
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

/// An open or a click that counted, without the scanners and link checks tracking.rs filters out.
#[derive(Serialize, FromRow, Debug)]
pub(super) struct EmailEvent {
    id: Uuid,
    /// "email.opened" or "email.clicked", like the webhooks.
    event: String,
    email_id: Uuid,
    lead_id: Uuid,
    /// The clicked link, null for an open.
    url: Option<String>,
    at: Ts,
}

/// The polling twin of the email.opened and email.clicked webhooks: every counted open and click, oldest first.
pub(super) async fn email_events(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
) -> AppResult<Json<Vec<EmailEvent>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let events = sqlx::query_as::<_, EmailEvent>(
        "select h.id, case h.kind when 'open' then 'email.opened' else 'email.clicked' end as event, h.email_id, h.lead_id, k.url,
                h.created_at as at
         from tracking_hits h left join tracked_links k on k.token = h.link_token
         where h.project_id = $1 and h.filter_reason is null and ($2::timestamptz is null or h.created_at > $2)
         order by h.created_at limit $3",
    )
    .bind(id)
    .bind(q.since)
    .bind(MAX_MESSAGES)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(events))
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

async fn list_suppression_lists(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<SuppressionList>>> {
    Ok(Json(lists::all(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct ListPerson {
    email: String,
}

#[derive(Deserialize)]
struct PeopleListBody {
    list: String,
    people: Vec<ListPerson>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

#[derive(Deserialize)]
struct CompaniesListBody {
    list: String,
    domains: Vec<String>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

fn check_list_size(n: usize) -> AppResult<()> {
    if n > MAX_LIST_ENTRIES_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_LIST_ENTRIES_PER_REQUEST} entries per request")));
    }
    Ok(())
}

/// Adds people to the named list, making it when the org has none by that name. Sends to them stop in the projects it covers.
async fn add_people_to_list(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<PeopleListBody>) -> AppResult<Json<lists::Added>> {
    check_list_size(body.people.len())?;
    let emails = body.people.iter().map(|p| p.email.as_str());
    Ok(Json(lists::add_by_name(&state.db, auth.org_id, &body.list, Kind::People, body.project_ids, emails).await?))
}

async fn add_companies_to_list(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<CompaniesListBody>) -> AppResult<Json<lists::Added>> {
    check_list_size(body.domains.len())?;
    let domains = body.domains.iter().map(String::as_str);
    Ok(Json(lists::add_by_name(&state.db, auth.org_id, &body.list, Kind::Companies, body.project_ids, domains).await?))
}

async fn rescope_suppression_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ScopeBody>,
) -> AppResult<Json<SuppressionList>> {
    Ok(Json(lists::set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

async fn delete_suppression_list(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    lists::delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct OffsetQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

async fn suppression_list_entries(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<OffsetQuery>,
) -> AppResult<Json<Paginated<String>>> {
    let (limit, offset) = (q.limit.unwrap_or(100).clamp(1, 1000), q.offset.unwrap_or(0).max(0));
    Ok(Json(lists::entries_page(&state.db, auth.org_id, id, limit, offset).await?))
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

#[derive(Deserialize)]
pub(super) struct LookalikesQuery {
    /// Comma-separated domains to match instead of the project's seeds.
    seeds: Option<String>,
}

/// Companies in the database like the project's customers, with the profile they were matched on, like the Companies page's
/// "Like your customers".
pub(super) async fn list_lookalikes(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<LookalikesQuery>,
) -> AppResult<Json<Lookalikes>> {
    check_project(&state.db, id, auth.org_id).await?;
    let seeds: Option<Vec<String>> = q.seeds.map(|s| s.split(',').map(String::from).collect());
    let seeds = seeds.as_deref().map(lookalike_routes::domains).transpose()?.filter(|s| !s.is_empty());
    Ok(Json(lookalikes::find(&state.db, id, seeds.as_deref()).await?))
}

pub(super) async fn add_lookalikes(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddLookalikesBody>,
) -> AppResult<Json<lookalike_routes::Added>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(lookalike_routes::add_matches(&state, auth.org_id, id, body).await?))
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
    /// "person" or "review_window" once approved.
    approved_via: Option<String>,
    /// When a draft goes out on its own if nobody reviews it.
    sends_at: Option<Ts>,
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
            approved_via: e.approved_via,
            sends_at: r.sends_at,
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

/// One email of a lead's thread, either way, with who it copied in and its files. Download a file with GET
/// /v1/attachments/{id} unless `stored` is false, for a lead's file over 25 MB that Splee didn't keep.
#[derive(Serialize)]
pub(super) struct LeadThreadMessage {
    id: Uuid,
    direction: String,
    kind: String,
    channel: String,
    status: String,
    subject: String,
    body: String,
    from_email: Option<String>,
    to_email: Option<String>,
    cc_emails: Vec<String>,
    classification: Option<String>,
    attachments: Vec<EmailAttachment>,
    sent_at: Option<Ts>,
    created_at: Ts,
}

impl From<WithFiles> for LeadThreadMessage {
    fn from(WithFiles { message: m, attachments }: WithFiles) -> Self {
        Self {
            id: m.id,
            direction: m.direction,
            kind: m.kind,
            channel: m.channel,
            status: m.status,
            subject: m.subject,
            body: m.body,
            from_email: m.from_email,
            to_email: m.to_email,
            cc_emails: m.cc_emails,
            classification: m.classification,
            attachments,
            sent_at: m.sent_at,
            created_at: m.created_at,
        }
    }
}

#[derive(Serialize)]
pub(super) struct LeadThread {
    lead_id: Uuid,
    /// Oldest first.
    messages: Vec<LeadThreadMessage>,
    /// Who a reply copies in when it doesn't name anyone: everyone the lead copied on their last message, minus your own
    /// mailboxes and the do-not-contact list.
    reply_cc: Vec<String>,
}

/// The lead's emails both ways, like the inbox's thread.
pub(super) async fn lead_thread(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<LeadThread>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let emails = sqlx::query_as::<_, EmailMessage>("select * from email_messages where lead_id = $1 order by created_at")
        .bind(lead.id)
        .fetch_all(&state.db)
        .await?;
    let reply_cc = match emails.iter().rfind(|m| m.direction == "inbound") {
        Some(inbound) => inbox::reply_all_cc(&state.db, auth.org_id, &lead, inbound).await?,
        None => vec![],
    };
    let messages = inbox::with_attachments(&state.db, emails).await?.into_iter().map(LeadThreadMessage::from).collect();
    Ok(Json(LeadThread { lead_id: lead.id, messages, reply_cc }))
}

#[derive(Deserialize)]
pub(super) struct ReplyBody {
    body_text: String,
    #[serde(flatten)]
    extras: ReplyExtras,
}

/// The inbox's Send: queues a reply to the lead's last message, which goes out on the sender's next pass.
pub(super) async fn reply_to_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ReplyBody>,
) -> AppResult<Json<LeadThreadMessage>> {
    let queued = inbox::send_reply(&state.db, auth.org_id, id, &body.body_text, &body.extras).await?;
    let mut with_files = inbox::with_attachments(&state.db, vec![queued]).await?;
    Ok(Json(with_files.remove(0).into()))
}

/// A multipart form with the file as `file`, kept for a day for a reply's attachment_ids.
pub(super) async fn upload_attachment(State(state): State<AppState>, auth: ApiKeyAuth, form: Multipart) -> AppResult<Json<EmailAttachment>> {
    Ok(Json(attachments::upload_to(&state.db, auth.org_id, form).await?))
}

pub(super) async fn download_attachment(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Response> {
    attachments::download_from(&state.db, auth.org_id, id).await
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

/// Moves seen on LinkedIn among the people the project watches, newest first, with the lead Splee made at each new company,
/// and how many people it watches.
pub(super) async fn list_job_changes(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Overview>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(jobchanges::overview(&state.db, id).await?))
}

/// Funding filings and company news at the project's companies, newest first, like the Companies page's News tab.
pub(super) async fn list_signals(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<super::signals::SignalsQuery>,
) -> AppResult<Json<Vec<crate::signals::Listed>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(super::signals::listed(&state.db, id, &q).await?))
}

/// The project's research questions and each company's answers, best score first, like the Companies page's Research view.
pub(super) async fn list_research(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<research::GridQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<research::Grid>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::grid_page(&state.db, id, &q, &page).await?))
}

pub(super) async fn add_research_question(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<research::QuestionBody>,
) -> AppResult<Json<crate::models::ResearchQuestion>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::create(&state.db, id, body).await?))
}

/// Queues the agent to answer the project's research questions for its companies.
pub(super) async fn start_research(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<research::StartBody>,
) -> AppResult<Json<research::Started>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::start_research(&state.db, id, auth.org_id, body).await?))
}

/// Adds past customers or champions to the project's watch list, from a list or a CSV, like the Job changes tab.
pub(super) async fn add_to_watch_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddBody>,
) -> AppResult<Json<jobchanges::Added>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(jobchanges::add(&state.db, id, body).await?))
}

/// The organization's last 30 data exports, newest first, with the files each wrote. See exports/.
pub(super) async fn list_exports(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<crate::exports::Run>>> {
    Ok(Json(crate::exports::recent(&state.db, auth.org_id, 30).await?))
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use reqwest::Method;
    use reqwest::header::HeaderMap;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::crypto::sha256_hex;
    use crate::rate_limit::{LIMITS, Limits, RateLimiter};
    use crate::state::AppState;

    const KEYS: [&str; 2] = ["splee_idempotency_a", "splee_idempotency_b"];

    #[derive(Clone)]
    struct Api {
        base: String,
        client: reqwest::Client,
    }

    impl Api {
        async fn start(db: &PgPool, limits: Limits) -> Self {
            let state = AppState { rate_limits: RateLimiter::new(limits), ..crate::autopilot::tests::state(db.clone()) };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router(&state)).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, client: reqwest::Client::new() }
        }

        async fn send(&self, method: Method, key: &str, path: &str, idempotency_key: Option<&str>, body: Option<Value>) -> (u16, HeaderMap, Value) {
            let mut req = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(key);
            if let Some(k) = idempotency_key {
                req = req.header("idempotency-key", k);
            }
            if let Some(body) = body {
                req = req.json(&body);
            }
            let res = req.send().await.unwrap();
            let (status, headers) = (res.status().as_u16(), res.headers().clone());
            (status, headers, res.json().await.unwrap_or(Value::Null))
        }

        async fn post(&self, key: &str, path: &str, idempotency_key: Option<&str>, body: Value) -> (u16, HeaderMap, Value) {
            self.send(Method::POST, key, path, idempotency_key, Some(body)).await
        }
    }

    /// Two organizations, each with a project and an API key from KEYS.
    async fn seed(db: &PgPool) -> [Uuid; 2] {
        let mut projects = vec![];
        for (name, key) in [("Acme", KEYS[0]), ("Other", KEYS[1])] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                .bind(org)
                .bind(name)
                .fetch_one(db)
                .await
                .unwrap();
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'test', 'splee_idempo', $2)")
                .bind(org)
                .bind(sha256_hex(key))
                .execute(db)
                .await
                .unwrap();
            projects.push(project);
        }
        [projects[0], projects[1]]
    }

    async fn count(db: &PgPool, sql: &str) -> i64 {
        sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
    }

    fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
        headers.get(name).and_then(|v| v.to_str().ok())
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_retry_with_the_same_key_replays_the_first_response_and_runs_nothing(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let campaigns = format!("/v1/projects/{a}/campaigns");
        let leeds = json!({ "prompt": "dental clinics in Leeds" });

        let (status, headers, first) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None), "{first}");
        let (status, headers, again) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
        assert_eq!((status, header(&headers, "idempotent-replayed"), &again), (200, Some("true"), &first));
        assert_eq!(header(&headers, "content-type"), Some("application/json"));
        let made = count(&db, "select (select count(*) from segments) + (select count(*) from agent_runs)").await;
        assert_eq!(made, 2, "one campaign and its run");

        // The same key on another request is a client bug, never a replay.
        let (status, _, body) = api.post(KEYS[0], &campaigns, Some("order-1"), json!({ "prompt": "gyms in York" })).await;
        assert_eq!(status, 422, "{body}");
        assert!(body["error"].as_str().unwrap().contains("different request"));
        assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{a}/leads"), Some("order-1"), leeds.clone()).await.0, 422);
        let campaign = first["campaign_id"].as_str().unwrap();
        assert_eq!(api.send(Method::PATCH, KEYS[0], &format!("/v1/campaigns/{campaign}"), Some("order-1"), Some(leeds.clone())).await.0, 422);

        // Keys are per organization.
        let (status, headers, theirs) = api.post(KEYS[1], &format!("/v1/projects/{b}/campaigns"), Some("order-1"), leeds.clone()).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
        assert_ne!(theirs["campaign_id"], first["campaign_id"]);

        // A 4xx is kept too, so a retry gets the same answer.
        let (status, _, refused) = api.post(KEYS[0], &campaigns, Some("order-2"), json!({ "prompt": " " })).await;
        assert_eq!(status, 400);
        let (status, headers, replayed) = api.post(KEYS[0], &campaigns, Some("order-2"), json!({ "prompt": " " })).await;
        assert_eq!((status, header(&headers, "idempotent-replayed"), replayed), (400, Some("true"), refused));
        assert_eq!(api.post(KEYS[0], &campaigns, Some(&"k".repeat(256)), leeds.clone()).await.0, 400);

        // Without the header nothing changes: each request runs and nothing is kept.
        let before = count(&db, "select count(*) from idempotency_keys").await;
        for _ in 0..2 {
            let (status, headers, _) = api.post(KEYS[0], &campaigns, None, leeds.clone()).await;
            assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
        }
        assert_eq!(count(&db, "select count(*) from segments").await, 4);
        assert_eq!(count(&db, "select count(*) from idempotency_keys").await, before);
        // A read ignores the header.
        let (status, headers, _) = api.send(Method::GET, KEYS[0], &campaigns, Some("order-1"), None).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));

        sqlx::query("update idempotency_keys set expires_at = now() - interval '1 second'").execute(&db).await.unwrap();
        crate::agent::expire_visitor_data(&db).await.unwrap();
        assert_eq!(count(&db, "select count(*) from idempotency_keys").await, 0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_duplicate_while_the_first_runs_is_409_and_a_5xx_releases_the_key(db: PgPool) {
        let [a, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let campaigns = format!("/v1/projects/{a}/campaigns");
        let leeds = json!({ "prompt": "dental clinics in Leeds" });

        // Holds the first request at its insert into segments, after it claimed the key.
        let mut lock = db.begin().await.unwrap();
        sqlx::query("lock table segments in exclusive mode").execute(&mut *lock).await.unwrap();
        let first = tokio::spawn({
            let (api, campaigns, leeds) = (api.clone(), campaigns.clone(), leeds.clone());
            async move { api.post(KEYS[0], &campaigns, Some("order-1"), leeds).await }
        });
        for _ in 0..100 {
            if count(&db, "select count(*) from idempotency_keys where status is null").await == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let (status, _, body) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
        assert_eq!(status, 409, "{body}");
        lock.rollback().await.unwrap();
        let (status, _, made) = first.await.unwrap();
        assert_eq!(status, 200, "{made}");
        let (status, headers, again) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
        assert_eq!((status, header(&headers, "idempotent-replayed"), again), (200, Some("true"), made));

        // A 5xx isn't kept, so the retry runs.
        let refuse = "create function refuse() returns trigger language plpgsql as $$ begin raise exception 'refused'; end $$";
        sqlx::query(refuse).execute(&db).await.unwrap();
        sqlx::query("create trigger refuse before insert on segments for each statement execute function refuse()").execute(&db).await.unwrap();
        assert_eq!(api.post(KEYS[0], &campaigns, Some("order-2"), leeds.clone()).await.0, 500);
        assert_eq!(count(&db, "select count(*) from idempotency_keys where key = 'order-2'").await, 0);
        sqlx::query("drop trigger refuse on segments").execute(&db).await.unwrap();
        let (status, headers, _) = api.post(KEYS[0], &campaigns, Some("order-2"), leeds).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
        assert_eq!(count(&db, "select count(*) from segments").await, 2);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_organization_gets_its_hour_of_requests_then_a_429(db: PgPool) {
        seed(&db).await;
        let api = Api::start(&db, Limits { per_hour: 2, in_flight: 150 }).await;

        for remaining in ["1", "0"] {
            let (status, headers, _) = api.send(Method::GET, KEYS[0], "/v1/projects", None, None).await;
            assert_eq!((status, header(&headers, "ratelimit-limit"), header(&headers, "ratelimit-remaining")), (200, Some("2"), Some(remaining)));
            let reset: u64 = header(&headers, "ratelimit-reset").unwrap().parse().unwrap();
            assert!((3500..=3600).contains(&reset), "{reset}");
        }
        let (status, headers, body) = api.send(Method::GET, KEYS[0], "/v1/projects", None, None).await;
        assert_eq!((status, header(&headers, "ratelimit-remaining")), (429, Some("0")));
        assert!(header(&headers, "retry-after").unwrap().parse::<u64>().unwrap() > 3500);
        assert!(body["error"].as_str().unwrap().contains("2 API requests this hour"), "{body}");

        let (status, headers, _) = api.send(Method::GET, KEYS[1], "/v1/projects", None, None).await;
        assert_eq!((status, header(&headers, "ratelimit-remaining")), (200, Some("1")), "another organization has its own hour");
        let (status, headers, _) = api.send(Method::GET, "splee_wrong", "/v1/projects", None, None).await;
        assert_eq!((status, header(&headers, "ratelimit-limit")), (401, None));
    }
}
