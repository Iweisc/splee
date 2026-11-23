use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::{check_project, strict};
use crate::ab_test::{self, AbTest};
use crate::auth::ApiKeyAuth;
use crate::campaign_imports::{self, RowOutcome, SubmittedLead};
use crate::error::{AppError, AppResult};
use crate::models::{Segment, SegmentStep, Ts};
use crate::routes::emails;
use crate::routes::my_copy::{self, CampaignCopy, Preview, SourceBody};
use crate::routes::outreach;
use crate::routes::segments::{self, ArchivedQuery, SegmentBody, SegmentWithStats};
use crate::routes::settings;
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::routes::util_scope;
use crate::send_state::SendState;
use crate::state::AppState;

/// A campaign and its funnel, without the targeting the agent works from.
#[derive(Serialize)]
pub(in crate::routes) struct Campaign {
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
pub(in crate::routes) async fn list_campaigns(
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
pub(in crate::routes) struct CampaignBody {
    #[serde(default)]
    prompt: String,
    language: Option<String>,
}

/// Like describing a campaign in the app: the agent names it and fills in its targeting from the prompt.
pub(in crate::routes) async fn create_campaign(
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
pub(in crate::routes) struct ImportBody {
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
pub(in crate::routes) async fn import_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<ImportBody>,
) -> AppResult<Json<Value>> {
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
            "followup_instructions isn't taken here, since a campaign's follow-ups are steps, each with its own instructions. Leave it out, \
             then give the campaign its own follow_ups with PATCH /v1/campaigns/{id}, or it follows the project's"
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

pub(in crate::routes) async fn campaign_import(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
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

pub(in crate::routes) async fn campaign_import_rows(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<RowOutcome>>> {
    let task = campaign_imports::find(&state.db, auth.org_id, id).await?.ok_or(AppError::NotFound)?;
    let (items, total) = campaign_imports::rows(&state.db, task.id, page.limit(), page.offset()).await?;
    Ok(Json(Paginated { items, total }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(in crate::routes) struct CampaignUpdate {
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
    /// Its own follow-ups, or null to follow the project's again.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    follow_ups: Option<Option<Value>>,
    /// Its own send days and hours, or null to send in the project's again.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    send_hours: Option<Option<segments::SendHoursBody>>,
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
pub(in crate::routes) async fn update_campaign(
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
    segments::check_follow_ups(&state.db, segment.id, &body.follow_ups).await?;
    let send_hours = body.send_hours.map(|h| h.map(segments::SendHoursBody::checked).transpose()).transpose()?;
    let (days, start, end) = segments::SendHoursBody::columns(send_hours.as_ref().and_then(Option::as_ref));
    sqlx::query(
        "update segments set name = coalesce($2, name), description = coalesce($3, description), targeting = targeting || coalesce($4, '{}'),
           pain_points = coalesce($5, pain_points), angle = coalesce($6, angle), language = coalesce($7, language),
           track_clicks = coalesce($8, track_clicks), track_opens = coalesce($9, track_opens), lead_pages = coalesce($10, lead_pages),
           daily_limit = case when $11 then $12 else daily_limit end, follow_ups = case when $13 then $14 else follow_ups end,
           send_days = case when $15 then $16 else send_days end, send_start_hour = case when $15 then $17 else send_start_hour end,
           send_end_hour = case when $15 then $18 else send_end_hour end
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
    .bind(body.follow_ups.is_some())
    .bind(body.follow_ups.flatten())
    .bind(send_hours.is_some())
    .bind(days)
    .bind(start)
    .bind(end)
    .execute(&state.db)
    .await?;
    get_campaign(State(state), auth, Path(id)).await
}

/// A campaign as its page shows it, with the project's schedule and the follow-ups it sends.
#[derive(Serialize)]
pub(in crate::routes) struct CampaignDetail {
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
    /// The follow-ups after the first email, each an email or a call some days later: its own, else the project's.
    follow_ups: Value,
    /// Whether follow_ups are the campaign's own rather than the project's.
    own_follow_ups: bool,
    /// The tool an imported campaign came from, or null for one made in Splee.
    imported_from: Option<String>,
    /// An imported campaign's emails. Empty for one made in Splee.
    sequence: Vec<SegmentStep>,
    /// What can't be changed on this campaign, and why.
    read_only: Vec<ReadOnly>,
}

/// When the campaign sends: the project's time zone and daily limit, and its own send days and hours or else the project's.
#[derive(Serialize)]
struct Schedule {
    /// False while the project's sending is paused, which holds every campaign in it.
    sending: bool,
    timezone: String,
    /// 1 (Monday) to 7 (Sunday).
    send_days: Vec<i32>,
    send_start_hour: i32,
    send_end_hour: i32,
    /// Whether the days and hours are the campaign's own rather than the project's.
    own_send_hours: bool,
    daily_email_limit: i32,
}

#[derive(Serialize)]
struct ReadOnly {
    field: &'static str,
    reason: String,
}

pub(in crate::routes) async fn get_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<CampaignDetail>> {
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
        ReadOnly {
            field: "schedule",
            reason: "The time zone and daily limit are the project's, so change them with PATCH /v1/projects/{id}. Give the campaign its own \
                     days and hours with send_hours"
                .into(),
        },
    ];
    if let Some(tool) = &imported.imported_from {
        read_only.push(ReadOnly { field: "sequence", reason: format!("It came from {tool}, and Splee sends these emails as written") });
    }
    if !imported.steps.is_empty() {
        read_only.push(ReadOnly { field: "follow_ups", reason: "It sends the steps it came with instead".into() });
    }
    let hours = segment.send_hours(&settings);
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
            send_days: hours.days,
            send_start_hour: hours.start_hour,
            send_end_hour: hours.end_hour,
            own_send_hours: segment.send_days.is_some(),
            timezone: settings.timezone,
            daily_email_limit: settings.daily_email_limit,
        },
        own_follow_ups: segment.follow_ups.is_some(),
        follow_ups: segment.follow_ups.unwrap_or(settings.follow_ups),
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
pub(in crate::routes) async fn campaign_send_state(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<SendState>> {
    Ok(Json(crate::routes::segments::send_state_of(&state.db, auth.org_id, id).await?))
}

fn accepted() -> Json<Value> {
    Json(json!({ "accepted": true }))
}

/// The campaign's Start button: it sends, and its drafts are approved. The app only offers Start on a campaign that isn't
/// sending, so on one that is, nothing changes and a scaling campaign stays scaling.
pub(in crate::routes) async fn start_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if !segments::running(&segment) {
        segments::launch(&state.db, &auth, &segment).await?;
    }
    Ok(accepted())
}

/// The campaign's Pause button: its unsent emails go back to drafts. Replies keep coming in. A campaign that isn't sending
/// is left as it is, like the app, which only offers Pause on a sending one.
pub(in crate::routes) async fn stop_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    if segments::running(&segment) {
        segments::hold(&state.db, &auth, &segment).await?;
    }
    Ok(accepted())
}

/// Pauses the campaign like stop and hides it from list_campaigns. Its leads, emails and stats stay, and replies still come in.
pub(in crate::routes) async fn archive_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Campaign>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    segments::archive(&state.db, &auth, &segment).await?;
    Ok(Json(Campaign::from(segments::one_with_stats(&state.db, &segment).await?)))
}

/// Lists the campaign again, paused. It sends once it's started.
pub(in crate::routes) async fn unarchive_campaign(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Campaign>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    segments::unarchive(&state.db, &auth, &segment).await?;
    Ok(Json(Campaign::from(segments::one_with_stats(&state.db, &segment).await?)))
}

/// Splee has no review state for a whole campaign. This is the Approve button on each of its first emails and follow-ups
/// that wait as drafts, as POST /v1/emails/{id}/approve does one at a time. A paused campaign holds them until it starts.
pub(in crate::routes) async fn approve_campaign(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
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
pub(in crate::routes) async fn campaign_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(my_copy::campaign(&state.db, auth.org_id, id).await?))
}

pub(in crate::routes) async fn update_campaign_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<CampaignCopy>> {
    Ok(Json(my_copy::update_campaign(&state.db, auth.org_id, id, strict(body)?).await?))
}

pub(in crate::routes) async fn set_my_copy_source(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<CampaignCopy>> {
    let body: SourceBody = strict(body)?;
    Ok(Json(my_copy::set_source(&state.db, auth.org_id, id, body.source).await?))
}

pub(in crate::routes) async fn preview_my_copy(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Preview>> {
    Ok(Json(my_copy::campaign_preview(&state.db, auth.org_id, id, Default::default()).await?))
}
