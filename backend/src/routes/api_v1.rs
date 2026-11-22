use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Multipart, Path, Query, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::analytics::{self, Analytics, AnalyticsQuery};
use super::attachments;
use super::calls::{self, CallRow, LogBody};
use super::dedup_lists::{self, Contents, OffsetQuery as DedupPage};
use super::directory::{self, Added, ByDomains, ByDomainsBody, DescribeBody, SearchQuery};
use super::directory_export::{self, ExportQuery};
use super::saved_searches::{self, NewMatches, NewSearch, SearchRow};
use super::emails::{self, EmailRow};
use super::inbox::{self, InboxThread, ReplyExtras, ThreadMessage as WithFiles};
use super::leads::{self, CompanyRow, LeadDetail, LeadRow};
use super::lookalikes::{self as lookalike_routes, AddBody as AddLookalikesBody};
use super::meetings::{self, MeetingRow};
use super::my_copy::{self, CampaignCopy, Preview, ProjectCopy, SourceBody};
use super::outreach;
use super::projects;
use super::research;
use super::runs;
use super::segments::{self, Archived, ArchivedQuery, FindPeopleBody, SegmentBody, SegmentWithStats, WriteEmailsBody};
use super::sending_domains::{self, SuggestQuery};
use super::settings;
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
use crate::audit::Who;
use crate::auth::{self, ApiKeyAuth, KeyAuth};
use crate::campaign_imports::{self, RowOutcome, SubmittedLead};
use crate::company_filters::{self, Claude, Described, Model};
use crate::deals::{self, Deal, DealFilter, Move, NewDeal, Stage};
use crate::email_lookups::{self, Batch, BatchPerson, Found, Person, PhoneFound, PhoneQuery};
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::jobchanges::{self, AddBody, Overview};
use crate::local_businesses::{self, AddOutcome, Results};
use crate::lookalikes::{self, Lookalikes};
use crate::models::{
    AgentRun, DedupList, EmailAttachment, EmailMessage, Lead, Project, ProjectSettings, Segment, SegmentStep, SendingDomain, Suppression,
    SuppressionList, Ts,
};
use crate::monthly_cap::{self, Cap};
use crate::ownership;
use crate::porkbun::Availability;
use crate::send_state::SendState;
use crate::state::AppState;

pub fn router(state: &AppState) -> Router<AppState> {
    router_with(state, Arc::new(Claude))
}

/// `model` reads POST /v1/companies/filters' descriptions. Tests pass a fake.
pub fn router_with(state: &AppState, model: Arc<dyn Model>) -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/projects/{id}", get(get_project).patch(update_project))
        .route("/v1/projects/{id}/campaigns", get(list_campaigns).post(create_campaign))
        .route("/v1/campaigns/{id}", get(get_campaign).patch(update_campaign))
        .route("/v1/campaigns/import", post(import_campaign).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)))
        .route("/v1/campaigns/import/{id}", get(campaign_import))
        .route("/v1/campaigns/import/{id}/rows", get(campaign_import_rows))
        .route("/v1/campaigns/{id}/start", post(start_campaign))
        .route("/v1/campaigns/{id}/send-state", get(campaign_send_state))
        .route("/v1/campaigns/{id}/stop", post(stop_campaign))
        .route("/v1/campaigns/{id}/approve", post(approve_campaign))
        .route("/v1/campaigns/{id}/archive", post(archive_campaign))
        .route("/v1/campaigns/{id}/unarchive", post(unarchive_campaign))
        .route("/v1/campaigns/{id}/my-copy", get(campaign_my_copy).patch(update_campaign_my_copy))
        .route("/v1/campaigns/{id}/my-copy/source", put(set_my_copy_source))
        .route("/v1/campaigns/{id}/my-copy/preview", get(preview_my_copy))
        .route("/v1/projects/{id}/my-copy", get(project_my_copy).patch(update_project_my_copy))
        .route("/v1/hot-leads", get(hot_leads))
        .route("/v1/projects/{id}/inbox/changes", get(inbox_changes))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/projects/{id}/leads/upsert", post(upsert_lead))
        .route("/v1/projects/{id}/companies", get(list_companies))
        .route("/v1/projects/{id}/enrich/email", post(find_email))
        .route("/v1/projects/{id}/enrich/email/batch", post(find_emails))
        .route("/v1/enrich/email/batch/{id}", get(email_batch))
        .route("/v1/projects/{id}/enrich/phone", post(find_phone))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/leads/{id}/assign", post(assign_lead))
        .route("/v1/leads/{id}/thread", get(lead_thread))
        .route("/v1/leads/{id}/reply", post(reply_to_lead))
        .route("/v1/erased-leads", get(erased_leads))
        .route("/v1/leads/{id}/mark-replied", post(mark_replied))
        .route("/v1/attachments", post(upload_attachment).layer(DefaultBodyLimit::max(attachments::UPLOAD_BODY_LIMIT)))
        .route("/v1/attachments/{id}", get(download_attachment))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/email-events", get(email_events))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/projects/{id}/analytics", get(project_analytics))
        .route("/v1/projects/{id}/monthly-cap", get(project_monthly_cap))
        .route("/v1/suppressions", get(list_suppressions).post(add_suppressions))
        .route("/v1/suppression-lists", get(list_suppression_lists))
        .route("/v1/suppression-lists/people", post(add_people_to_list))
        .route("/v1/suppression-lists/companies", post(add_companies_to_list))
        .route("/v1/suppression-lists/{id}", patch(rescope_suppression_list).delete(delete_suppression_list))
        .route("/v1/suppression-lists/{id}/entries", get(suppression_list_entries))
        .route("/v1/dedup/companies", get(list_dedup_lists).post(add_to_dedup_list))
        .route("/v1/dedup/companies/{id}", get(get_dedup_list).patch(rescope_dedup_list).delete(delete_dedup_list))
        .route("/v1/companies", get(search_companies))
        .route("/v1/companies/export", get(export_companies))
        .route("/v1/companies/filters", post(companies_filters))
        .route("/v1/companies/by-domains", post(companies_by_domains))
        .route("/v1/sending-domains", get(list_sending_domains))
        .route("/v1/sending-domains/suggestions", get(suggest_sending_domains))
        .route("/v1/sending-domains/availability", get(check_sending_domains))
        .route("/v1/segments/{id}/companies", post(add_companies))
        .route("/v1/projects/{id}/saved-searches", get(list_saved_searches).post(save_search))
        .route("/v1/saved-searches/{id}", delete(delete_saved_search))
        .route("/v1/saved-searches/{id}/new", get(new_matches))
        .route("/v1/projects/{id}/lookalikes", get(list_lookalikes).post(add_lookalikes))
        .route("/v1/segments/{id}/find-people", post(find_people))
        .route("/v1/local-businesses", get(search_local_businesses))
        .route("/v1/segments/{id}/local-businesses", post(add_local_businesses))
        .route("/v1/segments/{id}/write-emails", post(write_emails))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/tasks", get(list_tasks))
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
        .layer(Extension(model))
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
/// Room for campaign_imports::MAX_LEADS leads with long fields.
const IMPORT_BODY_LIMIT: usize = 20 * 1024 * 1024;
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
    /// When it was archived, or null. An archived campaign is paused and only listed with ?archived=true.
    archived_at: Option<Ts>,
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
            archived_at: segment.archived_at,
            ab_test: None,
        }
    }
}

/// Archived campaigns only come with ?archived=true.
pub(super) async fn list_campaigns(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<ArchivedQuery>,
) -> AppResult<Json<Vec<Campaign>>> {
    check_project(&state.db, id, auth.org_id).await?;
    let rows = segments::with_stats(&state.db, id, None, q.into()).await?;
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
    let (segment, run_id) =
        segments::insert(&mut *state.db.acquire().await?, auth.org_id, id, SegmentBody::from_prompt(body.prompt, body.language)).await?;
    Ok(Json(json!({ "campaign_id": segment.id, "run_id": run_id })))
}

#[derive(Deserialize)]
pub(super) struct ImportBody {
    project_id: Uuid,
    #[serde(default)]
    name: String,
    leads: Vec<SubmittedLead>,
    instructions: Option<String>,
    followup_instructions: Option<String>,
    language: Option<String>,
}

/// Like explee.com's campaign import: a new campaign holding exactly the leads that can be imported, which runs in the
/// background (campaign_imports.rs). The campaign is made now, so a bad name or language is a 400 here.
pub(super) async fn import_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<ImportBody>) -> AppResult<Json<Value>> {
    check_project(&state.db, body.project_id, auth.org_id).await?;
    if body.leads.is_empty() || body.leads.len() > campaign_imports::MAX_LEADS {
        return Err(AppError::BadRequest(format!("Send 1 to {} leads per import", campaign_imports::MAX_LEADS)));
    }
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AppError::BadRequest("Give the campaign a name".into()));
    }
    if body.followup_instructions.as_deref().is_some_and(|f| !f.trim().is_empty()) {
        return Err(AppError::Unprocessable(
            "A campaign has no follow-up instructions of its own: every campaign in a project sends the project's follow-ups. Leave out \
             followup_instructions and set them in the project's settings in the app"
                .into(),
        ));
    }
    // A name like "German" works too, as it does on a lead.
    let language =
        body.language.filter(|l| !l.trim().is_empty()).map(|l| crate::language::parse(&l).map_or(l.trim().to_string(), |found| found.code.into()));
    // The first email's prompt reads the angle as the campaign's outreach angle.
    let angle = body.instructions.unwrap_or_default().trim().to_string();
    // Together, so a campaign never exists without the import that fills it.
    let mut tx = state.db.begin().await?;
    let (segment, _) = segments::insert(&mut tx, auth.org_id, body.project_id, SegmentBody::imported(name.into(), angle, language)).await?;
    let task_id = campaign_imports::queue(&mut tx, auth.org_id, body.project_id, segment.id, body.leads).await?;
    tx.commit().await?;
    let db = state.db.clone();
    tokio::spawn(async move {
        if let Err(e) = campaign_imports::drain(&db).await {
            tracing::error!(error = ?e, "could not run campaign imports");
        }
    });
    Ok(Json(json!({ "task_id": task_id, "campaign_id": segment.id, "status": "pending" })))
}

pub(super) async fn campaign_import(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let task = campaign_imports::find(&state.db, auth.org_id, id).await?.ok_or(AppError::NotFound)?;
    let result = (task.status == "completed")
        .then(|| json!({ "campaign_id": task.segment_id, "imported": task.imported, "skipped": task.skipped, "deduped": task.deduped }));
    Ok(Json(json!({
        "task_id": task.id,
        "status": task.status,
        "project_id": task.project_id,
        "campaign_id": task.segment_id,
        "result": result,
        "error": Some(task.error).filter(|e| !e.is_empty()),
        "created_at": task.created_at,
        "completed_at": task.completed_at,
    })))
}

pub(super) async fn campaign_import_rows(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<RowOutcome>>> {
    let task = campaign_imports::find(&state.db, auth.org_id, id).await?.ok_or(AppError::NotFound)?;
    let (items, total) = campaign_imports::rows(&state.db, task.id, page.limit(), page.offset()).await?;
    Ok(Json(Paginated { items, total }))
}

/// A PATCH body. A field the route doesn't take is a 422, so a typo never reads as a change that happened.
fn strict<T: DeserializeOwned>(body: Value) -> AppResult<T> {
    serde_json::from_value(body).map_err(|e| AppError::Unprocessable(format!("Invalid body: {e}")))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CampaignUpdate {
    name: Option<String>,
    description: Option<String>,
    targeting: Option<CampaignTargeting>,
    pain_points: Option<String>,
    angle: Option<String>,
    language: Option<String>,
    track_clicks: Option<bool>,
    track_opens: Option<bool>,
    lead_pages: Option<bool>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    daily_limit: Option<Option<i32>>,
}

/// The lists the campaign page edits. Each one sent replaces that list and the others stay, so a typo in a key is a 422 rather
/// than a list nobody reads.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CampaignTargeting {
    #[serde(skip_serializing_if = "Option::is_none")]
    industries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    company_sizes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    countries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_countries: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    keywords: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    exclude_keywords: Option<Vec<String>>,
}

/// Changes the fields sent, as the campaign page does, and returns the whole definition. Emails already written keep their brief
/// and language until they're rewritten.
pub(super) async fn update_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<CampaignDetail>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let body: CampaignUpdate = strict(body)?;
    let name = body.name.as_deref().map(str::trim);
    if name == Some("") {
        return Err(AppError::Unprocessable("name can't be empty".into()));
    }
    let language = body.language.as_deref().map(str::trim);
    if let Some(language) = language {
        segments::check_language(language)?;
    }
    segments::check_daily_limit(body.daily_limit)?;
    sqlx::query(
        "update segments set name = coalesce($2, name), description = coalesce($3, description), targeting = targeting || coalesce($4, '{}'),
           pain_points = coalesce($5, pain_points), angle = coalesce($6, angle), language = coalesce($7, language),
           track_clicks = coalesce($8, track_clicks), track_opens = coalesce($9, track_opens), lead_pages = coalesce($10, lead_pages),
           daily_limit = case when $11 then $12 else daily_limit end
         where id = $1",
    )
    .bind(segment.id)
    .bind(name)
    .bind(body.description)
    .bind(body.targeting.map(|t| json!(t)))
    .bind(body.pain_points)
    .bind(body.angle)
    .bind(language)
    .bind(body.track_clicks)
    .bind(body.track_opens)
    .bind(body.lead_pages)
    .bind(body.daily_limit.is_some())
    .bind(body.daily_limit.flatten())
    .execute(&state.db)
    .await?;
    get_campaign(State(state), auth, Path(id)).await
}

/// A campaign as its page shows it, with the project's schedule and follow-ups it sends on.
#[derive(Serialize)]
pub(super) struct CampaignDetail {
    #[serde(flatten)]
    campaign: Campaign,
    project_id: Uuid,
    /// Why it has its status, in a sentence.
    status_reason: String,
    icon: String,
    est_size: i64,
    fit_score: i32,
    /// Who it targets: the object the campaign page edits, which the agent fills in from the description.
    targeting: Value,
    /// The brief its emails are written from.
    pain_points: String,
    angle: String,
    track_clicks: bool,
    track_opens: bool,
    lead_pages: bool,
    /// Its own cap on first emails and follow-ups a day, on top of the project's daily_email_limit, or null for none.
    daily_limit: Option<i32>,
    schedule: Schedule,
    /// The project's follow-ups after the first email, each an email or a call some days later.
    follow_ups: Value,
    /// The tool an imported campaign came from, or null for one made in Splee.
    imported_from: Option<String>,
    /// An imported campaign's emails. Empty for one made in Splee.
    sequence: Vec<SegmentStep>,
    /// What can't be changed on this campaign, and why.
    read_only: Vec<ReadOnly>,
}

/// When the project sends. Every campaign in it shares this.
#[derive(Serialize)]
struct Schedule {
    /// False while the project's sending is paused, which holds every campaign in it.
    sending: bool,
    timezone: String,
    /// 1 (Monday) to 7 (Sunday).
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
    daily_email_limit: i32,
}

#[derive(Serialize)]
struct ReadOnly {
    field: &'static str,
    reason: String,
}

pub(super) async fn get_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<CampaignDetail>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let row = segments::one_with_stats(&state.db, &segment).await?;
    let ab_test = ab_test::load(&state.db, &[&segment]).await?.remove(&segment.id);
    let settings = settings::load_or_create(&state.db, segment.project_id).await?;
    let imported = outreach::steps_of(&state.db, segment.id).await?;
    // The optimizer stamps auto_paused_at when it pauses a campaign. A person may have resumed and paused it since.
    let by_optimizer: bool = sqlx::query_scalar(
        "select exists (select 1 from segments s where s.id = $1 and s.auto_paused_at is not null
           and not exists (select 1 from audit_events a where a.target_id = s.id and a.action = 'campaign.paused' and a.created_at > s.auto_paused_at))",
    )
    .bind(segment.id)
    .fetch_one(&state.db)
    .await?;
    let mut read_only = vec![
        ReadOnly { field: "schedule", reason: "Every campaign in the project shares it. Change it with PATCH /v1/projects/{id}".into() },
        ReadOnly { field: "follow_ups", reason: "Every campaign in the project shares them. Change them in the project's settings in the app".into() },
    ];
    if let Some(tool) = &imported.imported_from {
        read_only.push(ReadOnly { field: "sequence", reason: format!("It came from {tool}, and Splee sends these emails as written") });
    }
    Ok(Json(CampaignDetail {
        status_reason: status_reason(&segment, !settings.paused, by_optimizer),
        project_id: segment.project_id,
        icon: segment.icon,
        est_size: segment.est_size,
        fit_score: segment.fit_score,
        targeting: segment.targeting,
        pain_points: segment.pain_points,
        angle: segment.angle,
        track_clicks: segment.track_clicks,
        track_opens: segment.track_opens,
        lead_pages: segment.lead_pages,
        daily_limit: segment.daily_limit,
        schedule: Schedule {
            sending: !settings.paused,
            timezone: settings.timezone,
            send_days: settings.send_days,
            send_start_hour: settings.send_start_hour,
            send_end_hour: settings.send_end_hour,
            daily_email_limit: settings.daily_email_limit,
        },
        follow_ups: settings.follow_ups,
        imported_from: imported.imported_from,
        sequence: imported.steps,
        read_only,
        campaign: Campaign { ab_test, ..Campaign::from(row) },
    }))
}

fn status_reason(segment: &Segment, project_sending: bool, by_optimizer: bool) -> String {
    match (segment.status.as_str(), segment.auto_paused_at) {
        ("draft", _) => "Not started. Starting it approves its drafts for sending".into(),
        ("paused", Some(at)) if by_optimizer => format!(
            "Splee's optimizer paused it on {}, for too few hot leads per email sent. Once started again it stays running",
            at.format("%b %-d, %Y")
        ),
        ("paused", _) => "Paused. Its unsent emails wait as drafts until it starts again".into(),
        _ if !project_sending => "Started, but sending is paused for the whole project".into(),
        ("scaling", _) => "Sending ahead of the project's other campaigns, since it gets hot leads for fewer emails".into(),
        _ => "Sending on the project's schedule".into(),
    }
}

/// Whether the campaign's emails go out now and, if not, the one thing holding them. See send_state.rs.
pub(super) async fn campaign_send_state(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<SendState>> {
    Ok(Json(super::segments::send_state_of(&state.db, auth.org_id, id).await?))
}

fn accepted() -> Json<Value> {
    Json(json!({ "accepted": true }))
}

/// The campaign's Start button: it sends, and its drafts are approved. The app only offers Start on a campaign that isn't
/// sending, so on one that is, nothing changes and a scaling campaign stays scaling.
pub(super) async fn start_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if !segments::running(&segment) {
        segments::launch(&state.db, &auth, &segment).await?;
    }
    Ok(accepted())
}

/// The campaign's Pause button: its unsent emails go back to drafts. Replies keep coming in. A campaign that isn't sending
/// is left as it is, like the app, which only offers Pause on a sending one.
pub(super) async fn stop_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if segments::running(&segment) {
        segments::hold(&state.db, &auth, &segment).await?;
    }
    Ok(accepted())
}

/// Pauses the campaign like stop and hides it from list_campaigns. Its leads, emails and stats stay, and replies still come in.
pub(super) async fn archive_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Campaign>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    segments::archive(&state.db, &auth, &segment).await?;
    Ok(Json(Campaign::from(segments::one_with_stats(&state.db, &segment).await?)))
}

/// Lists the campaign again, paused. It sends once it's started.
pub(super) async fn unarchive_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Campaign>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    segments::unarchive(&state.db, &auth, &segment).await?;
    Ok(Json(Campaign::from(segments::one_with_stats(&state.db, &segment).await?)))
}

/// Splee has no review state for a whole campaign. This is the Approve button on each of its first emails and follow-ups
/// that wait as drafts, as POST /v1/emails/{id}/approve does one at a time. A paused campaign holds them until it starts.
pub(super) async fn approve_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let drafts: Vec<Uuid> = sqlx::query_scalar(
        "select id from email_messages where segment_id = $1 and direction = 'outbound' and kind in ('initial', 'follow_up') and status = 'draft'
         order by created_at, id",
    )
    .bind(segment.id)
    .fetch_all(&state.db)
    .await?;
    for &email in &drafts {
        emails::update_one(&state.db, auth.org_id, email, emails::UpdateBody { status: Some("approved".into()), ..Default::default() }).await?;
    }
    Ok(Json(json!({ "accepted": true, "approved": drafts.len() })))
}

/// The campaign's first email: Instructions or My copy, and the project's or its own. See routes/my_copy.rs.
pub(super) async fn campaign_my_copy(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(my_copy::campaign(&state.db, auth.org_id, id).await?))
}

pub(super) async fn update_campaign_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(my_copy::update_campaign(&state.db, auth.org_id, id, strict(body)?).await?))
}

pub(super) async fn set_my_copy_source(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<CampaignCopy>> {
    let body: SourceBody = strict(body)?;
    Ok(Json(my_copy::set_source(&state.db, auth.org_id, id, body.source).await?))
}

pub(super) async fn preview_my_copy(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Preview>> {
    Ok(Json(my_copy::campaign_preview(&state.db, auth.org_id, id, Default::default()).await?))
}

pub(super) async fn project_my_copy(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(my_copy::project(&state.db, auth.org_id, id).await?))
}

pub(super) async fn update_project_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(my_copy::update_project(&state.db, auth.org_id, id, strict(body)?).await?))
}

/// A project's sending controls, as PATCH /v1/projects/{id} takes them.
#[derive(Serialize)]
pub(super) struct ProjectControls {
    project_id: Uuid,
    /// False while the project's Pause sending holds every campaign in it.
    sending: bool,
    daily_email_limit: i32,
    daily_text_limit: i32,
    timezone: String,
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
    /// Keeps sending campaigns stocked with written emails within the monthly budget and lead cap.
    autopilot: bool,
    /// Claude spend a calendar month (UTC) may reach, counting runs started by hand.
    autopilot_budget_micros: i64,
    /// New leads emailed a month.
    autopilot_lead_cap: i32,
    /// Autopilot's emails go out without approval.
    autopilot_auto_send: bool,
    /// Hours a draft waits for review before it goes out on its own, or null when drafts wait for a person.
    review_window_hours: Option<i32>,
    /// Email spend a calendar month (UTC) may reach before first emails and follow-ups wait for the 1st, or null for no cap.
    monthly_cap_micros: Option<i64>,
    /// The name the agent writes and signs emails as, unless the sending mailbox has its own. Empty for "the team at" the company.
    sender_name: String,
}

impl From<ProjectSettings> for ProjectControls {
    fn from(s: ProjectSettings) -> Self {
        Self {
            project_id: s.project_id,
            sending: !s.paused,
            daily_email_limit: s.daily_email_limit,
            daily_text_limit: s.daily_text_limit,
            timezone: s.timezone,
            send_days: s.send_days,
            send_start_hour: s.send_start_hour,
            send_end_hour: s.send_end_hour,
            autopilot: s.autopilot,
            autopilot_budget_micros: s.autopilot_budget_micros,
            autopilot_lead_cap: s.autopilot_lead_cap,
            autopilot_auto_send: s.autopilot_auto_send,
            review_window_hours: s.review_window_hours,
            monthly_cap_micros: s.monthly_cap_micros,
            sender_name: s.sender_name,
        }
    }
}

/// A project as GET /v1/projects lists it, with its controls.
#[derive(Serialize)]
pub(super) struct ProjectWithControls {
    #[serde(flatten)]
    project: Project,
    controls: ProjectControls,
}

/// The organization's project, else a 404.
async fn load_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<Project> {
    let project = sqlx::query_as::<_, Project>("select * from projects where id = $1 and org_id = $2").bind(id).bind(org_id);
    project.fetch_optional(db).await?.ok_or(AppError::NotFound)
}

pub(super) async fn get_project(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<ProjectWithControls>> {
    let project = load_project(&state.db, id, auth.org_id).await?;
    Ok(Json(ProjectWithControls { project, controls: settings::load_or_create(&state.db, id).await?.into() }))
}

/// The app's New project: Splee researches the business from its website or description, then suggests campaigns. Poll run_id
/// with GET /v1/runs/{id}.
pub(super) async fn create_project(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<projects::CreateBody>,
) -> AppResult<Json<Value>> {
    let (project, run_id) = projects::insert(&state.db, auth.org_id, auth.user_id, body).await?;
    Ok(Json(json!({ "project_id": project.id, "run_id": run_id })))
}

/// The cap PATCH /v1/projects/{id} sets as monthly_cap_micros, and what the project's emails cost since the 1st.
pub(super) async fn project_monthly_cap(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Cap>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(monthly_cap::status(&state.db, id, chrono::Utc::now()).await?))
}

/// The project's sending controls, with the settings page's checks, and its Start and Pause sending buttons as `sending`.
/// An empty body changes nothing and returns them.
pub(super) async fn update_project(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<ProjectControls>> {
    let project = load_project(&state.db, id, auth.org_id).await?;
    let controls: settings::Controls = strict(body)?;
    // Like the app's toggle, which offers Start only while paused and Pause only while sending.
    let paused = settings::load_or_create(&state.db, id).await?.paused;
    let (start, pause) = (controls.sending == Some(true) && paused, controls.sending == Some(false) && !paused);
    // Before anything is saved, so a refused start leaves the other fields as they were.
    if start && !projects::has_mailbox(&state.db, auth.org_id).await? {
        return Err(AppError::Conflict(projects::NO_MAILBOX.into()));
    }
    settings::apply(&state.db, auth.org_id, id, controls.into()).await?;
    if start {
        projects::start_sending(&state.db, &auth, &project).await?;
    } else if pause {
        projects::pause_sending(&state.db, &auth, &project).await?;
    }
    Ok(Json(settings::load_or_create(&state.db, id).await?.into()))
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
    super::imports::check_segment(&state, id, body.segment_id).await?;
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

#[derive(Deserialize)]
pub(super) struct HotLeadsQuery {
    /// Only leads that became hot after this. Pass the newest became_hot_at you've seen.
    since: Option<Ts>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// A lead whose reply the classifier called hot, with that first hot reply.
#[derive(Serialize, FromRow)]
pub(super) struct HotLead {
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
pub(super) async fn hot_leads(
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
pub(super) struct ChangesQuery {
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
pub(super) struct InboundReply {
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
pub(super) struct Changes<T> {
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
pub(super) struct CampaignFilter {
    /// Only this campaign's.
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    segment_id: Option<Uuid>,
}

pub(super) async fn inbox_changes(
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
pub(super) struct ErasedLead {
    lead_id: Uuid,
    project_id: Uuid,
    campaign_id: Option<Uuid>,
    /// The app's Forget rather than Delete, which also put the address on the do-not-contact list.
    forgotten: bool,
    erased_at: Ts,
}

#[derive(Deserialize)]
pub(super) struct ErasedQuery {
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// The leads erased since the cursor in the organization's projects, or in one, oldest first and each exactly once, so a CRM or a
/// Zap can delete its copy.
pub(super) async fn erased_leads(
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
        campaigns: segments::with_stats(&state.db, id, None, Archived::Included).await?,
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

pub(super) async fn list_dedup_lists(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<DedupList>>> {
    Ok(Json(dedup_lists::all(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
pub(super) struct DedupBody {
    name: String,
    domains: Vec<String>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

/// Adds domains to the named list, making it when the org has none by that name. Searches in the projects it covers leave
/// those companies out. Sending is never affected.
pub(super) async fn add_to_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<DedupBody>,
) -> AppResult<Json<dedup_lists::Added>> {
    check_list_size(body.domains.len())?;
    let domains = body.domains.iter().map(String::as_str);
    Ok(Json(dedup_lists::add_by_name(&state.db, auth.org_id, &body.name, body.project_ids, domains).await?))
}

pub(super) async fn get_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<DedupPage>,
) -> AppResult<Json<Contents>> {
    Ok(Json(dedup_lists::contents(&state.db, auth.org_id, id, &q).await?))
}

pub(super) async fn rescope_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ScopeBody>,
) -> AppResult<Json<DedupList>> {
    Ok(Json(dedup_lists::set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

pub(super) async fn delete_dedup_list(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    dedup_lists::delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

pub(super) async fn search_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<directory::Page>> {
    Ok(Json(directory::page_of(&state.db, auth.org_id, &q, &page).await?))
}

/// The filters a sentence like "dentists in Leeds that list an email" stands for, to pass to GET /v1/companies.
pub(super) async fn companies_filters(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Extension(model): Extension<Arc<dyn Model>>,
    Json(body): Json<DescribeBody>,
) -> AppResult<Json<Described>> {
    Ok(Json(company_filters::describe(&state, model.as_ref(), auth.org_id, &body.query).await?))
}

/// The database's company for each of up to 1,000 domains, and the domains it doesn't have.
pub(super) async fn companies_by_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<ByDomainsBody>,
) -> AppResult<Json<ByDomains>> {
    Ok(Json(directory::by_domains(&state.db, auth.org_id, body).await?))
}

/// The organization's sending domains as the Domains page lists them, with their DNS and how many mailboxes use each.
pub(super) async fn list_sending_domains(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<SendingDomain>>> {
    Ok(Json(crate::domains::list(&state.db, auth.org_id).await?))
}

/// The Domains page's suggestions for the company's domain, checked with the organization's Porkbun account.
pub(super) async fn suggest_sending_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SuggestQuery>,
) -> AppResult<Json<Vec<Availability>>> {
    Ok(Json(sending_domains::suggest(&state, auth.org_id, &q).await?))
}

#[derive(Deserialize)]
pub(super) struct AvailabilityQuery {
    /// Separated by commas.
    domains: String,
}

/// The most names one availability check takes.
const MAX_AVAILABILITY: usize = 20;

/// Whether the organization's Porkbun account can buy each name and at what price. Buying stays in the app.
pub(super) async fn check_sending_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<AvailabilityQuery>,
) -> AppResult<Json<Vec<Availability>>> {
    let mut names: Vec<String> = vec![];
    for raw in q.domains.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        let name = util_input::domain(raw).ok_or_else(|| AppError::BadRequest(format!("{raw:?} isn't a domain")))?;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() || names.len() > MAX_AVAILABILITY {
        return Err(AppError::BadRequest(format!("Pass 1 to {MAX_AVAILABILITY} domains, separated by commas")));
    }
    Ok(Json(sending_domains::availability(&state, auth.org_id, &names).await?))
}

/// The search as a file, up to 50,000 rows a page. See routes/directory_export.rs.
pub(super) async fn export_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SearchQuery>,
    Query(x): Query<ExportQuery>,
) -> AppResult<Response> {
    directory_export::export(&state, Who::from(&auth), &q, &x).await
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

pub(super) async fn list_saved_searches(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SearchRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::list_of(&state.db, id).await?))
}

/// Without a member behind the key, nobody owns the search, so only its chat alerts and GET .../new say what it finds.
pub(super) async fn save_search(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<NewSearch>,
) -> AppResult<Json<SearchRow>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::create_in(&state, id, auth.user_id, body).await?))
}

pub(super) async fn delete_saved_search(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    saved_searches::delete_one(&state.db, id, auth.org_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Unlike opening it in the app, reading it here leaves its new matches counted as new.
pub(super) async fn new_matches(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<NewMatches>> {
    let search = saved_searches::load(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::matches_of(&state.db, &search, q.since, &page).await?))
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

/// Work that goes on in the background after a request returns: an agent run, a find-email batch or a campaign import.
#[derive(Serialize, FromRow)]
pub(super) struct Task {
    id: Uuid,
    /// run, email_batch or campaign_import.
    #[serde(rename = "type")]
    task_type: String,
    /// A run's kind, like find_people. Null for the others.
    kind: Option<String>,
    project_id: Option<Uuid>,
    /// pending, completed or failed. A cancelled run counts as failed. Its own route has the details.
    status: String,
    created_at: Ts,
    finished_at: Option<Ts>,
    /// The route that follows it.
    url: String,
}

#[derive(Deserialize)]
pub(super) struct TasksQuery {
    status: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    /// A run kind, since every reply's classify_reply run would bury the ones an agent started.
    kind: Option<String>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// The organization's background work of every kind, newest first, so a lost task id can be found again.
pub(super) async fn list_tasks(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<TasksQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Task>>> {
    if let Some(status) = &q.status {
        util_input::one_of("status", status, &["pending", "completed", "failed"])?;
    }
    if let Some(task_type) = &q.task_type {
        util_input::one_of("type", task_type, &["run", "email_batch", "campaign_import"])?;
    }
    let tasks = "select * from (
           select r.id, 'run' as task_type, r.kind, r.project_id,
                  case when r.status = 'succeeded' then 'completed' when r.status in ('failed', 'cancelled') then 'failed'
                       else 'pending' end as status,
                  r.created_at, r.finished_at, '/v1/runs/' || r.id as url
           from agent_runs r left join projects p on p.id = r.project_id where r.org_id = $1 or p.org_id = $1
           union all
           select id, 'email_batch', null, project_id, case when finished_at is null then 'pending' else 'completed' end, created_at, finished_at,
                  '/v1/enrich/email/batch/' || id
           from email_lookup_batches where org_id = $1
           union all
           select id, 'campaign_import', null, project_id, status, created_at, completed_at, '/v1/campaigns/import/' || id
           from campaign_imports where org_id = $1
         ) t
         where ($2::text is null or status = $2) and ($3::text is null or task_type = $3) and ($4::uuid is null or project_id = $4)
           and ($5::text is null or kind = $5)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from ({tasks}) counted"))
        .bind(auth.org_id)
        .bind(&q.status)
        .bind(&q.task_type)
        .bind(q.project_id)
        .bind(&q.kind)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Task>(&format!("{tasks} order by created_at desc, id limit $6 offset $7"))
        .bind(auth.org_id)
        .bind(&q.status)
        .bind(&q.task_type)
        .bind(q.project_id)
        .bind(&q.kind)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
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
    /// The lead's campaign.
    campaign_id: Option<Uuid>,
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
            campaign_id: lead.segment_id,
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

#[derive(Serialize)]
pub(super) struct MarkedReplied {
    lead_id: Uuid,
    /// When the lead's latest reply came in. A reply after it brings the thread back to Needs reply.
    answered_through: Ts,
}

/// The inbox's Done, for a reply answered outside Splee. The lead's status stays as it is.
pub(super) async fn mark_replied(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<MarkedReplied>> {
    let answered_through = inbox::mark_replied(&state.db, auth.org_id, id).await?;
    Ok(Json(MarkedReplied { lead_id: id, answered_through }))
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

/// The lead's sales meetings from Fireflies, Fathom, Gong, Zoom or tl;dv, oldest first, with the agent's summary, next steps and
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

/// Finds a person's email with the project's finders and verifier, the way the email waterfall does for a lead, without
/// adding them as one. Each call counts against the organization's hourly email searches.
pub(super) async fn find_email(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<Person>) -> AppResult<Json<Found>> {
    check_project(&state.db, id, auth.org_id).await?;
    let person = body.checked().map_err(AppError::BadRequest)?;
    email_lookups::check_connected(&state.db, id).await?;
    email_lookups::claim_one(&state.db, auth.org_id).await?;
    Ok(Json(email_lookups::find(&state, id, &person).await))
}

/// Finds the mobile of the person behind a LinkedIn profile with the project's finders that return mobiles, without adding
/// them as a lead. Each call counts against the organization's hourly email searches.
pub(super) async fn find_phone(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<PhoneQuery>) -> AppResult<Json<PhoneFound>> {
    check_project(&state.db, id, auth.org_id).await?;
    let query = body.checked().map_err(AppError::BadRequest)?;
    email_lookups::check_phone_finder(&state.db, id).await?;
    email_lookups::claim_one(&state.db, auth.org_id).await?;
    Ok(Json(email_lookups::find_phone(&state, auth.org_id, id, &query).await))
}

#[derive(Deserialize)]
pub(super) struct BatchBody {
    people: Vec<BatchPerson>,
}

/// Queues up to 500 people for the same search, in the background and within the hourly limit, for GET
/// /v1/enrich/email/batch/{task_id} to follow.
pub(super) async fn find_emails(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>, Json(body): Json<BatchBody>) -> AppResult<Json<Value>> {
    check_project(&state.db, id, auth.org_id).await?;
    if body.people.is_empty() || body.people.len() > email_lookups::MAX_BATCH {
        return Err(AppError::BadRequest(format!("Send 1 to {} people", email_lookups::MAX_BATCH)));
    }
    let mut people = Vec::with_capacity(body.people.len());
    for (i, p) in body.people.into_iter().enumerate() {
        let person = p.person.checked().map_err(|e| AppError::BadRequest(format!("people[{i}]: {e}")))?;
        people.push(BatchPerson { person, client_ref: p.client_ref });
    }
    email_lookups::check_connected(&state.db, id).await?;
    let task_id = email_lookups::create(&state.db, auth.org_id, id, &people).await?;
    Ok(Json(json!({ "task_id": task_id, "status": "pending", "total": people.len() })))
}

pub(super) async fn email_batch(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Batch>> {
    Ok(Json(email_lookups::load(&state.db, auth.org_id, id).await?))
}

/// The organization's last 30 data exports, newest first, with the files each wrote. See exports/.
pub(super) async fn list_exports(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<crate::exports::Run>>> {
    Ok(Json(crate::exports::recent(&state.db, auth.org_id, 30).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
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
            Self::start_with(db, limits, Arc::new(crate::company_filters::Claude)).await
        }

        async fn start_with(db: &PgPool, limits: Limits, model: Arc<dyn crate::company_filters::Model>) -> Self {
            let state = AppState { rate_limits: RateLimiter::new(limits), ..crate::autopilot::tests::state(db.clone()) };
            Self::serve(state, model).await
        }

        async fn serve(state: AppState, model: Arc<dyn crate::company_filters::Model>) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router_with(&state, model)).with_state(state);
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

        async fn get(&self, path: &str) -> (u16, Value) {
            let (status, _, body) = self.send(Method::GET, KEYS[0], path, None, None).await;
            (status, body)
        }

        async fn patch(&self, path: &str, body: Value) -> (u16, Value) {
            let (status, _, body) = self.send(Method::PATCH, KEYS[0], path, None, Some(body)).await;
            (status, body)
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

    /// A campaign in the project with one lead and a first email waiting as a draft. Returns the campaign and the email.
    async fn campaign(db: &PgPool, project: Uuid, status: &str) -> (Uuid, Uuid) {
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', $2) returning id")
            .bind(project)
            .bind(status)
            .fetch_one(db)
            .await
            .unwrap();
        let email: Uuid = sqlx::query_scalar(
            "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
             select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l returning id",
        )
        .bind(project)
        .bind(segment)
        .fetch_one(db)
        .await
        .unwrap();
        (segment, email)
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
    async fn a_description_becomes_filters_within_the_organizations_hour(db: PgPool) {
        use crate::company_filters::tests::{Fake, leeds_dentists, seed as seed_directory};

        seed(&db).await;
        seed_directory(&db).await;
        let answer = crate::company_filters::Answer { note: "Company size isn't a filter.".into(), ..leeds_dentists() };
        let api = Api::start_with(&db, LIMITS, Arc::new(Fake::new(answer))).await;
        let path = "/v1/companies/filters";
        let query = json!({ "query": "dentists in Leeds with an email that use Shopify, 10 to 50 staff" });

        let (status, _, body) = api.post(KEYS[0], path, None, query.clone()).await;
        assert_eq!(status, 200, "{body}");
        let filters = json!({ "q": null, "country": "GB", "city": "Leeds", "category": "dental_clinic", "has_email": true, "has_phone": false, "tech": "Shopify" });
        assert_eq!(body, json!({ "filters": filters, "note": "Company size isn't a filter." }));
        assert_eq!(api.post(KEYS[0], path, None, json!({ "query": " " })).await.0, 400);
        assert_eq!(api.post(KEYS[0], path, None, json!({})).await.0, 422);
        assert_eq!(api.post("splee_wrong", path, None, query.clone()).await.0, 401);

        for _ in 1..30 {
            assert_eq!(api.post(KEYS[0], path, None, query.clone()).await.0, 200);
        }
        let (status, headers, body) = api.post(KEYS[0], path, None, query.clone()).await;
        assert_eq!(status, 429, "{body}");
        assert!(header(&headers, "retry-after").unwrap().parse::<u64>().unwrap() > 3500);
        assert!(body["error"].as_str().unwrap().contains("30 searches from a description this hour"), "{body}");
        assert_eq!(api.post(KEYS[1], path, None, query).await.0, 200, "another organization has its own hour");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_organization_gets_its_hour_of_requests_then_a_429(db: PgPool) {
        seed(&db).await;
        let api = Api::start(&db, Limits { per_hour: 2, in_flight: 150, what: "API requests" }).await;

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

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaigns_start_stop_and_approve_like_the_app_buttons(db: PgPool) {
        let [a, b] = seed(&db).await;
        let (segment, _) = campaign(&db, a, "draft").await;
        let (theirs, _) = campaign(&db, b, "draft").await;
        let api = Api::start(&db, LIMITS).await;
        let state = |id: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query_as::<_, (String, String, Option<String>, String)>(
                    "select s.status, e.status, e.approved_via, l.status from segments s join email_messages e on e.segment_id = s.id
                     join leads l on l.id = e.lead_id where s.id = $1",
                )
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap()
            }
        };
        let row = |status: &str, email: &str, via: Option<&str>, lead: &str| (status.to_string(), email.to_string(), via.map(String::from), lead.to_string());

        let (status, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
        assert_eq!((status, detail["status"].as_str(), detail["project_id"].as_str()), (200, Some("draft"), Some(a.to_string().as_str())), "{detail}");
        assert!(detail["status_reason"].as_str().unwrap().starts_with("Not started"), "{detail}");
        assert_eq!((detail["schedule"]["sending"].clone(), detail["schedule"]["daily_email_limit"].is_number()), (json!(false), true));
        assert!(detail["follow_ups"].is_array() && detail["targeting"].is_object(), "{detail}");
        let read_only: Vec<&str> = detail["read_only"].as_array().unwrap().iter().map(|r| r["field"].as_str().unwrap()).collect();
        assert_eq!(read_only, ["schedule", "follow_ups"]);

        // Another organization's campaign looks like one that doesn't exist, and nothing happens to it.
        assert_eq!(api.get(&format!("/v1/campaigns/{theirs}")).await.0, 404);
        for action in ["start", "stop", "approve"] {
            assert_eq!(api.post(KEYS[0], &format!("/v1/campaigns/{theirs}/{action}"), None, json!({})).await.0, 404, "{action}");
        }
        assert_eq!(state(theirs).await, row("draft", "draft", None, "new"));

        // Start is the app's Start: the campaign sends and its drafts are approved. A retry with the key replays, and a
        // repeat without one changes nothing, so it's audited once, under the key.
        let start = format!("/v1/campaigns/{segment}/start");
        let (status, headers, body) = api.post(KEYS[0], &start, Some("start-1"), json!({})).await;
        assert_eq!((status, body, header(&headers, "idempotent-replayed")), (200, json!({ "accepted": true }), None));
        assert_eq!(state(segment).await, row("working", "approved", Some("person"), "queued"));
        let (status, headers, _) = api.post(KEYS[0], &start, Some("start-1"), json!({})).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, Some("true")));
        sqlx::query("update segments set status = 'scaling' where id = $1").bind(segment).execute(&db).await.unwrap();
        assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.2, json!({ "accepted": true }));
        assert_eq!(state(segment).await.0, "scaling", "starting a sending campaign leaves it as it is");
        let audited = "select count(*) from audit_events where action = 'campaign.launched' and actor_key_id is not null";
        assert_eq!(count(&db, audited).await, 1);

        // Stop is the app's Pause: unsent emails go back to drafts. Twice is the same as once.
        for _ in 0..2 {
            let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/stop"), None, json!({})).await;
            assert_eq!((status, body), (200, json!({ "accepted": true })));
            assert_eq!(state(segment).await, row("paused", "draft", None, "new"));
        }
        assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.paused'").await, 1);
        let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
        assert!(detail["status_reason"].as_str().unwrap().starts_with("Paused."), "{detail}");
        // The optimizer paused it after the person last did.
        sqlx::query("update segments set auto_paused_at = now() + interval '1 minute' where id = $1").bind(segment).execute(&db).await.unwrap();
        let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
        assert!(detail["status_reason"].as_str().unwrap().contains("optimizer"), "{detail}");

        // Approve is each draft's Approve button. A paused campaign keeps them until it starts.
        let approve = format!("/v1/campaigns/{segment}/approve");
        assert_eq!(api.post(KEYS[0], &approve, None, json!({})).await.2, json!({ "accepted": true, "approved": 1 }));
        assert_eq!(state(segment).await, row("paused", "approved", Some("person"), "queued"));
        assert_eq!(api.post(KEYS[0], &approve, None, json!({})).await.2, json!({ "accepted": true, "approved": 0 }));

        // An imported campaign's emails are sent as written.
        sqlx::query("update segments set imported_from = 'instantly' where id = $1").bind(segment).execute(&db).await.unwrap();
        sqlx::query("insert into segment_steps (segment_id, position, subject, body) values ($1, 0, 'Quick one', 'Hi {first_name}')")
            .bind(segment)
            .execute(&db)
            .await
            .unwrap();
        let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
        assert_eq!((detail["imported_from"].as_str(), detail["sequence"][0]["body"].as_str()), (Some("Instantly"), Some("Hi {first_name}")));
        assert_eq!(detail["read_only"][2]["field"], "sequence", "{detail}");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_campaign_patch_changes_only_the_fields_it_sends(db: PgPool) {
        let [a, b] = seed(&db).await;
        let (segment, _) = campaign(&db, a, "draft").await;
        let (theirs, _) = campaign(&db, b, "draft").await;
        sqlx::query("update segments set targeting = $2, angle = 'Fewer no-shows' where id = $1")
            .bind(segment)
            .bind(json!({ "roles": ["Practice Manager"], "countries": ["GB"] }))
            .execute(&db)
            .await
            .unwrap();
        let api = Api::start(&db, LIMITS).await;
        let path = format!("/v1/campaigns/{segment}");

        let body = json!({
            "name": " Leeds dentists ",
            "targeting": { "countries": ["GB", "IE"], "keywords": [] },
            "pain_points": "Empty chairs",
            "track_opens": true,
            "language": "de",
        });
        let (status, detail) = api.patch(&path, body).await;
        assert_eq!((status, detail["name"].as_str(), detail["language"].as_str()), (200, Some("Leeds dentists"), Some("de")), "{detail}");
        assert_eq!(detail["targeting"], json!({ "roles": ["Practice Manager"], "countries": ["GB", "IE"], "keywords": [] }));
        assert_eq!((detail["pain_points"].as_str(), detail["angle"].as_str()), (Some("Empty chairs"), Some("Fewer no-shows")));
        assert_eq!((detail["track_opens"].clone(), detail["read_only"][0]["field"].clone()), (json!(true), json!("schedule")), "{detail}");
        assert_eq!(detail["daily_limit"], Value::Null, "no limit of its own until one is sent");
        assert_eq!(api.patch(&path, json!({ "daily_limit": 25 })).await.1["daily_limit"], 25);
        assert_eq!(api.patch(&path, json!({ "angle": "Fewer no-shows" })).await.1["daily_limit"], 25, "left out, it stays");
        assert_eq!(api.patch(&path, json!({ "daily_limit": null })).await.1["daily_limit"], Value::Null, "null takes it off");

        // A refused body changes nothing, and another organization's campaign looks like one that doesn't exist.
        for (body, status) in [
            (json!({ "name": " " }), 422),
            (json!({ "angle": "New", "targeting": { "role": ["CEO"] } }), 422),
            (json!({ "angle": "New", "schedule": { "send_days": [1] } }), 422),
            (json!({ "angle": "New", "language": "xx" }), 400),
            (json!({ "angle": "New", "daily_limit": 0 }), 400),
            (json!({ "angle": "New", "daily_limit": 5001 }), 400),
        ] {
            assert_eq!(api.patch(&path, body.clone()).await.0, status, "{body}");
        }
        assert_eq!(api.patch(&format!("/v1/campaigns/{theirs}"), json!({ "name": "Mine now" })).await.0, 404);
        let names: Vec<(String, String)> =
            sqlx::query_as("select name, angle from segments order by name").fetch_all(&db).await.unwrap();
        assert_eq!(names, [("Dentists".into(), "".into()), ("Leeds dentists".into(), "Fewer no-shows".into())]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_archived_campaign_stops_sending_and_leaves_the_list_until_it_comes_back_paused(db: PgPool) {
        let [a, b] = seed(&db).await;
        let (segment, email) = campaign(&db, a, "draft").await;
        let (theirs, _) = campaign(&db, b, "working").await;
        let api = Api::start(&db, LIMITS).await;
        let campaigns = format!("/v1/projects/{a}/campaigns");
        let ids = |body: Value| body.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let email_status = || {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, String>("select status from email_messages where id = $1").bind(email).fetch_one(&db).await.unwrap() }
        };
        let start = format!("/v1/campaigns/{segment}/start");
        assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.0, 200);
        assert_eq!(email_status().await, "approved");

        // Archiving stops it like Stop and takes it off the list. Twice is the same as once.
        for _ in 0..2 {
            let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/archive"), None, json!({})).await;
            assert_eq!((status, body["status"].as_str(), body["archived_at"].is_string()), (200, Some("paused"), true), "{body}");
        }
        assert_eq!(email_status().await, "draft");
        assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.archived' and actor_key_id is not null").await, 1);
        assert_eq!(ids(api.get(&campaigns).await.1), Vec::<String>::new());
        assert_eq!(ids(api.get(&format!("{campaigns}?archived=true")).await.1), [segment.to_string()]);
        let (status, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
        assert_eq!((status, detail["leads"].as_i64(), detail["archived_at"].is_string()), (200, Some(1), true), "its leads stay: {detail}");
        let (status, _, body) = api.post(KEYS[0], &start, None, json!({})).await;
        assert_eq!(status, 409, "it can't start while archived: {body}");
        assert_eq!(email_status().await, "draft");
        let lead = json!({ "email": "new@acme.example", "full_name": "New Lead", "segment_id": segment });
        let (_, _, added) = api.post(KEYS[0], &format!("/v1/projects/{a}/leads"), None, json!({ "leads": [lead.clone()] })).await;
        assert_eq!((added["imported"].as_i64(), added["errors"][0]["error"].as_str()), (Some(0), Some("Unknown or archived campaign")), "{added}");
        assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{a}/leads/upsert"), None, lead).await.0, 400, "no leads go into it");

        // Unarchiving lists it again, paused, so nothing goes out until someone starts it.
        for _ in 0..2 {
            let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/unarchive"), None, json!({})).await;
            assert_eq!((status, body["status"].as_str(), body["archived_at"].is_null()), (200, Some("paused"), true), "{body}");
        }
        assert_eq!((ids(api.get(&campaigns).await.1), email_status().await), (vec![segment.to_string()], "draft".to_string()));
        assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.unarchived'").await, 1);
        assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.0, 200);
        assert_eq!(email_status().await, "approved");

        // Another organization's campaign looks like one that doesn't exist, and keeps sending.
        for action in ["archive", "unarchive"] {
            assert_eq!(api.post(KEYS[0], &format!("/v1/campaigns/{theirs}/{action}"), None, json!({})).await.0, 404, "{action}");
        }
        let untouched = format!("select count(*) from segments where id = '{theirs}' and status = 'working' and archived_at is null");
        assert_eq!(count(&db, &untouched).await, 1);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn sending_domains_are_suggested_and_checked_with_the_organizations_porkbun(db: PgPool) {
        let [a, _] = seed(&db).await;
        let (porkbun, _) = crate::porkbun::fake::start().await;
        let base = crate::autopilot::tests::state(db.clone());
        let cfg = crate::config::Config { porkbun_api_url: porkbun, ..(*base.cfg).clone() };
        let state = AppState { cfg: Arc::new(cfg), rate_limits: RateLimiter::new(LIMITS), ..base };
        let secret = state.cfg.app_secret.clone();
        let api = Api::serve(state, Arc::new(crate::company_filters::Claude)).await;

        let (status, body) = api.get("/v1/sending-domains/suggestions?domain=acme.com").await;
        assert_eq!((status, body["error"].as_str()), (400, Some("Connect your Porkbun account first")));
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
        sqlx::query("insert into registrar_accounts (org_id, account, api_key_enc, secret_enc) values ($1, 'acme-ops', $2, $3)")
            .bind(org)
            .bind(crate::crypto::encrypt(&secret, "pk1_good"))
            .bind(crate::crypto::encrypt(&secret, "sk1_good"))
            .execute(&db)
            .await
            .unwrap();

        let (status, found) = api.get("/v1/sending-domains/suggestions?domain=acme.com").await;
        assert_eq!((status, found[0]["domain"].as_str(), found[0]["available"].as_bool()), (200, Some("getacme.com"), Some(true)), "{found}");
        // Cleaned up and counted once, buyable first, then the rest in the order asked. A registry that didn't answer is null.
        let asked = "acmeslow.com,https://www.GetAcme.com/,acmetaken.com,getacme.com";
        let (status, found) = api.get(&format!("/v1/sending-domains/availability?domains={asked}")).await;
        assert_eq!(status, 200, "{found}");
        let summary: Vec<(&str, Option<bool>, i64)> = found.as_array().unwrap().iter().map(|a| {
            (a["domain"].as_str().unwrap(), a["available"].as_bool(), a["price_micros"].as_i64().unwrap())
        }).collect();
        assert_eq!(summary, [("getacme.com", Some(true), 11_080_000), ("acmeslow.com", None, 0), ("acmetaken.com", Some(false), 11_080_000)]);
        let too_many = (0..21).map(|i| format!("acme{i}.com")).collect::<Vec<_>>().join(",");
        for domains in ["", "acme.com,not a domain", too_many.as_str()] {
            assert_eq!(api.get(&format!("/v1/sending-domains/availability?domains={domains}")).await.0, 400, "{domains}");
        }

        sqlx::query("insert into sending_domains (org_id, domain, status, price_micros) values ($1, 'getacme.com', 'registered', 11080000)")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let (status, owned) = api.get("/v1/sending-domains").await;
        assert_eq!((status, owned[0]["domain"].as_str(), owned[0]["status"].as_str()), (200, Some("getacme.com"), Some("registered")), "{owned}");
        assert_eq!(api.send(Method::GET, KEYS[1], "/v1/sending-domains", None, None).await.2, json!([]));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_project_is_made_and_read_with_its_sender_name(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;

        // New project, as in the app: the website is cleaned up and names it, and an explore run researches the business.
        assert_eq!(api.post(KEYS[0], "/v1/projects", None, json!({})).await.0, 400);
        let (status, _, made) = api.post(KEYS[0], "/v1/projects", None, json!({ "domain": "https://www.Fieldnote.example/about" })).await;
        assert_eq!(status, 200, "{made}");
        let id: Uuid = made["project_id"].as_str().unwrap().parse().unwrap();
        let run: Uuid = made["run_id"].as_str().unwrap().parse().unwrap();
        let (kind, project): (String, Uuid) =
            sqlx::query_as("select kind, project_id from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
        assert_eq!((kind.as_str(), project), ("explore", id));
        let (status, project) = api.get(&format!("/v1/projects/{id}")).await;
        assert_eq!((status, project["domain"].as_str(), project["name"].as_str()), (200, Some("fieldnote.example"), Some("fieldnote.example")));
        assert_eq!((project["controls"]["sending"].clone(), project["controls"]["sender_name"].clone()), (json!(false), json!("")));
        assert_eq!(api.get(&format!("/v1/projects/{b}")).await.0, 404);

        // The sender name is a name: its spaces collapse, and an address or one over 100 characters is refused.
        let path = format!("/v1/projects/{a}");
        let (status, controls) = api.patch(&path, json!({ "sender_name": "  Maya   Patel " })).await;
        assert_eq!((status, controls["sender_name"].clone()), (200, json!("Maya Patel")), "{controls}");
        for name in ["maya@fieldnote.example".to_string(), "M".repeat(101), "Maya\nPatel".to_string()] {
            assert_eq!(api.patch(&path, json!({ "sender_name": name })).await.0, 400, "{name}");
        }
        assert_eq!(api.get(&path).await.1["controls"]["sender_name"], "Maya Patel");
        assert_eq!(api.patch(&path, json!({ "sender_name": "" })).await.1["sender_name"], "");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn project_controls_take_only_fields_they_know(db: PgPool) {
        let [a, b] = seed(&db).await;
        let (segment, _) = campaign(&db, a, "draft").await;
        let api = Api::start(&db, LIMITS).await;
        let project = format!("/v1/projects/{a}");

        let (status, controls) = api.patch(&project, json!({ "daily_email_limit": 80, "review_window_hours": 24, "autopilot": true })).await;
        assert_eq!(status, 200, "{controls}");
        assert_eq!((controls["daily_email_limit"].clone(), controls["review_window_hours"].clone(), controls["autopilot"].clone()), (json!(80), json!(24), json!(true)));
        assert_eq!(controls["sending"], false);
        let (status, body) = api.patch(&project, json!({ "daily_limit": 5 })).await;
        assert!(status == 422 && body["error"].as_str().unwrap().contains("unknown field `daily_limit`"), "{status} {body}");
        assert_eq!(api.patch(&project, json!({ "daily_email_limit": 0 })).await.0, 400);
        assert_eq!(api.patch(&format!("/v1/projects/{b}"), json!({ "daily_email_limit": 70 })).await.0, 404);
        let (status, unchanged) = api.patch(&project, json!({})).await;
        assert_eq!((status, unchanged["daily_email_limit"].clone()), (200, json!(80)));

        // Turning sending on needs a mailbox, like the app's Start sending, and a refusal saves nothing.
        let (status, body) = api.patch(&project, json!({ "sending": true, "daily_email_limit": 90 })).await;
        assert!(status == 409 && body["error"].as_str().unwrap().contains("mailbox"), "{status} {body}");
        assert_eq!(api.patch(&project, json!({})).await.1["daily_email_limit"], 80);
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
        sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'ana@acme.example', 'smtp.test', '', '')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        for _ in 0..2 {
            let (status, controls) = api.patch(&project, json!({ "sending": true })).await;
            assert_eq!((status, controls["sending"].clone()), (200, json!(true)), "{controls}");
        }
        assert_eq!(count(&db, "select count(*) from audit_events where action = 'project.started' and actor_key_id is not null").await, 1);
        let (_, controls) = api.patch(&project, json!({ "sending": false, "review_window_hours": null })).await;
        assert_eq!((controls["sending"].clone(), controls["review_window_hours"].clone()), (json!(false), Value::Null));
        let paused: bool = sqlx::query_scalar("select paused from project_settings where project_id = $1").bind(a).fetch_one(&db).await.unwrap();
        assert!(paused);

        // The monthly cap is set with the controls and read with what the month's emails cost.
        assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": 500_000 })).await.0, 400, "under $1");
        assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": 2_000_000 })).await.1["monthly_cap_micros"], 2_000_000);
        let cap = format!("{project}/monthly-cap");
        let want = json!({ "project_id": a, "monthly_cap_micros": 2_000_000, "spent_this_month_micros": 0, "reached": false, "resumes_at": null });
        assert_eq!(api.get(&cap).await, (200, want));
        assert_eq!(api.get(&format!("/v1/projects/{b}/monthly-cap")).await.0, 404);
        assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": null })).await.1["monthly_cap_micros"], Value::Null);

        // The campaign PATCH refuses fields it doesn't take too, rather than ignoring them.
        let (status, body) = api.patch(&format!("/v1/campaigns/{segment}"), json!({ "language": "fr", "sequence": [] })).await;
        assert_eq!(status, 422, "{body}");
        let language: String = sqlx::query_scalar("select language from segments where id = $1").bind(segment).fetch_one(&db).await.unwrap();
        assert_eq!(language, "lead");
    }

    /// A lead in the project with an inbound message `ago` before now, classified as given. Returns the lead.
    async fn reply(db: &PgPool, project: Uuid, name: &str, classification: &str, ago: &str) -> Uuid {
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, email, title, company_name, company_domain, linkedin_url)
             values ($1, $2, $2 || '@acme.example', 'Founder', $2 || ' Co', $2 || '.example', 'https://www.linkedin.com/in/' || $2) returning id",
        )
        .bind(project)
        .bind(name)
        .fetch_one(db)
        .await
        .unwrap();
        another_reply(db, project, lead, classification, ago).await;
        lead
    }

    async fn another_reply(db: &PgPool, project: Uuid, lead: Uuid, classification: &str, ago: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, classification, classification_reason, created_at)
             values ($1, $2, 'inbound', 'inbound', 'Re', 'Reply ' || $4, 'received', $3, 'Wants a call', now() - $4::interval) returning id",
        )
        .bind(project)
        .bind(lead)
        .bind(classification)
        .bind(ago)
        .fetch_one(db)
        .await
        .unwrap()
    }

    fn at(db_time: crate::models::Ts) -> String {
        db_time.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn hot_leads_are_newest_first_and_honor_since_and_project(db: PgPool) {
        let [a, b] = seed(&db).await;
        let second: Uuid = sqlx::query_scalar("insert into projects (org_id, name) select org_id, 'Second' from projects where id = $1 returning id")
            .bind(a)
            .fetch_one(&db)
            .await
            .unwrap();
        let anna = reply(&db, a, "anna", "hot", "3 days").await;
        // A second hot reply doesn't make her new again.
        another_reply(&db, a, anna, "hot", "1 day").await;
        let bo = reply(&db, second, "bo", "hot", "2 days").await;
        reply(&db, a, "cy", "question", "1 hour").await;
        reply(&db, b, "dee", "hot", "1 hour").await;
        let api = Api::start(&db, LIMITS).await;

        let (status, page) = api.get("/v1/hot-leads").await;
        assert_eq!((status, page["total"].clone()), (200, json!(2)), "{page}");
        let ids: Vec<&str> = page["items"].as_array().unwrap().iter().map(|l| l["lead_id"].as_str().unwrap()).collect();
        assert_eq!(ids, [bo.to_string(), anna.to_string()]);
        let first = &page["items"][1];
        assert_eq!((first["name"].as_str(), first["email"].as_str()), (Some("anna"), Some("anna@acme.example")));
        assert_eq!((first["title"].as_str(), first["company_domain"].as_str()), (Some("Founder"), Some("anna.example")));
        assert_eq!((first["reply"].as_str(), first["why_hot"].as_str(), first["linkedin_url"].as_str()), (Some("Reply 3 days"), Some("Wants a call"), Some("https://www.linkedin.com/in/anna")));

        let since: crate::models::Ts = sqlx::query_scalar("select now() - interval '60 hours'").fetch_one(&db).await.unwrap();
        let (_, page) = api.get(&format!("/v1/hot-leads?since={}", at(since))).await;
        assert_eq!((page["total"].clone(), page["items"][0]["lead_id"].clone()), (json!(1), json!(bo)), "{page}");
        let (_, page) = api.get(&format!("/v1/hot-leads?project_id={a}")).await;
        assert_eq!((page["total"].clone(), page["items"][0]["lead_id"].clone()), (json!(1), json!(anna)), "{page}");
        assert_eq!(api.get(&format!("/v1/hot-leads?project_id={b}")).await.0, 404);
    }

    /// MillionVerifier as a project's verifier, through a fake that accepts only `valid`.
    async fn verifier(db: &PgPool, project: Uuid, valid: &'static str) -> reqwest::Url {
        use std::collections::HashMap;

        use axum::Json;
        use axum::extract::Query;
        use axum::routing::get;

        sqlx::query("insert into project_settings (project_id, verifier, verifier_key_enc) values ($1, 'millionverifier', $2)")
            .bind(project)
            .bind(crate::crypto::encrypt("", "mv-key"))
            .execute(db)
            .await
            .unwrap();
        let check = move |Query(q): Query<HashMap<String, String>>| async move {
            Json(json!({ "result": if q["email"] == valid { "ok" } else { "invalid" }, "error": "" }))
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        tokio::spawn(async move { axum::serve(listener, Router::new().route("/api/v3/", get(check))).await });
        url
    }

    async fn spend_the_hour(db: &PgPool, project: Uuid, searches: i32) {
        sqlx::query("insert into email_search_attempts (org_id) select org_id from projects, generate_series(1, $2) where id = $1")
            .bind(project)
            .bind(searches)
            .execute(db)
            .await
            .unwrap();
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finding_an_email_needs_a_finder_and_room_in_the_hour(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let find = format!("/v1/projects/{a}/enrich/email");
        let jane = json!({ "first_name": "Jane", "last_name": "Doe", "company_domain": "https://www.acme.example/about" });

        let (status, _, body) = api.post(KEYS[0], &find, None, jane.clone()).await;
        assert!(status == 409 && body["error"].as_str().unwrap().contains("email finder or verifier"), "{status} {body}");
        let batch = json!({ "people": [jane.clone()] });
        assert_eq!(api.post(KEYS[0], &format!("{find}/batch"), None, batch.clone()).await.0, 409);
        verifier(&db, a, "").await;
        let (status, _, body) = api.post(KEYS[0], &find, None, json!({ "first_name": " ", "company_domain": "acme.example" })).await;
        assert!(status == 400 && body["error"].as_str().unwrap().contains("first_name"), "{status} {body}");

        // The leads the waterfall searched this hour count, and the 429 says when the oldest of them stops counting.
        spend_the_hour(&db, a, 200).await;
        let (status, headers, body) = api.post(KEYS[0], &find, Some("find-1"), jane.clone()).await;
        assert!(status == 429 && body["error"].as_str().unwrap().contains("200 email searches"), "{status} {body}");
        let wait: u64 = header(&headers, "retry-after").unwrap().parse().unwrap();
        assert!((3590..=3600).contains(&wait), "{wait}");
        assert_eq!(count(&db, "select count(*) from email_search_attempts").await, 200);
        // A batch waits for room instead.
        assert_eq!(api.post(KEYS[0], &format!("{find}/batch"), None, batch.clone()).await.0, 200);

        // Another organization's project and batch look like ones that don't exist.
        verifier(&db, b, "").await;
        assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/email"), None, jane.clone()).await.0, 404);
        assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/email/batch"), None, batch.clone()).await.0, 404);
        let (status, _, theirs) = api.post(KEYS[1], &format!("/v1/projects/{b}/enrich/email/batch"), None, batch).await;
        assert_eq!(status, 200, "{theirs}");
        let task = format!("/v1/enrich/email/batch/{}", theirs["task_id"].as_str().unwrap());
        assert_eq!(api.get(&task).await.0, 404);
        assert_eq!(api.send(Method::GET, KEYS[1], &task, None, None).await.0, 200);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    // The finders' answers are in waterfall.rs, since a real call here would reach the finder.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finding_a_phone_needs_a_profile_a_finder_with_mobiles_and_room_in_the_hour(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let find = format!("/v1/projects/{a}/enrich/phone");
        let finder = |project: Uuid, provider: &'static str| {
            sqlx::query("insert into project_finders (project_id, provider, key_enc, position) values ($1, $2, $3, 0)")
                .bind(project)
                .bind(provider)
                .bind(crate::crypto::encrypt("", "key"))
                .execute(&db)
        };
        let sam = json!({ "linkedin_url": " https://www.linkedin.com/in/sam-lee/ ", "email": "Sam@Lee.example" });

        for bad in [json!({ "linkedin_url": "https://www.linkedin.com/company/acme" }), json!({ "linkedin_url": "https://www.linkedin.com/in/sam", "email": "sam" })] {
            let (status, _, body) = api.post(KEYS[0], &find, None, bad).await;
            assert_eq!(status, 400, "{body}");
        }
        // Hunter finds addresses only.
        finder(a, "hunter").await.unwrap();
        let (status, _, body) = api.post(KEYS[0], &find, None, sam.clone()).await;
        assert!(status == 409 && body["error"].as_str().unwrap().contains("People Data Labs, RocketReach, Lusha or Prospeo"), "{status} {body}");

        // With one, the hour still has to have room, and the 429 comes before any finder is called.
        finder(a, "peopledatalabs").await.unwrap();
        spend_the_hour(&db, a, 200).await;
        let (status, headers, body) = api.post(KEYS[0], &find, None, sam.clone()).await;
        assert!(status == 429 && body["error"].as_str().unwrap().contains("200 email searches"), "{status} {body}");
        assert!(header(&headers, "retry-after").is_some());

        // Another organization's project looks like one that doesn't exist.
        finder(b, "peopledatalabs").await.unwrap();
        assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/phone"), None, sam).await.0, 404);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_batch_of_people_is_searched_in_the_background_within_the_hour(db: PgPool) {
        let [a, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let fake = verifier(&db, a, "jane.doe@acme.example").await;
        let state = crate::autopilot::tests::state(db.clone());
        let tick = || crate::verify::TEST_API.scope(fake.clone(), crate::email_lookups::tick(&state));
        let batch = format!("/v1/projects/{a}/enrich/email/batch");

        assert_eq!(api.post(KEYS[0], &batch, None, json!({ "people": [] })).await.0, 400);
        let too_many: Vec<Value> = (0..501).map(|_| json!({ "first_name": "Jane", "company_domain": "acme.example" })).collect();
        assert_eq!(api.post(KEYS[0], &batch, None, json!({ "people": too_many })).await.0, 400);
        let one_bad = json!({ "people": [{ "first_name": "Jane", "company_domain": "acme.example" }, { "first_name": "Sam", "company_domain": "nope" }] });
        let (status, _, body) = api.post(KEYS[0], &batch, None, one_bad).await;
        assert!(status == 400 && body["error"].as_str().unwrap().starts_with("people[1]"), "{status} {body}");

        let people = json!({ "people": [
            { "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example", "client_ref": "crm-1" },
            { "first_name": "Sam", "last_name": "Lee", "company_domain": "lee.example" },
        ] });
        let (status, _, started) = api.post(KEYS[0], &batch, None, people).await;
        assert_eq!((status, started["status"].as_str(), started["total"].as_i64()), (200, Some("pending"), Some(2)), "{started}");
        let task = format!("/v1/enrich/email/batch/{}", started["task_id"].as_str().unwrap());
        let (_, waiting) = api.get(&task).await;
        assert_eq!((waiting["status"].as_str(), waiting["searched"].as_i64()), (Some("pending"), Some(0)), "{waiting}");
        assert_eq!((waiting["results"][0]["status"].as_str(), waiting["results"][0]["client_ref"].as_str()), (Some("pending"), Some("crm-1")));

        tick().await.unwrap();
        let (status, done) = api.get(&task).await;
        assert_eq!((status, done["status"].as_str(), done["searched"].as_i64()), (200, Some("completed"), Some(2)), "{done}");
        let jane = &done["results"][0];
        let found = (jane["status"].as_str(), jane["email"].as_str(), jane["email_status"].as_str());
        assert_eq!(found, (Some("guessed"), Some("jane.doe@acme.example"), Some("verified")));
        assert_eq!((jane["source"].as_str(), jane["client_ref"].as_str(), jane["phone"].clone()), (Some("first.last"), Some("crm-1"), Value::Null));
        let sam = &done["results"][1];
        assert_eq!((sam["status"].as_str(), sam["email"].clone(), sam["client_ref"].as_str()), (Some("not_found"), Value::Null, Some("")), "{sam}");
        assert_eq!(count(&db, "select count(*) from email_search_attempts").await, 2);
        assert_eq!(count(&db, "select count(*) from leads").await, 0, "nobody became a lead");

        // With the hour spent a batch waits, and goes on once there's room.
        spend_the_hour(&db, a, 198).await;
        let jane = json!({ "people": [{ "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example" }] });
        let (_, _, started) = api.post(KEYS[0], &batch, None, jane).await;
        let task = format!("/v1/enrich/email/batch/{}", started["task_id"].as_str().unwrap());
        tick().await.unwrap();
        assert_eq!(api.get(&task).await.1["status"], "pending");
        sqlx::query("update email_search_attempts set at = now() - interval '2 hours'").execute(&db).await.unwrap();
        tick().await.unwrap();
        let (_, done) = api.get(&task).await;
        assert_eq!((done["status"].as_str(), done["results"][0]["email"].as_str()), (Some("completed"), Some("jane.doe@acme.example")), "{done}");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn inbox_changes_return_each_reply_once_across_pages(db: PgPool) {
        let [a, b] = seed(&db).await;
        let anna = reply(&db, a, "anna", "hot", "50 minutes").await;
        for ago in ["40 minutes", "30 minutes", "20 minutes"] {
            another_reply(&db, a, anna, "question", ago).await;
        }
        // Two at the same moment, told apart by id.
        sqlx::query("update email_messages set created_at = (select created_at from email_messages where body = 'Reply 30 minutes') where body = 'Reply 20 minutes'")
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'initial', 'sent')")
            .bind(a)
            .bind(anna)
            .execute(&db)
            .await
            .unwrap();
        let fresh = another_reply(&db, a, anna, "other", "0 seconds").await;
        reply(&db, b, "dee", "hot", "10 minutes").await;
        let expected: Vec<String> = sqlx::query_scalar(
            "select id::text from email_messages where project_id = $1 and direction = 'inbound' and id <> $2 order by created_at, id",
        )
        .bind(a)
        .bind(fresh)
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(expected.len(), 4);
        let api = Api::start(&db, LIMITS).await;
        let changes = format!("/v1/projects/{a}/inbox/changes");

        let (mut seen, mut cursor) = (vec![], String::new());
        loop {
            let (status, page) = api.get(&format!("{changes}?limit=3&cursor={cursor}")).await;
            assert_eq!(status, 200, "{page}");
            seen.extend(page["items"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()));
            cursor = page["next_cursor"].as_str().unwrap().to_string();
            if page["has_more"] == false {
                break;
            }
        }
        assert_eq!(seen, expected, "each settled reply once, in order, and the one still settling not yet");

        // Once it has settled it comes next, and then the cursor stays put.
        sqlx::query("update email_messages set created_at = now() - interval '6 seconds' where id = $1").bind(fresh).execute(&db).await.unwrap();
        let (_, page) = api.get(&format!("{changes}?cursor={cursor}")).await;
        assert_eq!((page["items"].as_array().unwrap().len(), page["items"][0]["id"].clone()), (1, json!(fresh)), "{page}");
        assert_eq!(page["items"][0]["classification"], "other");
        let cursor = page["next_cursor"].as_str().unwrap().to_string();
        let (_, page) = api.get(&format!("{changes}?cursor={cursor}")).await;
        assert_eq!((page["items"].as_array().unwrap().len(), page["next_cursor"].as_str()), (0, Some(cursor.as_str())));

        let third: crate::models::Ts = sqlx::query_scalar("select created_at from email_messages where id::text = $1").bind(&expected[1]).fetch_one(&db).await.unwrap();
        let (_, page) = api.get(&format!("{changes}?since={}", at(third))).await;
        let ids: Vec<&str> = page["items"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
        assert_eq!(ids, [expected[2].as_str(), expected[3].as_str(), fresh.to_string().as_str()]);
        assert_eq!(api.get(&format!("{changes}?cursor=nope")).await.0, 400);
        assert_eq!(api.get(&format!("/v1/projects/{b}/inbox/changes")).await.0, 404);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_inbox_and_its_changes_filter_by_campaign(db: PgPool) {
        let [a, _] = seed(&db).await;
        let mut campaigns = vec![];
        for (name, ago) in [("anna", "50 minutes"), ("bo", "40 minutes")] {
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, $2) returning id")
                .bind(a)
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            let lead = reply(&db, a, name, "question", ago).await;
            sqlx::query("update leads set segment_id = $2 where id = $1").bind(lead).bind(segment).execute(&db).await.unwrap();
            sqlx::query("update email_messages set segment_id = $2 where lead_id = $1").bind(lead).bind(segment).execute(&db).await.unwrap();
            campaigns.push((segment, lead));
        }
        reply(&db, a, "cy", "question", "30 minutes").await;
        let api = Api::start(&db, LIMITS).await;
        let [(anna_campaign, anna), (bo_campaign, bo)] = [campaigns[0], campaigns[1]];

        let (status, page) = api.get(&format!("/v1/inbox?segment_id={anna_campaign}")).await;
        assert_eq!(status, 200, "{page}");
        let first = (&page["total"], &page["items"][0]["lead_id"], &page["items"][0]["campaign_id"]);
        assert_eq!(first, (&json!(1), &json!(anna), &json!(anna_campaign)));
        assert_eq!(api.get("/v1/inbox?segment_id=").await.1["total"], 3);

        let (_, changes) = api.get(&format!("/v1/projects/{a}/inbox/changes?segment_id={bo_campaign}")).await;
        let leads: Vec<&Value> = changes["items"].as_array().unwrap().iter().map(|m| &m["lead_id"]).collect();
        assert_eq!((leads, &changes["items"][0]["campaign_id"]), (vec![&json!(bo)], &json!(bo_campaign)), "{changes}");
        assert_eq!(api.get(&format!("/v1/projects/{a}/inbox/changes")).await.1["items"].as_array().unwrap().len(), 3);
    }

    fn import_lead(email: &str, title: &str) -> Value {
        json!({ "email": email, "first_name": "Ana", "last_name": "Silva", "company_domain": "smile.example", "job_title": title })
    }

    /// Polls an import until it's no longer pending.
    async fn finished_import(api: &Api, task: &str) -> Value {
        for _ in 0..200 {
            let (status, body) = api.get(&format!("/v1/campaigns/import/{task}")).await;
            assert_eq!(status, 200, "{body}");
            if body["status"] != "pending" {
                return body;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("the import never finished");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_lead_list_becomes_a_campaign_of_exactly_the_leads_it_could_import(db: PgPool) {
        let [a, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        // ana@smile.example is already a lead of another campaign, waiting on a draft.
        let (dentists, _) = campaign(&db, a, "working").await;
        sqlx::query(
            "insert into leads (project_id, segment_id, full_name, email, last_contacted_at) values ($1, $2, 'Raj', 'raj@smile.example', now())",
        )
            .bind(a)
            .bind(dentists)
            .execute(&db)
            .await
            .unwrap();
        let (status, _, body) = api.post(KEYS[0], "/v1/suppressions", None, json!({ "values": ["blocked@smile.example"] })).await;
        assert_eq!(status, 200, "{body}");

        let mut first = import_lead("bea@smile.example", "Owner");
        first["client_ref"] = json!("crm-1");
        first["linkedin_url"] = json!("https://www.linkedin.com/in/bea");
        let leads = json!([
            first,
            import_lead("caio@smile.example", "Dentist"),
            import_lead("dora@smile.example", " "),
            { "email": null, "first_name": "Eli" },
            import_lead("BEA@smile.example", "Owner"),
            import_lead("blocked@smile.example", "Owner"),
            import_lead("raj@smile.example", "Owner"),
            import_lead("ana@smile.example", "Owner"),
        ]);
        let body =
            json!({ "project_id": a, "name": "Smile clinics", "leads": leads, "instructions": "Mention our Lisbon office", "language": "German" });
        let (status, _, started) = api.post(KEYS[0], "/v1/campaigns/import", None, body).await;
        assert_eq!((status, &started["status"]), (200, &json!("pending")), "{started}");
        let task = started["task_id"].as_str().unwrap();
        let campaign_id: Uuid = started["campaign_id"].as_str().unwrap().parse().unwrap();

        let done = finished_import(&api, task).await;
        assert_eq!(done["status"], "completed", "{done}");
        assert_eq!(done["result"], json!({ "campaign_id": campaign_id, "imported": 2, "skipped": 2, "deduped": 4 }));

        let (status, page) = api.get(&format!("/v1/campaigns/import/{task}/rows?per_page=3&page=1")).await;
        assert_eq!((status, page["total"].clone(), page["items"].as_array().unwrap().len()), (200, json!(8), 3), "{page}");
        let (_, page) = api.get(&format!("/v1/campaigns/import/{task}/rows")).await;
        let outcomes: Vec<(String, String)> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| (r["status"].as_str().unwrap().to_string(), r["reason"].as_str().unwrap_or_default().to_string()))
            .collect();
        let expected = [
            ("imported", ""),
            ("imported", ""),
            ("skipped", "missing_job_title"),
            ("skipped", "missing_email"),
            ("deduped", "duplicate_in_request"),
            ("deduped", "do_not_contact"),
            ("deduped", "already_contacted"),
            ("deduped", "already_in_project"),
        ];
        assert_eq!(outcomes, expected.map(|(s, r)| (s.to_string(), r.to_string())));
        assert_eq!((page["items"][0]["row"].clone(), page["items"][0]["client_ref"].clone()), (json!(0), json!("crm-1")));

        // The campaign holds those two leads and no others, and they are the people the rows name.
        let held: Vec<(Uuid, String, String, String)> =
            sqlx::query_as("select id, email::text, title, linkedin_url from leads where segment_id = $1 order by email")
                .bind(campaign_id)
                .fetch_all(&db)
                .await
                .unwrap();
        assert_eq!(
            held.iter().map(|(_, email, title, _)| (email.as_str(), title.as_str())).collect::<Vec<_>>(),
            [("bea@smile.example", "Owner"), ("caio@smile.example", "Dentist")]
        );
        assert_eq!(held[0].3, "https://www.linkedin.com/in/bea");
        assert_eq!(page["items"][0]["person_id"], json!(held[0].0));
        assert_eq!(page["items"][1]["person_id"], json!(held[1].0));
        assert_eq!(page["items"][7]["person_id"], Value::Null);
        let ana: Uuid = sqlx::query_scalar("select segment_id from leads where email = 'ana@smile.example'").fetch_one(&db).await.unwrap();
        assert_eq!(ana, dentists, "a lead already in the project keeps its campaign");

        let (_, detail) = api.get(&format!("/v1/campaigns/{campaign_id}")).await;
        assert_eq!(
            (&detail["name"], &detail["status"], &detail["angle"], &detail["language"], &detail["leads"]),
            (&json!("Smile clinics"), &json!("draft"), &json!("Mention our Lisbon office"), &json!("de"), &json!(2)),
        );

        // After 30 days the task and its rows go, and the campaign and its leads stay.
        sqlx::query("update campaign_imports set created_at = now() - interval '31 days'").execute(&db).await.unwrap();
        crate::agent::expire_visitor_data(&db).await.unwrap();
        assert_eq!(count(&db, "select (select count(*) from campaign_imports) + (select count(*) from campaign_import_rows)").await, 0);
        assert_eq!(api.get(&format!("/v1/campaigns/import/{task}")).await.0, 404);
        assert_eq!(api.get(&format!("/v1/campaigns/{campaign_id}")).await.1["leads"], 2);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaign_imports_check_their_request_belong_to_one_organization_and_replay_by_key(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let import = |project: Uuid, leads: Value| json!({ "project_id": project, "name": "Smile clinics", "leads": leads });
        let one = json!([import_lead("bea@smile.example", "Owner")]);

        let (status, headers, first) = api.post(KEYS[0], "/v1/campaigns/import", Some("import-1"), import(a, one.clone())).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None), "{first}");
        let (status, headers, again) = api.post(KEYS[0], "/v1/campaigns/import", Some("import-1"), import(a, one.clone())).await;
        assert_eq!((status, header(&headers, "idempotent-replayed"), &again["task_id"]), (200, Some("true"), &first["task_id"]));
        assert_eq!(count(&db, "select (select count(*) from campaign_imports) + (select count(*) from segments)").await, 2);
        let task = first["task_id"].as_str().unwrap();
        assert_eq!(finished_import(&api, task).await["result"]["imported"], 1);

        // Another organization sees none of it and can't import into this project.
        for path in [format!("/v1/campaigns/import/{task}"), format!("/v1/campaigns/import/{task}/rows")] {
            assert_eq!(api.send(Method::GET, KEYS[1], &path, None, None).await.0, 404, "{path}");
        }
        assert_eq!(api.post(KEYS[1], "/v1/campaigns/import", None, import(a, one.clone())).await.0, 404);
        assert_eq!(api.post(KEYS[1], "/v1/campaigns/import", None, import(b, one.clone())).await.0, 200);

        // A refused request makes no campaign.
        let before = count(&db, "select count(*) from segments").await;
        assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, import(a, json!([]))).await.0, 400);
        let too_many = Value::Array(vec![json!({}); crate::campaign_imports::MAX_LEADS + 1]);
        assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, import(a, too_many)).await.0, 400);
        let mut unnamed = import(a, one.clone());
        unnamed["name"] = json!(" ");
        assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, unnamed).await.0, 400);
        let mut klingon = import(a, one.clone());
        klingon["language"] = json!("Klingon");
        assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, klingon).await.0, 400);
        let mut follow_ups = import(a, one.clone());
        follow_ups["followup_instructions"] = json!("Bump it after 3 days");
        let (status, _, body) = api.post(KEYS[0], "/v1/campaigns/import", None, follow_ups).await;
        assert_eq!(status, 422, "{body}");
        assert_eq!(count(&db, "select count(*) from segments").await, before);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn my_copy_switches_mode_and_source_without_losing_text_and_stays_in_its_organization(db: PgPool) {
        let [a, b] = seed(&db).await;
        let (segment, _) = campaign(&db, a, "draft").await;
        let (other, _) = campaign(&db, b, "draft").await;
        // A lead my_copy's pinned split keeps on the user's text.
        sqlx::query("insert into leads (id, project_id, segment_id, full_name, first_name, company_name, email, created_at)
                     values ('00000000-0000-0000-0000-000000000001', $1, $2, 'Ana Ruiz', 'Ana', 'Smile', 'ana@ruiz.example', now() - interval '1 day')")
            .bind(a)
            .bind(segment)
            .execute(&db)
            .await
            .unwrap();
        let api = Api::start(&db, LIMITS).await;
        let mine = format!("/v1/campaigns/{segment}/my-copy");
        let project = format!("/v1/projects/{a}/my-copy");
        let put = |path: String, body: Value| {
            let api = api.clone();
            async move {
                let (status, _, body) = api.send(Method::PUT, KEYS[0], &path, None, Some(body)).await;
                (status, body)
            }
        };

        // A campaign starts on the project's first email, the AI one, and can't change it here while it follows the project.
        let (status, copy) = api.get(&mine).await;
        assert_eq!(status, 200, "{copy}");
        assert_eq!((copy["source"].clone(), copy["mode"].clone(), copy["body"].clone()), (json!("project"), json!("instructions"), json!("")));
        assert!(copy["results"]["note"].as_str().unwrap().starts_with("Not enough data"));
        assert_eq!(api.patch(&mine, json!({ "body": "Hi" })).await.0, 409);

        // The project's text is checked before anything is saved.
        let (status, body) = api.patch(&project, json!({ "mode": "my_copy" })).await;
        assert!(status == 422 && body["error"].as_str().unwrap().contains("Write the email's body"), "{status} {body}");
        let (status, body) = api.patch(&project, json!({ "mode": "my_copy", "body": "Hi {{frist_name}}" })).await;
        assert!(status == 422 && body["error"].as_str().unwrap().contains("{{frist_name}} isn't a variable"), "{status} {body}");
        assert_eq!(api.patch(&project, json!({ "tone": "direct" })).await.0, 422);
        assert_eq!(api.patch(&project, json!({ "mode": "sometimes" })).await.0, 422);
        assert_eq!(api.get(&project).await.1["mode"], "instructions");
        let (status, saved) = api.patch(&project, json!({ "mode": "my_copy", "subject": "{Hi|Hello} {{first_name}}", "body": "How is {{company}}?" })).await;
        assert_eq!(status, 200, "{saved}");
        assert_eq!(saved["campaign_ids"], json!([segment]));
        assert_eq!(api.get(&mine).await.1["body"], "How is {{company}}?");

        // What leads get: the text filled in for each, with no Claude call.
        let (status, preview) = api.get(&format!("{mine}/preview")).await;
        assert_eq!(status, 200, "{preview}");
        let emails = preview["emails"].as_array().unwrap();
        let ana = emails.iter().find(|e| e["lead_id"] == "00000000-0000-0000-0000-000000000001").unwrap();
        assert!(ana["subject"] == "Hi Ana" || ana["subject"] == "Hello Ana", "{ana}");
        assert_eq!((ana["body"].clone(), ana["gets"].clone()), (json!("How is Smile?"), json!("my_copy")));

        // Custom starts as a copy of the project's, and switching either way loses neither text.
        let (status, custom) = put(format!("{mine}/source"), json!({ "source": "campaign" })).await;
        assert_eq!((status, custom["source"].clone(), custom["body"].clone()), (200, json!("campaign"), json!("How is {{company}}?")));
        assert_eq!(api.patch(&mine, json!({ "body": "Does your {{job_title|team}} plan visits?" })).await.0, 200);
        let (_, custom) = api.patch(&mine, json!({ "mode": "instructions" })).await;
        assert_eq!((custom["mode"].clone(), custom["body"].clone()), (json!("instructions"), json!("Does your {{job_title|team}} plan visits?")));
        let (_, back) = put(format!("{mine}/source"), json!({ "source": "project" })).await;
        assert_eq!((back["mode"].clone(), back["body"].clone()), (json!("my_copy"), json!("How is {{company}}?")));
        let (_, again) = put(format!("{mine}/source"), json!({ "source": "campaign" })).await;
        assert_eq!((again["mode"].clone(), again["body"].clone()), (json!("instructions"), json!("Does your {{job_title|team}} plan visits?")));
        assert_eq!(put(format!("{mine}/source"), json!({ "source": "elsewhere" })).await.0, 422);
        let (_, kept) = api.get(&project).await;
        assert_eq!((kept["body"].clone(), kept["campaign_ids"].clone()), (json!("How is {{company}}?"), json!([])));

        // Another organization's campaign and project are 404 on every route.
        let theirs = format!("/v1/campaigns/{other}/my-copy");
        assert_eq!(api.get(&theirs).await.0, 404);
        assert_eq!(api.patch(&theirs, json!({ "mode": "instructions" })).await.0, 404);
        assert_eq!(put(format!("{theirs}/source"), json!({ "source": "campaign" })).await.0, 404);
        assert_eq!(api.get(&format!("{theirs}/preview")).await.0, 404);
        assert_eq!(api.get(&format!("/v1/projects/{b}/my-copy")).await.0, 404);
        assert_eq!(api.patch(&format!("/v1/projects/{b}/my-copy"), json!({ "body": "Hi" })).await.0, 404);
        let theirs: String = sqlx::query_scalar("select my_copy_source from segments where id = $1").bind(other).fetch_one(&db).await.unwrap();
        assert_eq!(theirs, "project");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn erased_leads_come_once_each_oldest_first_for_the_keys_organization(db: PgPool) {
        async fn erased(db: &PgPool, project: Uuid, ago: &str) -> Uuid {
            sqlx::query_scalar(
                "insert into erased_leads (lead_id, org_id, project_id, forgotten, erased_at)
                 select gen_random_uuid(), org_id, id, false, now() - $2::interval from projects where id = $1 returning lead_id",
            )
            .bind(project)
            .bind(ago)
            .fetch_one(db)
            .await
            .unwrap()
        }
        fn ids(page: &Value) -> Vec<Uuid> {
            page["items"].as_array().unwrap().iter().map(|e| e["lead_id"].as_str().unwrap().parse().unwrap()).collect()
        }
        let [a, b] = seed(&db).await;
        let other: Uuid = sqlx::query_scalar("insert into projects (org_id, name) select org_id, 'Other' from projects where id = $1 returning id")
            .bind(a)
            .fetch_one(&db)
            .await
            .unwrap();
        let (first, second, third) = (erased(&db, a, "3 minutes").await, erased(&db, other, "2 minutes").await, erased(&db, a, "1 minute").await);
        erased(&db, a, "-30 seconds").await;
        erased(&db, b, "1 minute").await;
        let api = Api::start(&db, LIMITS).await;

        // The organization's projects, a page at a time. One erased this moment waits a few seconds, and B's never show.
        let (status, page) = api.get("/v1/erased-leads?limit=2").await;
        assert_eq!((status, ids(&page), page["has_more"].clone()), (200, vec![first, second], json!(true)), "{page}");
        let (_, page) = api.get(&format!("/v1/erased-leads?limit=2&cursor={}", page["next_cursor"].as_str().unwrap())).await;
        assert_eq!((ids(&page), page["has_more"].clone()), (vec![third], json!(false)));
        let cursor = page["next_cursor"].as_str().unwrap().to_string();
        let (_, page) = api.get(&format!("/v1/erased-leads?cursor={cursor}")).await;
        assert_eq!((ids(&page), page["next_cursor"].as_str()), (vec![], Some(cursor.as_str())), "an empty page keeps the cursor");

        let (_, page) = api.get(&format!("/v1/erased-leads?project_id={a}")).await;
        assert_eq!(ids(&page), [first, third]);
        let since = page["items"][0]["erased_at"].as_str().unwrap().to_string();
        let (_, page) = api.get(&format!("/v1/erased-leads?since={}", since.replace('+', "%2B"))).await;
        assert_eq!(ids(&page), [second, third], "since leaves out one erased at exactly that time");
        assert_eq!(api.get(&format!("/v1/erased-leads?project_id={b}")).await.0, 404);
        assert_eq!(api.get("/v1/erased-leads?cursor=nope").await.0, 400);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reply_marked_answered_leaves_needs_reply_until_the_lead_writes_again(db: PgPool) {
        let [a, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, email, status, remind_at)
             values ($1, 'Ana', 'ana@smile.example', 'replied', now() + interval '1 day') returning id",
        )
        .bind(a)
        .fetch_one(&db)
        .await
        .unwrap();
        let path = format!("/v1/leads/{lead}/mark-replied");
        let (status, _, body) = api.post(KEYS[0], &path, None, json!({})).await;
        assert_eq!(status, 409, "{body}");

        let write = |at: &'static str| {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, status, created_at)
                 values ($1, $2, 'inbound', 'inbound', 'received', $3::timestamptz)",
            )
            .bind(a)
            .bind(lead)
            .bind(at)
            .execute(&db)
        };
        write("2026-11-20T09:00:00Z").await.unwrap();
        write("2026-11-20T10:00:00Z").await.unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'auto_reply', 'draft')")
            .bind(a)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        let needs_reply = || async { api.get("/v1/inbox?filter=needs_reply").await.1["total"].clone() };
        assert_eq!(needs_reply().await, json!(1));

        assert_eq!(api.post(KEYS[1], &path, None, json!({})).await.0, 404, "another organization's lead");
        let (status, _, body) = api.post(KEYS[0], &path, None, json!({})).await;
        assert_eq!((status, body), (200, json!({ "lead_id": lead, "answered_through": "2026-11-20T10:00:00Z" })));
        assert_eq!(needs_reply().await, json!(0));
        let left: (String, bool) = sqlx::query_as(
            "select l.status, l.remind_at is null and not exists (select 1 from email_messages e where e.lead_id = l.id and e.status = 'draft')
             from leads l where l.id = $1",
        )
        .bind(lead)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(left, ("replied".to_string(), true), "the status stays, and the reminder and the agent's draft are gone");

        write("2026-11-20T11:00:00Z").await.unwrap();
        assert_eq!(needs_reply().await, json!(1), "a new reply brings the thread back");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn tasks_of_every_type_are_listed_newest_first_for_the_keys_organization(db: PgPool) {
        let [a, b] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
        let second: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Second') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        /// Runs the insert with {ago} as that many minutes before now, so the rows have an order.
        async fn task(db: &PgPool, sql: &str, project: Uuid, minutes_ago: i32) -> Uuid {
            let sql = sql.replace("{ago}", "now() - make_interval(mins => $2)");
            sqlx::query_scalar(&sql).bind(project).bind(minutes_ago).fetch_one(db).await.unwrap()
        }
        let import = task(
            &db,
            "insert into campaign_imports (org_id, project_id, status, created_at) select org_id, id, 'failed', {ago} from projects where id = $1
             returning id",
            second,
            5,
        )
        .await;
        let batch_sql = "insert into email_lookup_batches (org_id, project_id, created_at) select org_id, id, {ago} from projects where id = $1
                         returning id";
        let batch = task(&db, batch_sql, a, 4).await;
        // A run counts for the organization through its project, or through org_id when it has none.
        let found_sql = "insert into agent_runs (project_id, kind, status, created_at, finished_at)
                         values ($1, 'find_people', 'succeeded', {ago}, now()) returning id";
        let found = task(&db, found_sql, a, 3).await;
        let cancelled_sql = "insert into agent_runs (org_id, kind, status, created_at)
                             select org_id, 'classify_reply', 'cancelled', {ago} from projects where id = $1 returning id";
        let cancelled = task(&db, cancelled_sql, a, 2).await;
        let writing_sql = "insert into agent_runs (project_id, kind, status, created_at) values ($1, 'write_emails', 'running', {ago}) returning id";
        let writing = task(&db, writing_sql, second, 1).await;
        let theirs = task(&db, "insert into agent_runs (project_id, kind, created_at) values ($1, 'find_people', {ago}) returning id", b, 0).await;

        let ids = |page: &Value| -> Vec<Uuid> {
            page["items"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().parse().unwrap()).collect()
        };
        let (status, all) = api.get("/v1/tasks").await;
        assert_eq!((status, &all["total"]), (200, &json!(5)), "{all}");
        assert_eq!(ids(&all), [writing, cancelled, found, batch, import]);
        let summary: Vec<(&str, Option<&str>, &str, String)> = all["items"].as_array().unwrap().iter().map(|t| {
            (t["type"].as_str().unwrap(), t["kind"].as_str(), t["status"].as_str().unwrap(), t["url"].as_str().unwrap().to_string())
        }).collect();
        assert_eq!(summary, [
            ("run", Some("write_emails"), "pending", format!("/v1/runs/{writing}")),
            ("run", Some("classify_reply"), "failed", format!("/v1/runs/{cancelled}")),
            ("run", Some("find_people"), "completed", format!("/v1/runs/{found}")),
            ("email_batch", None, "pending", format!("/v1/enrich/email/batch/{batch}")),
            ("campaign_import", None, "failed", format!("/v1/campaigns/import/{import}")),
        ]);
        // Each url is the route that follows the task.
        for task in all["items"].as_array().unwrap() {
            let (status, body) = api.get(task["url"].as_str().unwrap()).await;
            assert_eq!(status, 200, "{task} {body}");
        }

        assert_eq!(ids(&api.get("/v1/tasks?status=pending").await.1), [writing, batch]);
        assert_eq!(ids(&api.get("/v1/tasks?type=run&kind=find_people").await.1), [found]);
        assert_eq!(ids(&api.get(&format!("/v1/tasks?project_id={second}")).await.1), [writing, import]);
        let (_, page) = api.get("/v1/tasks?per_page=2&page=2").await;
        assert_eq!((ids(&page), &page["total"]), (vec![found, batch], &json!(5)));
        for bad in ["status=done", "type=job"] {
            assert_eq!(api.get(&format!("/v1/tasks?{bad}")).await.0, 400, "{bad}");
        }
        assert_eq!(ids(&api.send(Method::GET, KEYS[1], "/v1/tasks", None, None).await.2), [theirs]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_campaigns_send_state_names_what_holds_it_for_the_keys_organization(db: PgPool) {
        let [a, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let (segment, email) = campaign(&db, a, "working").await;
        // Sending, every hour of every day, so the test doesn't depend on when it runs. New projects start paused.
        sqlx::query(
            "insert into project_settings (project_id, paused, send_days, send_start_hour, send_end_hour) values ($1, false, '{1,2,3,4,5,6,7}', 0, 24)
             on conflict (project_id) do update set paused = false, send_days = excluded.send_days, send_start_hour = 0, send_end_hour = 24",
        )
        .bind(a)
        .execute(&db)
        .await
        .unwrap();
        let path = format!("/v1/campaigns/{segment}/send-state");
        let state = || async { api.get(&path).await.1 };

        assert_eq!(state().await["state"], "no_mailbox");
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
        sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'sam@acme.test', 'x', '', '')")
            .bind(org)
            .execute(&db)
            .await
            .unwrap();
        let waiting = state().await;
        assert_eq!(
            (&waiting["state"], &waiting["message"], &waiting["sending_now"], &waiting["today"]["drafts"]),
            (&json!("awaiting_approval"), &json!("1 email waits for approval"), &json!(false), &json!(1)),
            "{waiting}"
        );
        assert_eq!(waiting["next_action"], format!("POST /v1/campaigns/{segment}/approve"));
        assert_eq!(waiting["allowed_actions"], json!(["stop", "approve", "archive"]));

        sqlx::query("update email_messages set status = 'approved' where id = $1").bind(email).execute(&db).await.unwrap();
        let sending = state().await;
        let summary = (&sending["state"], &sending["sending_now"], &sending["today"]["queued"]);
        assert_eq!(summary, (&json!("sending"), &json!(true), &json!(1)), "{sending}");
        assert_eq!(sending["next_action"], Value::Null);

        // A follow-up waits for its thread's mailbox, here one still warming up, even with another mailbox free.
        let warming: Uuid = sqlx::query_scalar(
            "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, warmup, total_daily_limit)
             values ($1, 'ana@acme.test', 'x', '', '', true, 0) returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("update email_messages set kind = 'follow_up', mailbox_id = $2 where id = $1")
            .bind(email)
            .bind(warming)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(state().await["state"], "mailboxes_full");
        // A first email due too can go out from the free mailbox, until autopilot's lead cap holds it.
        sqlx::query(
            "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Bo', 'bo@smile.example') returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
             select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'approved' from l",
        )
        .bind(a)
        .bind(segment)
        .execute(&db)
        .await
        .unwrap();
        assert_eq!(state().await["state"], "sending");
        sqlx::query("update project_settings set autopilot = true, autopilot_lead_cap = 0 where project_id = $1").bind(a).execute(&db).await.unwrap();
        assert_eq!(state().await["state"], "mailboxes_full", "the follow-up still waits for its mailbox, and the first email for the 1st");

        sqlx::query("update project_settings set paused = true where project_id = $1").bind(a).execute(&db).await.unwrap();
        let paused = state().await;
        let resume = format!("PATCH /v1/projects/{a} with {{\"sending\": true}}");
        assert_eq!((&paused["state"], &paused["next_action"]), (&json!("project_paused"), &json!(resume)));

        assert_eq!(api.send(Method::GET, KEYS[1], &path, None, None).await.0, 404);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn people_are_found_at_companies_picked_by_domain_in_the_titles_asked_for(db: PgPool) {
        let [project, _] = seed(&db).await;
        let api = Api::start(&db, LIMITS).await;
        let (segment, _) = campaign(&db, project, "working").await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
        // Two the database lists with an email on a do-not-contact list, one of the organization's and one of a list for the project.
        sqlx::query(
            "insert into directory_companies (domain, name, website, email, city, country_code, source_id, release)
             values ('smile.example', 'Smile Dental', 'https://smile.example', null, 'Leeds', 'GB', 'x', 'r'),
                    ('mailed.example', 'Mailed', 'https://mailed.example', 'hi@mailed.example', 'Leeds', 'GB', 'x', 'r'),
                    ('listed.example', 'Listed', 'https://listed.example', 'hi@listed.example', 'Leeds', 'GB', 'x', 'r')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'hi@mailed.example', 'email', 'manual')").bind(org).execute(&db).await.unwrap();
        sqlx::query(
            "with l as (insert into suppression_lists (org_id, name, kind, project_ids) values ($1, 'Customers', 'people', array[$2]) returning id)
             insert into suppression_list_entries (list_id, org_id, value) select id, $1, 'hi@listed.example' from l",
        )
        .bind(org)
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
        // One the project has, one it excluded, and one on the do-not-contact list.
        sqlx::query("insert into companies (project_id, domain, name, status) values ($1, 'known.example', 'Known', 'people_found'), ($1, 'gone.example', 'Gone', 'excluded')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.example', 'domain', 'manual')").bind(org).execute(&db).await.unwrap();

        let path = format!("/v1/segments/{segment}/find-people");
        let domains =
            ["https://Smile.example/team", "bare.example", "known.example", "gone.example", "blocked.example", "smile.example", "mailed.example", "listed.example"];
        let body = json!({ "domains": domains, "titles": [" Practice Manager ", "Owner", ""], "per_company": 1 });
        let (status, _, body) = api.post(KEYS[0], &path, None, body).await;
        assert_eq!(status, 200, "{body}");
        let run: Uuid = body["run_id"].as_str().unwrap().parse().unwrap();
        let input: Value = sqlx::query_scalar("select input from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
        assert_eq!((input["roles"].clone(), input["max_per_company"].clone()), (json!("Practice Manager, Owner"), json!(1)));
        let ids: Vec<Uuid> = serde_json::from_value(input["company_ids"].clone()).unwrap();
        let searched: Vec<(String, String, bool)> =
            sqlx::query_as("select domain::text, name, segment_id is not distinct from $2 from companies where id = any($1) order by domain")
                .bind(&ids)
                .bind(segment)
                .fetch_all(&db)
                .await
                .unwrap();
        let searched: Vec<_> = searched.iter().map(|(d, n, here)| (d.as_str(), n.as_str(), *here)).collect();
        assert_eq!(searched, [("bare.example", "bare.example", true), ("known.example", "Known", false), ("smile.example", "Smile Dental", true)]);
        assert_eq!(count(&db, "select count(*) from companies where domain in ('mailed.example', 'listed.example')").await, 0, "not even bare");

        crate::agent::fake::answer_with(run, |_| json!({ "people": [] }));
        crate::agent::start_now(&crate::autopilot::tests::state(db.clone()), run).await.unwrap();
        let mut status = String::new();
        for _ in 0..250 {
            status = sqlx::query_scalar("select status from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
            if status != "running" {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(status, "succeeded");
        let prompt = crate::agent::fake::prompts(run).concat();
        assert!(prompt.contains("Only people in these roles: Practice Manager, Owner.") && prompt.contains("bare.example"), "{prompt}");

        for (body, why) in [
            (json!({ "domains": ["blocked.example", "gone.example"] }), "every one left out"),
            (json!({ "domains": ["not a domain"] }), "not a domain"),
            (json!({ "domains": [] }), "no domains"),
            (json!({ "domains": ["bare.example"], "company_ids": [] }), "both"),
            (json!({ "per_company": 6 }), "too many a company"),
            (json!({ "titles": vec!["CEO"; 21] }), "too many titles"),
        ] {
            assert_eq!(api.post(KEYS[0], &path, None, body).await.0, 400, "{why}");
        }
        assert_eq!(api.post(KEYS[1], &path, None, json!({ "domains": ["bare.example"] })).await.0, 404, "another organization's campaign");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_company_another_request_adds_at_the_same_moment_is_searched_too(db: PgPool) {
        let [project, _] = seed(&db).await;
        let (segment, _) = campaign(&db, project, "working").await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
        // The other request's insert, not committed yet, which this one's waits on.
        let mut other = db.begin().await.unwrap();
        sqlx::query("insert into companies (project_id, segment_id, domain, name) values ($1, $2, 'race.example', 'race.example')")
            .bind(project)
            .bind(segment)
            .execute(&mut *other)
            .await
            .unwrap();
        let ask = {
            let db = db.clone();
            tokio::spawn(async move { crate::directory::companies_at(&db, org, project, segment, &["race.example".to_string()]).await })
        };
        let mut waiting = 0;
        for _ in 0..250 {
            waiting = count(&db, "select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'").await;
            if waiting == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(waiting, 1, "the request waits on the other's insert, so the test races them");
        other.commit().await.unwrap();
        assert_eq!(ask.await.unwrap().unwrap().len(), 1, "the company the other request added");
    }
}
