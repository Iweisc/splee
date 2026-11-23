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
use crate::models::{Segment, Ts};
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
        .route("/segments/{id}/duplicate", post(duplicate_route))
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
    /// The campaign's own follow-ups, where null goes back to the project's and leaves out keeps them.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    follow_ups: Option<Option<Value>>,
    /// The campaign's own send days and hours, where null goes back to the project's and leaves out keeps them.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    send_hours: Option<Option<SendHoursBody>>,
    /// When the campaign starts itself, where null cancels it and leaves out keeps it.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    start_at: Option<Option<Ts>>,
    /// When the campaign pauses itself, where null cancels it and leaves out keeps it.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    pause_at: Option<Option<Ts>>,
    /// The mailboxes its first emails go out from, where null goes back to the project's and leaves out keeps them.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    mailbox_ids: Option<Option<Vec<Uuid>>>,
}

/// A campaign's own send days, 1 (Monday) to 7, and hours, read in the project's time zone.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SendHoursBody {
    days: Vec<i32>,
    start_hour: i32,
    end_hour: i32,
}

impl SendHoursBody {
    /// The days, sorted without repeats, or a 400.
    pub(super) fn checked(mut self) -> AppResult<Self> {
        self.days.sort_unstable();
        self.days.dedup();
        if self.days.is_empty() {
            return Err(AppError::BadRequest("Pick at least one send day".into()));
        }
        super::settings::check_send_window(&self.days, self.start_hour, self.end_hour).map_err(|m| AppError::BadRequest(m.into()))?;
        Ok(self)
    }

    /// The segments columns it sets, all null for the project's.
    pub(super) fn columns(hours: Option<&Self>) -> (Option<&[i32]>, Option<i32>, Option<i32>) {
        (hours.map(|h| h.days.as_slice()), hours.map(|h| h.start_hour), hours.map(|h| h.end_hour))
    }
}

/// A campaign's own follow-ups, or a 400. One brought over from another tool sends the steps it came with, so it can't have any.
pub(super) async fn check_follow_ups(db: &PgPool, segment: Uuid, follow_ups: &Option<Option<Value>>) -> AppResult<()> {
    let Some(Some(steps)) = follow_ups else { return Ok(()) };
    super::settings::check_follow_ups(steps).map_err(|m| AppError::BadRequest(m.into()))?;
    let imported: bool = sqlx::query_scalar("select exists (select 1 from segment_steps where segment_id = $1)").bind(segment).fetch_one(db).await?;
    if imported {
        return Err(AppError::BadRequest("This campaign came from another tool and sends the steps it came with".into()));
    }
    Ok(())
}

/// A campaign's own mailboxes, checked like the project's, or a 400. Null (the inner None) goes back to the project's.
pub(super) async fn check_mailboxes(db: &PgPool, org_id: Uuid, ids: Option<Option<Vec<Uuid>>>) -> AppResult<Option<Option<Vec<Uuid>>>> {
    match ids {
        Some(Some(ids)) if ids.is_empty() => Err(AppError::BadRequest("Pick at least one mailbox".into())),
        Some(Some(ids)) => Ok(Some(Some(super::settings::check_mailboxes(db, org_id, ids).await?))),
        other => Ok(other),
    }
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

async fn duplicate_route(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Segment>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(duplicate(&state.db, &segment).await?))
}

/// A new draft campaign, last in the project, named "<name> (copy)", with the campaign's brief, targeting and settings, its
/// own first email, its A/B variants that aren't paused, and an imported campaign's steps. No leads, emails or results come
/// with it, and it starts paused like any new campaign, so it sends nothing until someone starts it. An archived campaign
/// can be copied too. POST /v1/campaigns/{id}/duplicate calls it.
pub(super) async fn duplicate(db: &PgPool, segment: &Segment) -> AppResult<Segment> {
    let mut tx = db.begin().await?;
    let copy = sqlx::query_as::<_, Segment>(
        "insert into segments (project_id, name, description, icon, est_size, fit_score, targeting, pain_points, angle, position, language,
                               track_clicks, track_opens, lead_pages, daily_limit, follow_ups, send_days, send_start_hour, send_end_hour,
                               my_copy_source, my_copy_mode, my_copy_subject, my_copy_body, variant_pick_after, imported_from, mailbox_ids)
         select project_id, name || ' (copy)', description, icon, est_size, fit_score, targeting, pain_points, angle,
                (select max(position) + 1 from segments where project_id = s.project_id), language,
                track_clicks, track_opens, lead_pages, daily_limit, follow_ups, send_days, send_start_hour, send_end_hour,
                my_copy_source, my_copy_mode, my_copy_subject, my_copy_body, variant_pick_after, imported_from, mailbox_ids
         from segments s where id = $1 returning *",
    )
    .bind(segment.id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query(
        "insert into segment_variants (segment_id, name, instructions, position)
         select $2, name, instructions, position from segment_variants where segment_id = $1 and paused_at is null",
    )
    .bind(segment.id)
    .bind(copy.id)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "insert into segment_steps (segment_id, position, delay_days, subject, body)
         select $2, position, delay_days, subject, body from segment_steps where segment_id = $1",
    )
    .bind(segment.id)
    .bind(copy.id)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(copy)
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
    check_follow_ups(&state.db, segment.id, &body.follow_ups).await?;
    let send_hours = body.send_hours.map(|h| h.map(SendHoursBody::checked).transpose()).transpose()?;
    let (days, start, end) = SendHoursBody::columns(send_hours.as_ref().and_then(Option::as_ref));
    if body.status.is_some() {
        ensure_not_archived(&segment)?;
    }
    let status = body.status.as_deref().unwrap_or(&segment.status);
    let (set, start_at, by_user, by_key) = start_columns(&segment, status, (&auth).into(), body.start_at)?;
    let starts = if set { start_at } else { segment.start_at };
    let (set_pause, pause_at, pause_user, pause_key) = pause_columns(&segment, status, starts, set, (&auth).into(), body.pause_at)?;
    let mailbox_ids = check_mailboxes(&state.db, auth.org_id, body.mailbox_ids).await?;
    let segment = sqlx::query_as::<_, Segment>(&format!(
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
           daily_limit = case when $16 then $17 else daily_limit end,
           follow_ups = case when $18 then $19 else follow_ups end,
           send_days = case when $20 then $21 else send_days end,
           send_start_hour = case when $20 then $22 else send_start_hour end,
           send_end_hour = case when $20 then $23 else send_end_hour end,
           {},
           mailbox_ids = case when $28 then $29 else mailbox_ids end,
           {}
         where id = $1 returning *",
        set_start(24, "coalesce($7, status)"),
        set_pause(30),
    ))
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
    .bind(body.follow_ups.is_some())
    .bind(body.follow_ups.flatten())
    .bind(send_hours.is_some())
    .bind(days)
    .bind(start)
    .bind(end)
    .bind(set)
    .bind(start_at)
    .bind(by_user)
    .bind(by_key)
    .bind(mailbox_ids.is_some())
    .bind(mailbox_ids.flatten())
    .bind(set_pause)
    .bind(pause_at)
    .bind(pause_user)
    .bind(pause_key)
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

/// What a PATCH's start_at sets: whether it changes, then start_at and the member or API key asking, all None to cancel. Left
/// out (the outer None) changes nothing. A campaign the PATCH leaves sending (`status`) has nothing to start, and an archived
/// one is refused like its Start button. set_start writes them in the PATCH's update.
pub(super) fn start_columns(segment: &Segment, status: &str, who: Who, start_at: Option<Option<Ts>>) -> AppResult<ScheduleColumns> {
    let Some(start_at) = start_at else { return Ok((false, None, None, None)) };
    if let Some(at) = start_at {
        ensure_not_archived(segment)?;
        if matches!(status, "working" | "scaling") {
            return Err(AppError::BadRequest("This campaign is already sending".into()));
        }
        within_a_year(at, "start")?;
    }
    Ok((true, start_at, start_at.and(who.user_id()), start_at.and(who.key_id())))
}

/// What a PATCH's pause_at sets, like start_columns. Only a campaign the PATCH leaves sending (`status`), or one that starts
/// by itself at `starts`, has something to pause, and the pause comes after the start. A start moved past a pause already set
/// is refused too, so the pause never lands before the campaign sends. `start_changed` is whether the PATCH sets start_at.
/// set_pause writes them.
pub(super) fn pause_columns(
    segment: &Segment,
    status: &str,
    starts: Option<Ts>,
    start_changed: bool,
    who: Who,
    pause_at: Option<Option<Ts>>,
) -> AppResult<ScheduleColumns> {
    let sending = matches!(status, "working" | "scaling");
    let starts = starts.filter(|_| !sending);
    let Some(pause_at) = pause_at else {
        return match (starts, segment.pause_at) {
            (Some(start), Some(pause)) if pause <= start => Err(AppError::BadRequest("Pick a time to start before it pauses".into())),
            // Cancelling the start of a campaign that isn't sending takes its pause with it, so a later Start doesn't inherit
            // one the page no longer shows beside Start.
            (None, Some(_)) if start_changed && !sending => Ok((true, None, None, None)),
            _ => Ok((false, None, None, None)),
        };
    };
    if let Some(at) = pause_at {
        ensure_not_archived(segment)?;
        within_a_year(at, "pause")?;
        match starts {
            Some(start) if at <= start => return Err(AppError::BadRequest("Pick a time to pause after it starts".into())),
            None if !sending => return Err(AppError::BadRequest("Start this campaign, or schedule its start, first".into())),
            _ => {}
        }
    }
    Ok((true, pause_at, pause_at.and(who.user_id()), pause_at.and(who.key_id())))
}

fn within_a_year(at: Ts, what: &str) -> AppResult<()> {
    let now = chrono::Utc::now();
    if at <= now || at > now + chrono::Duration::days(366) {
        return Err(AppError::BadRequest(format!("Pick a time to {what} within the next year")));
    }
    Ok(())
}

/// Whether a schedule changes, then its time and the member or API key asking, all None to cancel.
pub(super) type ScheduleColumns = (bool, Option<Ts>, Option<Uuid>, Option<Uuid>);

/// SQL for an update that writes start_columns, bound in order from parameter `$n`. `status` is the status the update leaves:
/// a campaign left sending has nothing to start, like after Start. start_columns checked the status as loaded, and this also
/// covers a Start or a schedule that another request committed in between.
pub(super) fn set_start(n: usize, status: &str) -> String {
    let (to, user, key) = (n + 1, n + 2, n + 3);
    let sends = format!("{status} in ('working', 'scaling')");
    format!(
        "start_at = case when {sends} then null when ${n} then ${to} else start_at end,
         start_by_user = case when {sends} then null when ${n} then ${user} else start_by_user end,
         start_by_key = case when {sends} then null when ${n} then ${key} else start_by_key end"
    )
}

/// SQL for an update that writes pause_columns, bound in order from parameter `$n`.
pub(super) fn set_pause(n: usize) -> String {
    let (to, user, key) = (n + 1, n + 2, n + 3);
    format!(
        "pause_at = case when ${n} then ${to} else pause_at end,
         pause_by_user = case when ${n} then ${user} else pause_by_user end,
         pause_by_key = case when ${n} then ${key} else pause_by_key end"
    )
}

/// The campaign, its organization, and the time and member or API key of the schedule it had.
type Due = (Uuid, Uuid, Ts, Option<Uuid>, Option<Uuid>);

/// Claims the campaigns whose `{which}_at` (start or pause) has come by clearing it, so two servers can't both act on one.
/// A pause waits while a start is still due, so start_scheduled sees the window it belongs to.
async fn claim_due(db: &PgPool, which: &str) -> sqlx::Result<Vec<Due>> {
    let waits = if which == "pause" { "and start_at is null" } else { "" };
    sqlx::query_as(&format!(
        "with due as (select id, {which}_at as at, {which}_by_user as by_user, {which}_by_key as by_key from segments
                      where {which}_at <= now() {waits} for update skip locked)
         update segments s set {which}_at = null, {which}_by_user = null, {which}_by_key = null from due, projects p
         where s.id = due.id and p.id = s.project_id returning s.id, p.org_id, due.at, due.by_user, due.by_key"
    ))
    .fetch_all(db)
    .await
}

/// Puts back a claimed schedule whose action failed, for the next minute, unless it was set again meanwhile.
async fn unclaim(db: &PgPool, which: &str, (id, _, at, user, key): Due) -> sqlx::Result<()> {
    sqlx::query(&format!(
        "update segments set {which}_at = $2, {which}_by_user = $3, {which}_by_key = $4 where id = $1 and {which}_at is null"
    ))
    .bind(id)
    .bind(at)
    .bind(user)
    .bind(key)
    .execute(db)
    .await?;
    Ok(())
}

/// Starts each campaign whose start_at has come, as its Start button would, in the name of whoever set it. A campaign that's
/// sending or archived by then is left as it is.
pub async fn start_scheduled(db: &PgPool) -> anyhow::Result<()> {
    for due in claim_due(db, "start").await? {
        let (id, org_id, _, user, key) = due;
        // Deleted since it was claimed, or started or archived by hand.
        let segment: Option<Segment> = sqlx::query_as("select * from segments where id = $1").bind(id).fetch_optional(db).await?;
        let Some(segment) = segment.filter(|s| !running(s) && s.archived_at.is_none()) else { continue };
        // Its window passed while nothing ran, after downtime say, so it stays paused rather than send with no end.
        if segment.pause_at.is_some_and(|at| at <= chrono::Utc::now()) {
            sqlx::query(&format!("update segments set {CANCEL_PAUSE} where id = $1")).bind(id).execute(db).await?;
            continue;
        }
        if let Err(e) = launch(db, Who::stored(org_id, user, key), &segment).await {
            tracing::error!(error = ?e, segment_id = %id, "could not start a scheduled campaign");
            unclaim(db, "start", due).await?;
        }
    }
    Ok(())
}

/// Pauses each campaign whose pause_at has come, as its Pause button would, in the name of whoever set it. A campaign that
/// isn't sending by then, paused or archived by hand, is left as it is.
pub async fn pause_scheduled(db: &PgPool) -> anyhow::Result<()> {
    for due in claim_due(db, "pause").await? {
        let (id, org_id, _, user, key) = due;
        let segment: Option<Segment> = sqlx::query_as("select * from segments where id = $1").bind(id).fetch_optional(db).await?;
        let Some(segment) = segment.filter(|s| running(s) && s.archived_at.is_none()) else { continue };
        if let Err(e) = hold(db, Who::stored(org_id, user, key), &segment).await {
            tracing::error!(error = ?e, segment_id = %id, "could not pause a scheduled campaign");
            unclaim(db, "pause", due).await?;
        }
    }
    Ok(())
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
        sqlx::query(&format!("update segments set status = 'working', {CANCEL_START} where id = $1 and archived_at is null"))
            .bind(segment.id)
            .execute(&mut *tx)
            .await?;
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
    sqlx::query(&format!("update segments set status = 'paused', {CANCEL_PAUSE} where id = $1")).bind(segment.id).execute(&mut *tx).await?;
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

/// Starting or archiving a campaign takes back its scheduled start.
const CANCEL_START: &str = "start_at = null, start_by_user = null, start_by_key = null";
/// Pausing or archiving it takes back its scheduled pause. A start, by hand or scheduled, keeps it.
const CANCEL_PAUSE: &str = "pause_at = null, pause_by_user = null, pause_by_key = null";

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
    let status: Option<String> =
        sqlx::query_scalar(&format!("update segments set archived_at = now(), {CANCEL_START}, {CANCEL_PAUSE} where id = $1 and archived_at is null returning status"))
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

    use super::{Archived, get_send_state, update, with_stats};
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

    // The states themselves are tested through GET /v1/campaigns/{id}/send-state in api_v1/tests.rs, which shares send_state_of.
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

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_campaign_picks_its_own_mailboxes_from_its_organization_and_goes_back_to_the_projects(db: PgPool) {
        let org = |name: &str| sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ($1) returning id").bind(name.to_string()).fetch_one(&db);
        let (ours, theirs) = (org("Ours").await.unwrap(), org("Theirs").await.unwrap());
        let mailbox = |org_id: Uuid, email: &'static str| {
            sqlx::query_scalar::<_, Uuid>(
                "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.example.test', $2, 'x') returning id",
            )
            .bind(org_id)
            .bind(email)
            .fetch_one(&db)
        };
        let (a, b, foreign) =
            (mailbox(ours, "a@ours.example").await.unwrap(), mailbox(ours, "b@ours.example").await.unwrap(), mailbox(theirs, "c@theirs.example").await.unwrap());
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'P') returning id").bind(ours).fetch_one(&db).await.unwrap();
        let segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'C') returning id").bind(project).fetch_one(&db).await.unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let patch = |body: serde_json::Value| {
            let auth = Auth { user_id: Uuid::new_v4(), org_id: ours, role: "member".into(), token_hash: String::new() };
            update(State(state.clone()), auth, Path(segment), Json(serde_json::from_value(body).unwrap()))
        };
        let bad = |r: Result<_, AppError>| match r {
            Err(AppError::BadRequest(m)) => m,
            Err(e) => panic!("{e:?}"),
            Ok(_) => panic!("saved"),
        };

        let Json(saved) = patch(serde_json::json!({ "mailbox_ids": [b, a, b] })).await.unwrap();
        let mut both = vec![a, b];
        both.sort();
        assert_eq!(saved.segment.mailbox_ids, Some(both.clone()), "sorted without repeats");
        let Json(kept) = patch(serde_json::json!({ "name": "Renamed" })).await.unwrap();
        assert_eq!(kept.segment.mailbox_ids, Some(both), "left out keeps them");
        assert_eq!(bad(patch(serde_json::json!({ "mailbox_ids": [] })).await), "Pick at least one mailbox");
        assert_eq!(bad(patch(serde_json::json!({ "mailbox_ids": [a, foreign] })).await), "Pick mailboxes connected to this organization");
        let Json(back) = patch(serde_json::json!({ "mailbox_ids": null })).await.unwrap();
        assert_eq!(back.segment.mailbox_ids, None, "null goes back to the project's");
    }
}
