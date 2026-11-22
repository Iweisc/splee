use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{SinceQuery, check_project, owner_by_email};
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{EmailMessage, Lead, Ts};
use crate::ownership;
use crate::routes::analytics::{self, Analytics, AnalyticsQuery};
use crate::routes::leads::{self, CompanyRow, LeadDetail, LeadRow};
use crate::routes::segments::{self, Archived, SegmentWithStats};
use crate::routes::stats::{self, Totals};
use crate::routes::util_input::{self, LEAD_STATUSES};
use crate::routes::util_lead_import::{self, NewLead};
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::routes::util_scope;
use crate::state::AppState;

const MAX_LEADS_PER_REQUEST: usize = 1000;
const MAX_MESSAGES: i64 = 500;

pub(in crate::routes) async fn list_leads(
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

/// The project's own companies with their scores. GET /v1/companies searches the company database instead.
pub(in crate::routes) async fn list_companies(
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
pub(super) struct AddLeadsBody {
    leads: Vec<NewLead>,
}

pub(super) async fn add_leads(
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
pub(super) struct Upserted {
    /// False when a lead with the email was already in the project and got updated.
    created: bool,
    #[serde(flatten)]
    lead: Lead,
}

/// Adds one lead by email, or updates the project's lead with that email, for Zapier, Make and n8n steps. Empty fields keep
/// what the lead has, and segment_id moves it into that campaign.
pub(super) async fn upsert_lead(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<NewLead>,
) -> AppResult<Json<Upserted>> {
    check_project(&state.db, id, auth.org_id).await?;
    let Some(email) = util_input::email(&body.email) else {
        let error = if body.email.trim().is_empty() { "Send the lead's email".to_string() } else { format!("Invalid email: {}", body.email.trim()) };
        return Err(AppError::BadRequest(error));
    };
    crate::routes::imports::check_segment(&state, id, body.segment_id).await?;
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
pub(super) struct FindQuery {
    #[serde(default)]
    email: String,
    /// Only this project's lead.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// Calendars and CRMs know a person by email, not by lead id. The same person can be a lead in several projects, so this is a list.
pub(super) async fn find_leads(State(state): State<AppState>, auth: ApiKeyAuth, Query(q): Query<FindQuery>) -> AppResult<Json<Vec<Lead>>> {
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

pub(super) async fn get_lead(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(leads::detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
pub(in crate::routes) struct LeadUpdateBody {
    status: Option<String>,
    notes: Option<String>,
    /// A code from language::LANGUAGES or its name, or "" to go back to their country and site.
    language: Option<String>,
}

/// Lets a CRM or calendar mark a lead, e.g. meeting_booked, write its notes or set its language. Follow-ups only go to
/// contacted leads, so any other status stops them.
pub(in crate::routes) async fn update_lead(
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
pub(in crate::routes) struct AssignBody {
    /// A team member's user id or email, or null for nobody.
    #[serde(default)]
    owner: Value,
}

/// Gives the lead to someone on the team, or takes it back with null. A rule never moves it afterwards.
pub(in crate::routes) async fn assign_lead(
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

pub(super) async fn replies(
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

#[derive(Deserialize)]
pub(in crate::routes) struct HotLeadsQuery {
    /// Only leads that became hot after this. Pass the newest became_hot_at you've seen.
    since: Option<Ts>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// A lead whose reply the classifier called hot, with that first hot reply.
#[derive(Serialize, FromRow)]
pub(in crate::routes) struct HotLead {
    lead_id: Uuid,
    project_id: Uuid,
    campaign_id: Option<Uuid>,
    campaign: Option<String>,
    name: String,
    email: Option<String>,
    title: String,
    company_name: String,
    company_domain: Option<String>,
    linkedin_url: String,
    /// Their status now: hot, or meeting_booked and on from there.
    status: String,
    reply_id: Uuid,
    /// The reply that made them hot.
    reply: String,
    /// Why the classifier called it hot.
    why_hot: String,
    /// When that reply arrived.
    became_hot_at: Ts,
}

/// The first hot reply of each of the organization's leads, over `email_messages e` and `projects p`. The first, so a later
/// hot reply from the same lead doesn't bring them back as new.
const FIRST_HOT: &str = "select distinct on (e.lead_id) e.lead_id, e.id as reply_id, e.body as reply, e.classification_reason as why_hot,
           e.created_at as became_hot_at
         from email_messages e join projects p on p.id = e.project_id
         where p.org_id = $1 and e.direction = 'inbound' and e.classification = 'hot' and ($3::uuid is null or e.project_id = $3)
         order by e.lead_id, e.created_at, e.id";

/// Every hot lead across the organization, newest first, for following them without re-listing each project's inbox.
pub(in crate::routes) async fn hot_leads(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<HotLeadsQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<HotLead>>> {
    if let Some(project) = q.project_id {
        check_project(&state.db, project, auth.org_id).await?;
    }
    let filter = "($2::timestamptz is null or h.became_hot_at > $2)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from ({FIRST_HOT}) h where {filter}"))
        .bind(auth.org_id)
        .bind(q.since)
        .bind(q.project_id)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, HotLead>(&format!(
        "select h.*, l.project_id, l.segment_id as campaign_id, s.name as campaign, l.full_name as name, l.email::text as email, l.title,
                l.company_name, l.company_domain, l.linkedin_url, l.status
         from ({FIRST_HOT}) h join leads l on l.id = h.lead_id left join segments s on s.id = l.segment_id
         where {filter} order by h.became_hot_at desc, h.lead_id desc limit $4 offset $5"
    ))
    .bind(auth.org_id)
    .bind(q.since)
    .bind(q.project_id)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Paginated { items, total }))
}

#[derive(Deserialize)]
pub(in crate::routes) struct ChangesQuery {
    /// next_cursor from the last call.
    cursor: Option<String>,
    /// On the first call, only changes after this. Without either, from the first one.
    since: Option<Ts>,
    limit: Option<i64>,
}

impl ChangesQuery {
    /// The (time, id) the page starts after, None for the first.
    fn after(&self) -> AppResult<Option<(Ts, Uuid)>> {
        Ok(match (self.cursor.as_deref().filter(|c| !c.is_empty()), self.since) {
            (Some(c), _) => Some(parse_cursor(c)?),
            // The largest id, so a change at exactly `since` counts as before it, like the other since filters.
            (None, Some(since)) => Some((since, Uuid::from_u128(u128::MAX))),
            (None, None) => None,
        })
    }

    fn limit(&self) -> i64 {
        self.limit.unwrap_or(100).clamp(1, MAX_CHANGES)
    }
}

/// A reply that came in, by email or to a text.
#[derive(Serialize, FromRow)]
pub(in crate::routes) struct InboundReply {
    id: Uuid,
    lead_id: Uuid,
    campaign_id: Option<Uuid>,
    lead_name: String,
    lead_email: Option<String>,
    company_name: String,
    channel: String,
    from_email: Option<String>,
    subject: String,
    body: String,
    /// hot, question, not_interested, ooo, unsubscribe, bounce or other. Null while the classifier hasn't read it yet.
    classification: Option<String>,
    received_at: Ts,
}

#[derive(Serialize)]
pub(in crate::routes) struct Changes<T> {
    /// Oldest first.
    items: Vec<T>,
    /// Pass it as cursor next time. Null only when there's been no change yet and no cursor or since to keep.
    next_cursor: Option<String>,
    /// More are waiting now, so call again straight away.
    has_more: bool,
}

impl<T> Changes<T> {
    /// `items` were fetched with limit + 1, so one more than `limit` says there are more. `at` is the (time, id) they're ordered by.
    fn page(mut items: Vec<T>, q: &ChangesQuery, at: impl Fn(&T) -> (Ts, Uuid)) -> AppResult<Self> {
        let has_more = items.len() as i64 > q.limit();
        items.truncate(q.limit() as usize);
        let next = items.last().map(at).or(q.after()?);
        Ok(Self { items, next_cursor: next.map(|(at, id)| cursor(at, id)), has_more })
    }
}

/// Changes are read this long after they happen, so one whose insert started first but committed last is never skipped.
const SETTLE: &str = "5 seconds";
const MAX_CHANGES: i64 = 500;

/// A place in a list of changes, after (time, id). Opaque to clients.
fn cursor(at: Ts, id: Uuid) -> String {
    format!("{}_{id}", at.timestamp_micros())
}

fn parse_cursor(cursor: &str) -> AppResult<(Ts, Uuid)> {
    let parsed: Option<(Ts, Uuid)> = cursor.split_once('_').and_then(|(micros, id)| Some((Ts::from_timestamp_micros(micros.parse().ok()?)?, id.parse().ok()?)));
    parsed.ok_or_else(|| AppError::BadRequest("cursor isn't one this API gave out. Pass next_cursor from the last call".into()))
}

/// New replies since the cursor, oldest first, each exactly once: they're ordered by when they arrived and then by id, and the
/// cursor is the last one returned.
#[derive(Deserialize)]
pub(in crate::routes) struct CampaignFilter {
    /// Only this campaign's.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
}

pub(in crate::routes) async fn inbox_changes(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<ChangesQuery>,
    Query(campaign): Query<CampaignFilter>,
) -> AppResult<Json<Changes<InboundReply>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let after = q.after()?;
    let items = sqlx::query_as::<_, InboundReply>(&format!(
        "select e.id, e.lead_id, e.segment_id as campaign_id, l.full_name as lead_name, l.email::text as lead_email, l.company_name, e.channel,
                e.from_email::text as from_email, e.subject, e.body, e.classification, e.created_at as received_at
         from email_messages e join leads l on l.id = e.lead_id
         where e.project_id = $1 and e.direction = 'inbound' and e.created_at <= now() - interval '{SETTLE}'
           and ($2::timestamptz is null or (e.created_at, e.id) > ($2, $3)) and ($5::uuid is null or e.segment_id = $5)
         order by e.created_at, e.id limit $4"
    ))
    .bind(id)
    .bind(after.map(|(at, _)| at))
    .bind(after.map(|(_, id)| id))
    .bind(q.limit() + 1)
    .bind(campaign.segment_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Changes::page(items, &q, |m| (m.received_at, m.id))?))
}

/// A lead someone deleted or forgot, by the ids an integration got it with. Its name and address are already gone.
#[derive(Serialize, FromRow)]
pub(in crate::routes) struct ErasedLead {
    lead_id: Uuid,
    project_id: Uuid,
    campaign_id: Option<Uuid>,
    /// The app's Forget rather than Delete, which also put the address on the do-not-contact list.
    forgotten: bool,
    erased_at: Ts,
}

#[derive(Deserialize)]
pub(in crate::routes) struct ErasedQuery {
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// The leads erased since the cursor in the organization's projects, or in one, oldest first and each exactly once, so a CRM or a
/// Zap can delete its copy.
pub(in crate::routes) async fn erased_leads(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(p): Query<ErasedQuery>,
    Query(q): Query<ChangesQuery>,
) -> AppResult<Json<Changes<ErasedLead>>> {
    if let Some(project) = p.project_id {
        check_project(&state.db, project, auth.org_id).await?;
    }
    let after = q.after()?;
    let items = sqlx::query_as::<_, ErasedLead>(&format!(
        "select lead_id, project_id, campaign_id, forgotten, erased_at from erased_leads
         where org_id = $1 and ($2::uuid is null or project_id = $2) and erased_at <= now() - interval '{SETTLE}'
           and ($3::timestamptz is null or (erased_at, lead_id) > ($3, $4))
         order by erased_at, lead_id limit $5"
    ))
    .bind(auth.org_id)
    .bind(p.project_id)
    .bind(after.map(|(at, _)| at))
    .bind(after.map(|(_, id)| id))
    .bind(q.limit() + 1)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Changes::page(items, &q, |e| (e.erased_at, e.lead_id))?))
}

/// The polling twin of the email_sent webhook: what Splee sent, oldest first, for logging outreach in a CRM.
pub(super) async fn sent_emails(
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
pub(in crate::routes) struct EmailEvent {
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
pub(in crate::routes) async fn email_events(
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
pub(super) struct ProjectStats {
    totals: Totals,
    campaigns: Vec<SegmentWithStats>,
    steps: Vec<stats::StepStats>,
}

/// Lifetime funnel and real cost, for reporting tools. Same numbers as the overview and campaign pages.
pub(super) async fn project_stats(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectStats>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(ProjectStats {
        totals: stats::totals(&state.db, id).await?,
        campaigns: segments::with_stats(&state.db, id, None, Archived::Included).await?,
        steps: stats::steps(&state.db, id).await?,
    }))
}

/// The Analytics page's numbers for a date range: totals against the period before, a daily or weekly series, and
/// breakdowns by campaign, mailbox, step, send time and channel.
pub(in crate::routes) async fn project_analytics(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<AnalyticsQuery>,
) -> AppResult<Json<Analytics>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(analytics::analytics(&state.db, id, &q).await?))
}
