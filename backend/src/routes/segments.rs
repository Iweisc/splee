use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgConnection, PgPool};
use uuid::Uuid;

use super::{util_input, util_scope};
use crate::agent::{self, kind};
use crate::audit::{self, Target, Who};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Segment;
use crate::send_state::{self, SendState};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/segments", get(list).post(create))
        .route("/segments/{id}", get(get_one).patch(update).delete(remove))
        .route("/segments/{id}/find-companies", post(find_companies))
        .route("/segments/{id}/find-people", post(find_people))
        .route("/segments/{id}/write-emails", post(write_emails))
        .route("/segments/{id}/start", post(start))
        .route("/segments/{id}/pause", post(pause))
        .route("/segments/{id}/archive", post(archive_route))
        .route("/segments/{id}/unarchive", post(unarchive_route))
        .route("/segments/{id}/send-state", get(get_send_state))
}

const ICONS: &[&str] = &[
    "building", "store", "hotel", "heart", "palette", "home", "scissors", "briefcase", "graduation-cap", "stethoscope",
    "factory", "truck", "utensils", "code", "shopping-cart", "users", "landmark", "plane", "dumbbell", "camera",
];
const STATUSES: &[&str] = &["draft", "scaling", "working", "paused"];

#[derive(Serialize, FromRow)]
pub struct SegmentWithStats {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub segment: Segment,
    pub companies: i64,
    pub leads: i64,
    pub sent: i64,
    pub contacted: i64,
    pub replies: i64,
    /// Leads whose address bounced. A high share points at a bad lead list.
    pub bounced: i64,
    pub hot: i64,
    pub spend_micros: i64,
    #[sqlx(skip)]
    pub cost_per_hot_micros: Option<i64>,
}

/// Segments of a project with their funnel numbers. Pass `segment_id` to get just one, archived or not.
/// Agent runs that research the whole project, like explore, have no segment, so their cost only counts in the project's stats.
pub async fn with_stats(db: &PgPool, project_id: Uuid, segment_id: Option<Uuid>, archived: Archived) -> AppResult<Vec<SegmentWithStats>> {
    let mut rows = sqlx::query_as::<_, SegmentWithStats>(
        "select s.*,
           (select count(*) from companies c where c.segment_id = s.id) as companies,
           (select count(*) from leads l where l.segment_id = s.id) as leads,
           (select count(*) from email_messages e
             where e.segment_id = s.id and e.direction = 'outbound' and e.status = 'sent') as sent,
           (select count(*) from leads l where l.segment_id = s.id and l.last_contacted_at is not null) as contacted,
           (select count(*) from leads l where l.segment_id = s.id
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound'
                         and e.classification is distinct from 'bounce')) as replies,
           (select count(*) from leads l where l.segment_id = s.id and l.status = 'bounced') as bounced,
           (select count(*) from leads l where l.segment_id = s.id and l.status in ('hot', 'meeting_booked')) as hot,
           (select (coalesce(sum(r.cost_usd), 0) * 1000000)::bigint from agent_runs r
             where r.project_id = s.project_id and r.input->>'segment_id' = s.id::text)
           + (select coalesce(sum(e.cost_micros), 0)::bigint from email_messages e
             where e.segment_id = s.id and e.direction = 'outbound' and e.status = 'sent') as spend_micros
         from segments s
         where s.project_id = $1 and ($2::uuid is null or s.id = $2) and ($3 or $2::uuid is not null or s.archived_at is null)
         order by s.position, s.created_at",
    )
    .bind(project_id)
    .bind(segment_id)
    .bind(archived == Archived::Included)
    .fetch_all(db)
    .await?;
    for row in &mut rows {
        row.cost_per_hot_micros = (row.hot > 0).then(|| row.spend_micros / row.hot);
    }
    Ok(rows)
}

/// Lists that pick a campaign to add leads to leave archived ones out. Stats and history keep them.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Archived {
    Hidden,
    Included,
}

/// `?archived=true` on a campaign list adds the archived campaigns.
#[derive(Deserialize)]
pub(super) struct ArchivedQuery {
    #[serde(default)]
    archived: bool,
}

impl From<ArchivedQuery> for Archived {
    fn from(q: ArchivedQuery) -> Self {
        if q.archived { Archived::Included } else { Archived::Hidden }
    }
}

pub(super) async fn one_with_stats(db: &PgPool, segment: &Segment) -> AppResult<SegmentWithStats> {
    with_stats(db, segment.project_id, Some(segment.id), Archived::Included).await?.pop().ok_or(AppError::NotFound)
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ArchivedQuery>,
) -> AppResult<Json<Vec<SegmentWithStats>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(with_stats(&state.db, id, None, q.into()).await?))
}

#[derive(Deserialize, Default)]
pub(super) struct SegmentBody {
    prompt: Option<String>,
    name: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    est_size: Option<i64>,
    fit_score: Option<i32>,
    status: Option<String>,
    targeting: Option<Value>,
    pain_points: Option<String>,
    angle: Option<String>,
    position: Option<i32>,
    track_clicks: Option<bool>,
    track_opens: Option<bool>,
    lead_pages: Option<bool>,
    /// "lead" (the default) for each lead's own language, or a code from language::LANGUAGES.
    language: Option<String>,
    /// The campaign's own daily limit, where null takes it off and leaves out keeps it.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    daily_limit: Option<Option<i32>>,
}

/// A campaign's own daily limit, or a 400.
pub(super) fn check_daily_limit(limit: Option<Option<i32>>) -> AppResult<()> {
    match limit.flatten() {
        Some(n) if !(1..=5000).contains(&n) => Err(AppError::BadRequest("daily_limit is 1 to 5,000 emails, or null for none".into())),
        _ => Ok(()),
    }
}

/// A campaign's language setting, or a 400 that lists the ones that work.
pub(super) fn check_language(language: &str) -> AppResult<()> {
    if language == crate::language::EACH_LEAD || crate::language::find(language).is_some() {
        return Ok(());
    }
    let codes: Vec<&str> = crate::language::LANGUAGES.iter().map(|l| l.code).collect();
    Err(AppError::BadRequest(format!("language must be \"lead\" for each lead's own, or one of {}", codes.join(", "))))
}

impl SegmentBody {
    pub(super) fn from_prompt(prompt: String, language: Option<String>) -> Self {
        Self { prompt: Some(prompt), language, ..Default::default() }
    }

    /// A campaign for a lead list the caller brings (POST /v1/campaigns/import), so no agent fills in its targeting.
    pub(super) fn imported(name: String, angle: String, language: Option<String>) -> Self {
        Self { name: Some(name), angle: Some(angle), language, ..Default::default() }
    }

    fn validate(&self) -> AppResult<()> {
        if let Some(icon) = &self.icon {
            util_input::one_of("icon", icon, ICONS)?;
        }
        if let Some(status) = &self.status {
            util_input::one_of("status", status, STATUSES)?;
        }
        if self.targeting.as_ref().is_some_and(|t| !t.is_object()) {
            return Err(AppError::BadRequest("targeting must be an object".into()));
        }
        if self.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(AppError::BadRequest("Name can't be empty".into()));
        }
        if let Some(language) = &self.language {
            check_language(language)?;
        }
        check_daily_limit(self.daily_limit)
    }
}

async fn create(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SegmentBody>,
) -> AppResult<Json<Segment>> {
    auth.project(&state.db, id).await?;
    Ok(Json(insert(&mut *state.db.acquire().await?, auth.org_id, id, body).await?.0))
}

/// Creates a campaign in a project the caller already checked. With a prompt, it starts as "New campaign" and a
/// segment_from_prompt run fills it in, whose id comes back too.
pub(super) async fn insert(db: &mut PgConnection, org_id: Uuid, id: Uuid, body: SegmentBody) -> AppResult<(Segment, Option<Uuid>)> {
    body.validate()?;
    let prompt = body.prompt.as_deref().map(str::trim).filter(|p| !p.is_empty());
    let name = match (prompt, body.name.as_deref()) {
        (Some(_), _) => "New campaign",
        (None, Some(name)) => name.trim(),
        (None, None) => return Err(AppError::BadRequest("Give the campaign a name or describe it".into())),
    };
    let segment = sqlx::query_as::<_, Segment>(
        "insert into segments (project_id, name, description, icon, est_size, fit_score, targeting, pain_points, angle, position, language)
         values ($1, $2, coalesce($3, ''), coalesce($4, 'building'), coalesce($5, 0), coalesce($6, 0), coalesce($7, '{}'::jsonb),
                 coalesce($8, ''), coalesce($9, ''),
                 coalesce($10, (select coalesce(max(position) + 1, 0) from segments where project_id = $1)), coalesce($11, 'lead'))
         returning *",
    )
    .bind(id)
    .bind(name)
    .bind(prompt.or(body.description.as_deref()))
    .bind(body.icon)
    .bind(body.est_size)
    .bind(body.fit_score)
    .bind(body.targeting)
    .bind(body.pain_points)
    .bind(body.angle)
    .bind(body.position)
    .bind(body.language)
    .fetch_one(&mut *db)
    .await?;
    let run_id = match prompt {
        Some(prompt) => Some(
            agent::enqueue(db, kind::SEGMENT_FROM_PROMPT, Some(id), Some(org_id), json!({ "segment_id": segment.id, "prompt": prompt }))
                .await?,
        ),
        None => None,
    };
    Ok((segment, run_id))
}

async fn get_one(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

/// Whether the campaign's emails go out now and, if not, the one thing holding them, for the campaign page and GET
/// /v1/campaigns/{id}/send-state. See send_state.rs.
pub(super) async fn send_state_of(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<SendState> {
    let segment: Segment = util_scope::load(db, "segments", id, org_id).await?;
    let settings = super::settings::load_or_create(db, segment.project_id).await?;
    send_state::load(db, org_id, &segment, &settings, chrono::Utc::now()).await
}

async fn get_send_state(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SendState>> {
    Ok(Json(send_state_of(&state.db, auth.org_id, id).await?))
}

async fn update(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<SegmentBody>,
) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    body.validate()?;
    if body.status.is_some() {
        ensure_not_archived(&segment)?;
    }
    let segment = sqlx::query_as::<_, Segment>(
        "update segments set
           name = coalesce($2, name),
           description = coalesce($3, description),
           icon = coalesce($4, icon),
           est_size = coalesce($5, est_size),
           fit_score = coalesce($6, fit_score),
           status = coalesce($7, status),
           targeting = coalesce($8, targeting),
           pain_points = coalesce($9, pain_points),
           angle = coalesce($10, angle),
           position = coalesce($11, position),
           track_clicks = coalesce($12, track_clicks),
           track_opens = coalesce($13, track_opens),
           language = coalesce($14, language),
           lead_pages = coalesce($15, lead_pages),
           daily_limit = case when $16 then $17 else daily_limit end
         where id = $1 returning *",
    )
    .bind(segment.id)
    .bind(body.name.as_deref().map(str::trim))
    .bind(body.description)
    .bind(body.icon)
    .bind(body.est_size)
    .bind(body.fit_score)
    .bind(body.status)
    .bind(body.targeting)
    .bind(body.pain_points)
    .bind(body.angle)
    .bind(body.position)
    .bind(body.track_clicks)
    .bind(body.track_opens)
    .bind(body.language)
    .bind(body.lead_pages)
    .bind(body.daily_limit.is_some())
    .bind(body.daily_limit.flatten())
    .fetch_one(&state.db)
    .await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    sqlx::query("delete from segments where id = $1").bind(segment.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn enqueue_for(db: &PgPool, org_id: Uuid, segment: &Segment, run_kind: &str, input: Value) -> AppResult<Uuid> {
    Ok(agent::enqueue(db, run_kind, Some(segment.project_id), Some(org_id), input).await?)
}

/// Refuses a run for the whole campaign when every lead already has a first email. It would finish instantly with
/// nothing written, and the caller would never hear why.
async fn ensure_unwritten_leads(db: &PgPool, segment: &Segment) -> AppResult<()> {
    if agent::leads_without_initial(db, segment.project_id, Some(segment.id)).await?.is_empty() {
        return Err(AppError::Conflict("Every lead in this campaign already has an email. Find or add more people first.".into()));
    }
    Ok(())
}

/// Keeps only ids that belong to the segment's project.
async fn owned_ids(db: &PgPool, table: &'static str, ids: &[Uuid], project_id: Uuid) -> AppResult<Vec<Uuid>> {
    let sql = format!("select id from {table} where id = any($1) and project_id = $2");
    Ok(sqlx::query_scalar(&sql).bind(ids).bind(project_id).fetch_all(db).await?)
}

#[derive(Deserialize, Default)]
struct FindCompaniesBody {
    count: Option<i64>,
}

async fn find_companies(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<FindCompaniesBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let count = body.unwrap_or_default().count.unwrap_or(25).clamp(1, 100);
    let run_id = enqueue_for(&state.db, auth.org_id, &segment, kind::FIND_COMPANIES, json!({ "segment_id": id, "count": count })).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize, Default)]
pub(super) struct FindPeopleBody {
    pub company_ids: Option<Vec<Uuid>>,
    /// Companies by domain in place of company_ids, added to the campaign when the project hasn't got them.
    pub domains: Option<Vec<String>>,
    /// Job titles to look for in place of the campaign's roles.
    pub titles: Option<Vec<String>>,
    pub per_company: Option<usize>,
}

/// Each company is a few searches by the agent, so a run takes a list's worth, not a database's.
const MAX_DOMAINS: usize = 100;
const MAX_TITLES: usize = 20;
/// What the run allows. It reads people from a company's pages and LinkedIn's first page, where more are rarely right.
const MAX_PER_COMPANY: usize = 5;

async fn find_people(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<FindPeopleBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let run_id = start_find_people(&state.db, auth.org_id, &segment, body.unwrap_or_default().0).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

pub(super) async fn start_find_people(db: &PgPool, org_id: Uuid, segment: &Segment, body: FindPeopleBody) -> AppResult<Uuid> {
    let bad = |message: String| Err(AppError::BadRequest(message));
    let mut input = json!({ "segment_id": segment.id });
    let titles: Vec<&str> = body.titles.iter().flatten().map(|t| t.trim()).filter(|t| !t.is_empty()).collect();
    if titles.len() > MAX_TITLES || titles.iter().any(|t| t.chars().count() > 100) {
        return bad(format!("Send up to {MAX_TITLES} titles of up to 100 characters"));
    }
    if !titles.is_empty() {
        input["roles"] = json!(titles.join(", "));
    }
    if let Some(n) = body.per_company {
        if !(1..=MAX_PER_COMPANY).contains(&n) {
            return bad(format!("per_company is 1 to {MAX_PER_COMPANY}"));
        }
        input["max_per_company"] = json!(n);
    }
    match (body.company_ids, body.domains) {
        (Some(_), Some(_)) => return bad("Send company_ids or domains, not both".into()),
        (Some(ids), None) => input["company_ids"] = json!(owned_ids(db, "companies", &ids, segment.project_id).await?),
        (None, Some(domains)) => {
            let mut wanted: Vec<String> = Vec::new();
            for domain in &domains {
                let Some(domain) = util_input::domain(domain) else { return bad(format!("{domain} isn't a domain")) };
                if !wanted.contains(&domain) {
                    wanted.push(domain);
                }
            }
            if wanted.is_empty() || wanted.len() > MAX_DOMAINS {
                return bad(format!("Send 1 to {MAX_DOMAINS} domains"));
            }
            let ids = crate::directory::companies_at(db, org_id, segment.project_id, segment.id, &wanted).await?;
            if ids.is_empty() {
                return bad("Every one of these companies is excluded or on your do-not-contact list".into());
            }
            input["company_ids"] = json!(ids);
        }
        (None, None) => {}
    }
    enqueue_for(db, org_id, segment, kind::FIND_PEOPLE, input).await
}

#[derive(Deserialize, Default)]
pub(super) struct WriteEmailsBody {
    pub lead_ids: Option<Vec<Uuid>>,
    pub instructions: Option<String>,
}

async fn write_emails(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    body: Option<Json<WriteEmailsBody>>,
) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let run_id = start_write_emails(&state.db, auth.org_id, &segment, body.unwrap_or_default().0).await?;
    Ok(Json(json!({ "run_id": run_id })))
}

/// Without lead_ids, writes a first email for every lead in the campaign that has none.
pub(super) async fn start_write_emails(db: &PgPool, org_id: Uuid, segment: &Segment, body: WriteEmailsBody) -> AppResult<Uuid> {
    let mut input = json!({ "segment_id": segment.id });
    if let Some(ids) = body.lead_ids {
        input["lead_ids"] = json!(owned_ids(db, "leads", &ids, segment.project_id).await?);
    } else {
        ensure_unwritten_leads(db, segment).await?;
    }
    if let Some(instructions) = body.instructions.filter(|i| !i.trim().is_empty()) {
        input["instructions"] = json!(instructions);
    }
    enqueue_for(db, org_id, segment, kind::WRITE_EMAILS, input).await
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    launch(&state.db, &auth, &segment).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Whether the campaign sends. The app shows Pause for these and Start for the rest.
pub(super) fn running(segment: &Segment) -> bool {
    matches!(segment.status.as_str(), "working" | "scaling")
}

/// The campaign's Start button: it sends, and its drafts are approved. POST /v1/campaigns/{id}/start calls it too.
pub(super) async fn launch(db: &PgPool, who: impl Into<Who>, segment: &Segment) -> AppResult<()> {
    let mut tx = db.begin().await?;
    // Checked in the update, so an archive that lands after the campaign was loaded still wins.
    let started =
        sqlx::query("update segments set status = 'working' where id = $1 and archived_at is null").bind(segment.id).execute(&mut *tx).await?;
    if started.rows_affected() == 0 {
        return Err(archived_error());
    }
    // Pausing turned queued follow-ups back into drafts, so starting queues them again for the day they were due.
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "update email_messages set status = 'approved', approved_via = 'person',
           scheduled_for = case when kind = 'follow_up' then coalesce(scheduled_for, now()) else now() end
         where segment_id = $1 and kind in ('initial', 'follow_up') and status = 'draft' returning lead_id",
    )
    .bind(segment.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("update leads set status = 'queued' where id = any($1) and status = 'new'")
        .bind(&lead_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update text_messages set status = 'approved' where segment_id = $1 and kind = 'sequence' and status = 'draft'")
        .bind(segment.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let details = json!({ "project_id": segment.project_id, "emails_queued": lead_ids.len() });
    audit::record(db, who, "campaign.launched", Target::new("campaign", segment.id, &segment.name), details).await;
    Ok(())
}

async fn pause(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    hold(&state.db, &auth, &segment).await?;
    Ok(Json(json!({ "ok": true })))
}

/// The campaign's Pause button: its unsent emails go back to drafts until it starts again. POST /v1/campaigns/{id}/stop calls it too.
pub(super) async fn hold(db: &PgPool, who: impl Into<Who>, segment: &Segment) -> AppResult<()> {
    let mut tx = db.begin().await?;
    sqlx::query("update segments set status = 'paused' where id = $1").bind(segment.id).execute(&mut *tx).await?;
    // Manual and auto replies are answers to real people, so pausing a campaign leaves them queued.
    let lead_ids: Vec<Uuid> = sqlx::query_scalar(
        "update email_messages set status = 'draft', approved_via = null
         where segment_id = $1 and kind in ('initial', 'follow_up') and status in ('approved', 'scheduled')
         returning lead_id",
    )
    .bind(segment.id)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query("update leads set status = 'new' where id = any($1) and status = 'queued'")
        .bind(&lead_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("update text_messages set status = 'draft' where segment_id = $1 and kind = 'sequence' and status = 'approved'")
        .bind(segment.id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    let details = json!({ "project_id": segment.project_id, "emails_held": lead_ids.len() });
    audit::record(db, who, "campaign.paused", Target::new("campaign", segment.id, &segment.name), details).await;
    Ok(())
}

fn archived_error() -> AppError {
    AppError::Conflict("This campaign is archived. Unarchive it first.".into())
}

fn ensure_not_archived(segment: &Segment) -> AppResult<()> {
    if segment.archived_at.is_some() { Err(archived_error()) } else { Ok(()) }
}

async fn archive_route(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    archive(&state.db, &auth, &segment).await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

async fn unarchive_route(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SegmentWithStats>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    unarchive(&state.db, &auth, &segment).await?;
    Ok(Json(one_with_stats(&state.db, &segment).await?))
}

/// Pauses the campaign and takes it out of the lists that add leads or send. Its leads, emails and stats stay, and replies
/// to its emails still reach the inbox. Archiving it again changes nothing. POST /v1/campaigns/{id}/archive calls it too.
pub(super) async fn archive(db: &PgPool, who: impl Into<Who>, segment: &Segment) -> AppResult<()> {
    let who = who.into();
    // Archived before it's held, so a Start in between is refused rather than sending again. The status is the row's as it
    // is archived, not as it was loaded, since a Start may have landed in between.
    let status: Option<String> = sqlx::query_scalar("update segments set archived_at = now() where id = $1 and archived_at is null returning status")
        .bind(segment.id)
        .fetch_optional(db)
        .await?;
    let Some(status) = status else { return Ok(()) };
    // A draft campaign can hold emails approved one at a time, which the sender would still send, so it's held too.
    if status != "paused" {
        hold(db, who, segment).await?;
    }
    let target = Target::new("campaign", segment.id, &segment.name);
    audit::record(db, who, "campaign.archived", target, json!({ "project_id": segment.project_id })).await;
    Ok(())
}

/// Brings the campaign back to the lists, paused. It only sends once someone starts it.
pub(super) async fn unarchive(db: &PgPool, who: impl Into<Who>, segment: &Segment) -> AppResult<()> {
    let unarchived =
        sqlx::query("update segments set archived_at = null where id = $1 and archived_at is not null").bind(segment.id).execute(db).await?;
    if unarchived.rows_affected() > 0 {
        let target = Target::new("campaign", segment.id, &segment.name);
        audit::record(db, who, "campaign.unarchived", target, json!({ "project_id": segment.project_id })).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::{Path, State};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::{Archived, get_send_state, with_stats};
    use crate::auth::Auth;
    use crate::error::AppError;
    use crate::routes::stats::tests::seed_costs;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaign_spend_counts_its_own_runs_and_emails(db: PgPool) {
        let seeded = seed_costs(&db).await;
        let rows = with_stats(&db, seeded.project, None, Archived::Hidden).await.unwrap();
        let row = |id| rows.iter().find(|r| r.segment.id == id).unwrap();

        let cheap = row(seeded.cheap);
        assert_eq!((cheap.sent, cheap.hot, cheap.spend_micros), (2, 1, 251_000));
        assert_eq!(cheap.cost_per_hot_micros, Some(251_000));

        let pricey = row(seeded.pricey);
        assert_eq!((pricey.sent, pricey.hot, pricey.spend_micros), (1, 0, 500_700));
        assert_eq!(pricey.cost_per_hot_micros, None);
        assert_eq!((cheap.bounced, pricey.bounced), (0, 0));

        sqlx::query("update leads set status = 'bounced' where segment_id = $1").bind(seeded.pricey).execute(&db).await.unwrap();
        let rows = with_stats(&db, seeded.project, Some(seeded.pricey), Archived::Hidden).await.unwrap();
        assert_eq!(rows[0].bounced, 1);
    }

    // The states themselves are tested through GET /v1/campaigns/{id}/send-state in api_v1.rs, which shares send_state_of.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_campaign_page_reads_its_send_state_only_in_its_organization(db: PgPool) {
        let org = |name: &str| sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ($1) returning id").bind(name.to_string()).fetch_one(&db);
        let (ours, theirs) = (org("Ours").await.unwrap(), org("Theirs").await.unwrap());
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(ours).fetch_one(&db).await.unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', 'working') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |org_id| Auth { user_id: Uuid::new_v4(), org_id, role: "member".into(), token_hash: String::new() };

        // A new project starts paused, which holds the campaign before anything else.
        let Json(read) = get_send_state(State(state.clone()), auth(ours), Path(segment)).await.unwrap();
        assert_eq!((read.campaign_id, read.state, read.sending_now), (segment, "project_paused", false));
        let theirs = get_send_state(State(state), auth(theirs), Path(segment)).await;
        assert!(matches!(theirs, Err(AppError::NotFound)), "another organization's campaign is a 404");
    }
}
