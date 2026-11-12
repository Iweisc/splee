use std::sync::LazyLock;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::suppressions;
use super::util_csv;
use super::util_input::{self, LEAD_STATUSES};
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::agent;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::hiring;
use crate::language;
use crate::models::{Call, Company, EmailMessage, Lead, TextMessage, Ts};
use crate::ownership::{self, OwnerFilter};
use crate::state::AppState;
use crate::waterfall;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/companies", get(list_companies))
        .route("/companies/{id}", patch(update_company))
        .route("/projects/{id}/leads", get(list_leads))
        .route("/projects/{id}/leads/export", get(export_leads))
        .route("/projects/{id}/leads/move", post(move_leads))
        .route("/projects/{id}/leads/delete", post(delete_leads))
        .route("/projects/{id}/leads/status", post(set_status))
        .route("/projects/{id}/leads/assign", post(assign_leads))
        .route("/projects/{id}/leads/find-emails", get(email_progress).post(find_emails))
        .route("/projects/{id}/meetings", get(upcoming_meetings))
        .route("/leads/{id}", get(get_lead).patch(update_lead).delete(remove_lead))
        .route("/leads/{id}/owner", put(set_owner))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub segment_id: Option<Uuid>,
    pub status: Option<String>,
    pub q: Option<String>,
    /// "missing" keeps only leads without an email.
    pub email: Option<String>,
    pub min_score: Option<i16>,
    /// "score" puts the highest scores first, "newest" (leads' default) the newest.
    pub sort: Option<String>,
    /// Leads only: mine, unassigned or a member's id. See ownership::OwnerFilter.
    pub owner: Option<String>,
    /// Leads only: a code from language::LANGUAGES, matched against each lead's effective_language.
    pub language: Option<String>,
}

impl ListQuery {
    fn language(&self) -> AppResult<Option<&'static str>> {
        match self.language.as_deref().map(str::trim).filter(|l| !l.is_empty()) {
            None => Ok(None),
            Some(raw) => language::parse(raw).map(|l| Some(l.code)).ok_or_else(|| AppError::BadRequest(format!("Splee doesn't write in {raw}"))),
        }
    }

    fn min_score(&self) -> AppResult<Option<i16>> {
        match self.min_score {
            Some(n) if !(0..=100).contains(&n) => Err(AppError::BadRequest("min_score goes from 0 to 100".into())),
            n => Ok(n),
        }
    }

    /// The order by, `default` without a sort. The tiebreaks keep pages stable.
    fn order(&self, alias: &str, default: &'static str) -> AppResult<String> {
        Ok(match self.sort.as_deref().filter(|s| !s.is_empty()) {
            None => default.replace("{a}", alias),
            Some("score") => format!("{alias}.score desc, {alias}.score_intent desc, {alias}.created_at desc, {alias}.id desc"),
            Some("newest") => format!("{alias}.created_at desc, {alias}.id desc"),
            Some(_) => return Err(AppError::BadRequest("Sort by score or newest".into())),
        })
    }
}

const COMPANY_FILTER: &str = "from companies c where project_id = $1
       and ($2::uuid is null or segment_id = $2)
       and ($3::text is null or status = $3)
       and ($4::text is null or name ilike $4 or domain::text ilike $4)
       and ($5::int2 is null or score >= $5)";

#[derive(Serialize, FromRow)]
pub struct CompanyRow {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub company: Company,
    /// The watched roles that open posts at the company match, newest first, each once.
    pub hiring: Vec<String>,
    /// The tools its last tech scan found, in catalogue order.
    pub tech: Vec<String>,
}

async fn list_companies(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<CompanyRow>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(companies_page(&state.db, id, q, &page).await?))
}

/// A page of a project's companies, best fit first unless sorted. The caller checks the project belongs to its organization.
pub async fn companies_page(db: &PgPool, project_id: Uuid, q: ListQuery, page: &PageQuery) -> AppResult<Paginated<CompanyRow>> {
    let like = util_input::like_pattern(q.q.as_deref());
    let min_score = q.min_score()?;
    let order = q.order("c", "{a}.fit_score desc, {a}.created_at desc, {a}.id desc")?;
    let status = q.status.filter(|s| !s.is_empty());
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {COMPANY_FILTER}"))
        .bind(project_id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .bind(min_score)
        .fetch_one(db)
        .await?;
    let items = sqlx::query_as::<_, CompanyRow>(&format!(
        "select c.*, array(select j.role from job_openings j where j.project_id = c.project_id and j.domain = c.domain and j.closed_at is null
                           group by j.role order by max(j.first_seen) desc, j.role) as hiring,
                coalesce((select t.techs from tech_scans t where t.domain = c.domain), '{{}}') as tech
         {COMPANY_FILTER} order by {order} limit $6 offset $7"
    ))
    .bind(project_id)
    .bind(q.segment_id)
    .bind(&status)
    .bind(&like)
    .bind(min_score)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(db)
    .await?;
    Ok(Paginated { items, total })
}

#[derive(Deserialize)]
struct CompanyBody {
    status: String,
}

async fn update_company(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<CompanyBody>,
) -> AppResult<Json<Company>> {
    let company: Company = util_scope::load(&state.db, "companies", id, auth.org_id).await?;
    util_input::one_of("status", &body.status, &["new", "people_found", "no_people", "excluded"])?;
    let company = sqlx::query_as::<_, Company>("update companies set status = $2 where id = $1 returning *")
        .bind(company.id)
        .bind(&body.status)
        .fetch_one(&state.db)
        .await?;
    Ok(Json(company))
}

#[derive(Serialize, FromRow)]
pub struct LeadRow {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub lead: Lead,
    pub segment_name: Option<String>,
    pub last_email_status: Option<String>,
    /// The watched roles that open posts at the lead's company match, newest first, each once.
    pub hiring: Vec<String>,
    /// The owner's name, or their email when they have none.
    pub owner_name: Option<String>,
    pub owner_email: Option<String>,
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub effective: Effective,
}

/// The lead's own language as a campaign set to "Each lead's language" picks it, and where it came from: "set" on the lead,
/// their "country", their company's "site", or English by "default". See language.rs.
#[derive(Serialize, FromRow)]
pub struct Effective {
    pub effective_language: String,
    pub effective_language_source: String,
}

/// Select list for LeadRow. Callers append `where` on `l` (leads), `s` (segments) and `own` (language::OWN_JOIN).
pub static LEAD_ROW_SELECT: LazyLock<String> = LazyLock::new(|| {
    format!(
        "select l.*, s.name as segment_name,
           (select e.status from email_messages e where e.lead_id = l.id and e.direction = 'outbound'
             order by e.created_at desc limit 1) as last_email_status,
           array(select j.role from job_openings j where j.project_id = l.project_id and j.domain = l.company_domain and j.closed_at is null
                 group by j.role order by max(j.first_seen) desc, j.role) as hiring,
           coalesce(nullif(o.name, ''), o.email::text) as owner_name, o.email::text as owner_email,
           own.code as effective_language, own.source as effective_language_source
         from leads l left join segments s on s.id = l.segment_id left join users o on o.id = l.owner_id {}",
        *language::OWN_JOIN
    )
});

pub async fn lead_rows(db: &PgPool, ids: &[Uuid]) -> AppResult<Vec<LeadRow>> {
    Ok(sqlx::query_as::<_, LeadRow>(&format!("{} where l.id = any($1)", *LEAD_ROW_SELECT)).bind(ids).fetch_all(db).await?)
}

/// The Leads page's filters as $1 to $9, $7 and $8 an OwnerFilter. They need `own`, so count queries join language::OWN_JOIN.
fn lead_filter() -> String {
    format!(
        "where l.project_id = $1
           and ($2::uuid is null or l.segment_id = $2)
           and ($3::text is null or l.status = $3)
           and ($4::text is null or l.full_name ilike $4 or l.email::text ilike $4
                or l.company_name ilike $4 or l.title ilike $4)
           and (not $5 or l.email is null)
           and ($6::int2 is null or l.score >= $6)
           and {}
           and ($9::text is null or own.code = $9)",
        ownership::sql("l", 7, 8)
    )
}

async fn list_leads(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<LeadRow>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(page_of(&state.db, id, Some(auth.user_id), q, &page).await?))
}

const NEWEST: &str = "{a}.created_at desc, {a}.id desc";

/// A page of a project's leads, newest first unless sorted. The caller checks the project belongs to its organization. `me` is
/// who "mine" means, None for an API key.
pub async fn page_of(db: &PgPool, project_id: Uuid, me: Option<Uuid>, q: ListQuery, page: &PageQuery) -> AppResult<Paginated<LeadRow>> {
    let like = util_input::like_pattern(q.q.as_deref());
    let min_score = q.min_score()?;
    let order = q.order("l", NEWEST)?;
    let owner = OwnerFilter::parse(q.owner.as_deref(), me)?;
    let language = q.language()?;
    let status = q.status.filter(|s| !s.is_empty());
    let missing = q.email.as_deref() == Some("missing");
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from leads l {} {}", *language::OWN_JOIN, lead_filter()))
        .bind(project_id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .bind(missing)
        .bind(min_score)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(language)
        .fetch_one(db)
        .await?;
    let items = sqlx::query_as::<_, LeadRow>(&format!("{} {} order by {order} limit $10 offset $11", *LEAD_ROW_SELECT, lead_filter()))
        .bind(project_id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .bind(missing)
        .bind(min_score)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(language)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(db)
        .await?;
    Ok(Paginated { items, total })
}

/// Every lead matching the list filters, as a CSV download.
async fn export_leads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let project = auth.project(&state.db, id).await?;
    let like = util_input::like_pattern(q.q.as_deref());
    let min_score = q.min_score()?;
    let order = q.order("l", NEWEST)?;
    let owner = OwnerFilter::parse(q.owner.as_deref(), Some(auth.user_id))?;
    let language = q.language()?;
    let status = q.status.filter(|s| !s.is_empty());
    let rows = sqlx::query_as::<_, LeadRow>(&format!("{} {} order by {order}", *LEAD_ROW_SELECT, lead_filter()))
        .bind(id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .bind(q.email.as_deref() == Some("missing"))
        .bind(min_score)
        .bind(owner.mode())
        .bind(owner.user())
        .bind(language)
        .fetch_all(&state.db)
        .await?;

    let header = [
        "full_name", "first_name", "last_name", "title", "email", "email_status", "phone", "company_name", "company_domain",
        "linkedin_url", "x_url", "city", "country_code", "campaign", "status", "last_email_status", "last_contacted_at", "created_at", "notes",
        "score", "score_fit", "score_intent", "score_reasons", "owner_email", "language",
    ];
    let records = rows.iter().map(|r| {
        let l = &r.lead;
        [
            l.full_name.clone(),
            l.first_name.clone(),
            l.last_name.clone(),
            l.title.clone(),
            l.email.clone().unwrap_or_default(),
            l.email_status.clone(),
            l.phone.clone(),
            l.company_name.clone(),
            l.company_domain.clone().unwrap_or_default(),
            l.linkedin_url.clone(),
            l.x_url.clone(),
            l.city.clone(),
            l.country_code.clone(),
            r.segment_name.clone().unwrap_or_default(),
            l.status.clone(),
            r.last_email_status.clone().unwrap_or_default(),
            l.last_contacted_at.map(|t| t.to_rfc3339()).unwrap_or_default(),
            l.created_at.to_rfc3339(),
            l.notes.clone(),
            l.score.to_string(),
            l.score_fit.to_string(),
            l.score_intent.to_string(),
            l.score_reasons.iter().map(|r| r.text.as_str()).collect::<Vec<_>>().join("; "),
            r.owner_email.clone().unwrap_or_default(),
            r.effective.effective_language.clone(),
        ]
    });
    let slug: String = project.domain.as_deref().unwrap_or(&project.name).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' }).collect();
    util_csv::download(&format!("{}-leads.csv", slug.trim_matches('-')), &header, records)
}

#[derive(Serialize)]
pub struct LeadDetail {
    #[serde(flatten)]
    pub lead: Lead,
    pub segment_name: Option<String>,
    pub company: Option<Company>,
    pub emails: Vec<EmailMessage>,
    /// Opens and clicks of their emails that counted, oldest first.
    pub email_events: Vec<EmailEvent>,
    /// Texts Splee wrote or sent them, oldest first. Their answers are in `emails`, with a channel.
    pub texts: Vec<TextMessage>,
    /// Calls to them, open and made, oldest first.
    pub calls: Vec<Call>,
    /// Sales meetings with them from Fireflies, Fathom, Gong or Zoom, oldest first.
    pub meetings: Vec<super::meetings::MeetingRow>,
    /// Open roles at the lead's company that match the project's hiring signals.
    pub hiring: Vec<hiring::Post>,
    /// Their open deal, or else the one that closed last.
    pub deal: Option<crate::deals::Deal>,
    /// The move that made them a lead here, when a job change did.
    pub job_change: Option<JobChangeFrom>,
    /// The owner's name, or their email when they have none.
    pub owner_name: Option<String>,
    pub owner_email: Option<String>,
    #[serde(flatten)]
    pub effective: Effective,
}

/// Lead with its company and every email in both directions, oldest first, scored again first so the score shown is
/// current. The caller checks org access.
pub async fn detail(db: &PgPool, mut lead: Lead) -> AppResult<LeadDetail> {
    match crate::score::refresh_lead(db, lead.id).await {
        Ok(Some(s)) => {
            (lead.score, lead.score_fit, lead.score_intent, lead.score_reasons) = (s.score, s.fit, s.intent, sqlx::types::Json(s.reasons));
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(lead_id = %lead.id, error = ?e, "could not score a lead"),
    }
    let segment_name: Option<String> = sqlx::query_scalar("select name from segments where id = $1")
        .bind(lead.segment_id)
        .fetch_optional(db)
        .await?;
    let company = sqlx::query_as::<_, Company>("select * from companies where id = $1")
        .bind(lead.company_id)
        .fetch_optional(db)
        .await?;
    let emails = sqlx::query_as::<_, EmailMessage>("select * from email_messages where lead_id = $1 order by created_at")
        .bind(lead.id)
        .fetch_all(db)
        .await?;
    let texts = sqlx::query_as::<_, TextMessage>("select * from text_messages where lead_id = $1 order by coalesce(sent_at, due_at, created_at)")
        .bind(lead.id)
        .fetch_all(db)
        .await?;
    let calls = sqlx::query_as::<_, Call>("select * from calls where lead_id = $1 order by coalesce(called_at, closed_at, due_at)")
        .bind(lead.id)
        .fetch_all(db)
        .await?;
    let hiring = match &lead.company_domain {
        Some(domain) => hiring::open_at(db, lead.project_id, std::slice::from_ref(domain)).await?,
        None => vec![],
    };
    let meetings = super::meetings::for_lead(db, lead.id).await?;
    let deal = crate::deals::for_lead(db, lead.id).await?;
    let job_change = sqlx::query_as(
        "select j.old_company, j.old_domain::text, w.lead_id as old_lead_id, w.relationship, j.seen_at from job_changes j
         join watched_people w on w.id = j.watched_id where j.new_lead_id = $1 and j.dismissed_at is null order by j.seen_at desc limit 1",
    )
    .bind(lead.id)
    .fetch_optional(db)
    .await?;
    let (owner_name, owner_email) = owner_of(db, &lead).await?;
    let email_events = email_events(db, lead.id).await?;
    let effective = sqlx::query_as::<_, Effective>(&format!(
        "select own.code as effective_language, own.source as effective_language_source from leads l {} where l.id = $1",
        *language::OWN_JOIN
    ))
    .bind(lead.id)
    .fetch_one(db)
    .await?;
    Ok(LeadDetail {
        lead,
        segment_name,
        company,
        emails,
        email_events,
        texts,
        calls,
        meetings,
        hiring,
        deal,
        job_change,
        owner_name,
        owner_email,
        effective,
    })
}

/// An email's opens, or its clicks on one link, that counted. See tracking.rs.
#[derive(Serialize, FromRow, Debug)]
pub struct EmailEvent {
    email_id: Uuid,
    /// "open" or "click".
    kind: String,
    /// The clicked link's host and path, like acme.com/pricing.
    link: Option<String>,
    url: Option<String>,
    count: i64,
    first_at: Ts,
    last_at: Ts,
}

async fn email_events(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Vec<EmailEvent>> {
    let mut events: Vec<EmailEvent> = sqlx::query_as(
        "select h.email_id, h.kind, null::text as link, k.url, count(*) as count, min(h.created_at) as first_at, max(h.created_at) as last_at
         from tracking_hits h left join tracked_links k on k.token = h.link_token
         where h.lead_id = $1 and h.filter_reason is null
         group by h.email_id, h.kind, k.url order by first_at",
    )
    .bind(lead_id)
    .fetch_all(db)
    .await?;
    for e in &mut events {
        e.link = e.url.as_deref().map(crate::tracking::link_label);
    }
    Ok(events)
}

/// Where a lead from a job change was before, and what they did with the project there: won, booked or hot.
#[derive(Serialize, FromRow)]
pub struct JobChangeFrom {
    old_company: String,
    old_domain: Option<String>,
    old_lead_id: Option<Uuid>,
    relationship: String,
    seen_at: Ts,
}

/// The lead owner's name and email, the name falling back to the email like everywhere else.
async fn owner_of(db: &PgPool, lead: &Lead) -> sqlx::Result<(Option<String>, Option<String>)> {
    let found: Option<(String, String)> = sqlx::query_as("select coalesce(nullif(name, ''), email::text), email::text from users where id = $1")
        .bind(lead.owner_id)
        .fetch_optional(db)
        .await?;
    Ok(found.map_or((None, None), |(name, email)| (Some(name), Some(email))))
}

async fn get_lead(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(detail(&state.db, lead).await?))
}

#[derive(Deserialize)]
struct LeadBody {
    status: Option<String>,
    email: Option<String>,
    title: Option<String>,
    full_name: Option<String>,
    first_name: Option<String>,
    last_name: Option<String>,
    linkedin_url: Option<String>,
    x_url: Option<String>,
    /// Blank clears it. Anything else is read as E.164, or by the lead's or the project's country.
    phone: Option<String>,
    company_name: Option<String>,
    notes: Option<String>,
    /// A code from language::LANGUAGES or its name. Blank goes back to their country and site.
    language: Option<String>,
}

/// The country a lead's number is read by when it has no + in front: theirs, else the project's own.
pub(crate) async fn phone_country(db: &PgPool, project_id: Uuid, lead_country: &str) -> sqlx::Result<String> {
    if !lead_country.trim().is_empty() {
        return Ok(lead_country.trim().to_string());
    }
    let project: Option<String> = sqlx::query_scalar("select company ->> 'country_code' from projects where id = $1").bind(project_id).fetch_one(db).await?;
    Ok(project.unwrap_or_default())
}

async fn update_lead(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<LeadBody>,
) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    if let Some(status) = &body.status {
        util_input::one_of("status", status, LEAD_STATUSES)?;
    }
    // Blank clears a wrong address. Leads without one already exist, research leaves them when it finds none.
    let clear_email = body.email.as_deref().is_some_and(|raw| raw.trim().is_empty());
    let email = match body.email.as_deref() {
        Some(raw) if !clear_email => Some(util_input::email(raw).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?),
        _ => None,
    };
    // Blank clears it. Anything else has to hold an X handle, stored as https://x.com/<handle> like research writes it.
    let x_url = match body.x_url.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        Some(raw) => match agent::x::profile_url(raw) {
            url if url.is_empty() => return Err(AppError::BadRequest("Enter an X profile link or @handle".into())),
            url => Some(url),
        },
        None => None,
    };
    let notes = util_input::notes(body.notes.as_deref())?;
    let phone = match body.phone.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        Some(raw) => {
            let country = phone_country(&state.db, lead.project_id, &lead.country_code).await?;
            let phone = crate::phone::normalize(raw, &country);
            Some(phone.ok_or_else(|| AppError::BadRequest("Enter the phone number with its country code, like +44 7700 900123".into()))?)
        }
        None => None,
    };
    let language = body.language.as_deref().map(language::lead_setting).transpose().map_err(AppError::BadRequest)?;
    let before = lead.status;
    let lead = sqlx::query_as::<_, Lead>(
        "update leads set
           status = coalesce($2, status),
           email = case when $11 then null else coalesce($3, email) end,
           email_status = case when $11 or ($3 is not null and $3 is distinct from email::text) then 'unknown' else email_status end,
           title = coalesce($4, title),
           full_name = coalesce($5, full_name),
           first_name = coalesce($6, first_name),
           last_name = coalesce($7, last_name),
           linkedin_url = coalesce($8, linkedin_url),
           company_name = coalesce($9, company_name),
           x_url = coalesce($10, x_url),
           notes = coalesce($12, notes),
           phone = coalesce($13, phone),
           language = coalesce($14, language)
         where id = $1 returning *",
    )
    .bind(lead.id)
    .bind(body.status)
    .bind(email)
    .bind(body.title.as_deref().map(str::trim))
    .bind(body.full_name.as_deref().map(str::trim))
    .bind(body.first_name.as_deref().map(str::trim))
    .bind(body.last_name.as_deref().map(str::trim))
    .bind(body.linkedin_url.as_deref().map(str::trim))
    .bind(body.company_name.as_deref().map(str::trim))
    .bind(x_url)
    .bind(clear_email)
    .bind(notes)
    .bind(phone)
    .bind(language)
    .fetch_one(&state.db)
    .await?;
    crate::hooks::team_set_status(&state, &before, &lead).await;
    Ok(Json(detail(&state.db, lead).await?))
}

/// Calls leads booked that haven't ended, soonest first, for the overview. One that started in the last hour may still be on.
async fn upcoming_meetings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Lead>>> {
    auth.project(&state.db, id).await?;
    let leads = sqlx::query_as::<_, Lead>(
        "select * from leads where project_id = $1 and meeting_at > now() - interval '1 hour' order by meeting_at, id limit 10",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(leads))
}

#[derive(Deserialize)]
struct RemoveQuery {
    #[serde(default)]
    forget: bool,
}

async fn remove_lead(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<RemoveQuery>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    erase(&state.db, auth.org_id, std::slice::from_ref(&lead), q.forget).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct OwnerBody {
    /// A team member's id, or null for nobody.
    owner_id: Option<Uuid>,
}

/// Anyone on the team can give a lead to anyone on the team, or take it back.
async fn set_owner(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<OwnerBody>) -> AppResult<Json<LeadDetail>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    ownership::assign(&state.db, auth.org_id, &[lead.id], body.owner_id).await?;
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(detail(&state.db, lead).await?))
}

/// Leads picked on the Leads page. Ids from another project are ignored.
#[derive(Deserialize)]
struct PickedLeads {
    ids: Vec<Uuid>,
    /// For a move, the campaign they join, or null for none.
    segment_id: Option<Uuid>,
    /// For a status change, the new status.
    status: Option<String>,
    /// For an assignment, the new owner, or null for nobody.
    owner_id: Option<Uuid>,
}

/// A page of the Leads table holds at most 100, so a bigger pick isn't from the page.
const MAX_PICKED: usize = 500;

async fn picked(state: &AppState, auth: &Auth, project_id: Uuid, body: &PickedLeads) -> AppResult<()> {
    auth.project(&state.db, project_id).await?;
    if body.ids.len() > MAX_PICKED {
        return Err(AppError::BadRequest(format!("Pick at most {MAX_PICKED} leads at a time")));
    }
    Ok(())
}

async fn move_leads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PickedLeads>) -> AppResult<Json<Value>> {
    picked(&state, &auth, id, &body).await?;
    super::imports::check_segment(&state, id, body.segment_id).await?;
    let moved = sqlx::query("update leads set segment_id = $3 where project_id = $1 and id = any($2)")
        .bind(id)
        .bind(&body.ids)
        .bind(body.segment_id)
        .execute(&state.db)
        .await?
        .rows_affected();
    Ok(Json(json!({ "moved": moved })))
}

/// A lead whose status a bulk change moved, with the status it had.
#[derive(FromRow)]
struct Changed {
    before: String,
    #[sqlx(flatten)]
    lead: Lead,
}

/// Leads already in the status are left alone, so only real changes announce a booked meeting.
async fn set_status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PickedLeads>) -> AppResult<Json<Value>> {
    picked(&state, &auth, id, &body).await?;
    let status = body.status.as_deref().ok_or_else(|| AppError::BadRequest("Pick a status".into()))?;
    util_input::one_of("status", status, LEAD_STATUSES)?;
    // The joined copy of the row still has the old status, so it can be returned next to the new one.
    let changed: Vec<Changed> = sqlx::query_as(
        "update leads l set status = $3 from leads old
         where old.id = l.id and l.project_id = $1 and l.id = any($2) and l.status <> $3
         returning old.status as before, l.*",
    )
    .bind(id)
    .bind(&body.ids)
    .bind(status)
    .fetch_all(&state.db)
    .await?;
    for c in &changed {
        crate::hooks::team_set_status(&state, &c.before, &c.lead).await;
    }
    Ok(Json(json!({ "changed": changed.len() })))
}

async fn assign_leads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PickedLeads>) -> AppResult<Json<Value>> {
    picked(&state, &auth, id, &body).await?;
    let ids: Vec<Uuid> = sqlx::query_scalar("select id from leads where project_id = $1 and id = any($2)")
        .bind(id)
        .bind(&body.ids)
        .fetch_all(&state.db)
        .await?;
    let changed = ownership::assign(&state.db, auth.org_id, &ids, body.owner_id).await?;
    Ok(Json(json!({ "changed": changed })))
}

async fn delete_leads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PickedLeads>) -> AppResult<Json<Value>> {
    picked(&state, &auth, id, &body).await?;
    let leads: Vec<Lead> = sqlx::query_as("select * from leads where project_id = $1 and id = any($2)")
        .bind(id)
        .bind(&body.ids)
        .fetch_all(&state.db)
        .await?;
    erase(&state.db, auth.org_id, &leads, false).await?;
    Ok(Json(json!({ "deleted": leads.len() })))
}

#[derive(Deserialize)]
struct FindEmails {
    #[serde(default)]
    ids: Vec<Uuid>,
    /// Instead of ids, every lead without an email that the Leads page's filters match.
    #[serde(default)]
    all: bool,
    #[serde(flatten)]
    filter: ListQuery,
}

/// Queues leads without an email for the email waterfall, ahead of the leads it finds on its own. A lead already searched
/// is searched again, which is how a search is retried.
async fn find_emails(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<FindEmails>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    if body.ids.len() > MAX_PICKED {
        return Err(AppError::BadRequest(format!("Pick at most {MAX_PICKED} leads at a time")));
    }
    if !crate::waterfall::connected(&state.db, id).await? {
        return Err(AppError::BadRequest("Connect an email finder or verifier under Integrations first".into()));
    }
    let ids: Vec<Uuid> = if body.all {
        let min_score = body.filter.min_score()?;
        let owner = OwnerFilter::parse(body.filter.owner.as_deref(), Some(auth.user_id))?;
        let language = body.filter.language()?;
        sqlx::query_scalar(&format!("select l.id from leads l {} {}", *language::OWN_JOIN, lead_filter()))
            .bind(id)
            .bind(body.filter.segment_id)
            .bind(body.filter.status.filter(|s| !s.is_empty()))
            .bind(util_input::like_pattern(body.filter.q.as_deref()))
            .bind(true)
            .bind(min_score)
            .bind(owner.mode())
            .bind(owner.user())
            .bind(language)
            .fetch_all(&state.db)
            .await?
    } else {
        body.ids
    };
    let queued = sqlx::query("update leads set email_search = 'queued', email_search_note = '' where project_id = $1 and email is null and id = any($2)")
        .bind(id)
        .bind(&ids)
        .execute(&state.db)
        .await?
        .rows_affected();
    waterfall::wake();
    Ok(Json(json!({ "queued": queued })))
}

/// How many of the project's leads wait for an email search, and how many more its organization can search this hour.
async fn email_progress(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let queued: i64 = sqlx::query_scalar("select count(*) from leads where project_id = $1 and email is null and email_search = 'queued'")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let room = (waterfall::HOURLY - waterfall::searched_last_hour(&state.db, auth.org_id).await?).max(0);
    Ok(Json(json!({ "queued": queued, "room": room, "hourly": waterfall::HOURLY })))
}

/// Deletes leads with their emails and the webhook log entries that carry their details. With `forget`, their
/// addresses go on the do-not-contact list first, so no later import or search brings them back.
async fn erase(db: &PgPool, org_id: Uuid, leads: &[Lead], forget: bool) -> AppResult<()> {
    if forget {
        suppressions::insert(db, org_id, leads.iter().filter_map(|l| l.email.as_deref())).await?;
    }
    let ids: Vec<Uuid> = leads.iter().map(|l| l.id).collect();
    let ids_text: Vec<String> = ids.iter().map(Uuid::to_string).collect();
    let projects: Vec<Uuid> = leads.iter().map(|l| l.project_id).collect();
    let mut tx = db.begin().await?;
    sqlx::query("delete from webhook_deliveries where project_id = any($1) and payload->'lead'->>'id' = any($2)")
        .bind(&projects)
        .bind(&ids_text)
        .execute(&mut *tx)
        .await?;
    sqlx::query("delete from leads where id = any($1)").bind(&ids).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn forgetting_a_lead_suppresses_them_and_clears_the_webhook_log(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(&db).await.unwrap();
        let add = |name: &'static str, email: &'static str| {
            sqlx::query_as::<_, Lead>("insert into leads (project_id, full_name, email) values ($1, $2, $3) returning *")
                .bind(project)
                .bind(name)
                .bind(email)
                .fetch_one(&db)
        };
        let (sarah, marcus) = (add("Sarah Chen", "sarah@northwind.example").await.unwrap(), add("Marcus Webb", "marcus@fernway.example").await.unwrap());
        for lead in [&sarah, &marcus] {
            sqlx::query("insert into webhook_deliveries (project_id, event, payload) values ($1, 'hot_lead', $2)")
                .bind(project)
                .bind(json!({"lead": {"id": lead.id}}))
                .execute(&db)
                .await
                .unwrap();
        }

        erase(&db, org, std::slice::from_ref(&sarah), true).await.unwrap();
        erase(&db, org, std::slice::from_ref(&marcus), false).await.unwrap();

        let leads: i64 = sqlx::query_scalar("select count(*) from leads").fetch_one(&db).await.unwrap();
        let deliveries: i64 = sqlx::query_scalar("select count(*) from webhook_deliveries").fetch_one(&db).await.unwrap();
        let suppressed: Vec<String> = sqlx::query_scalar("select value::text from suppressions where org_id = $1").bind(org).fetch_all(&db).await.unwrap();
        assert_eq!((leads, deliveries), (0, 0));
        assert_eq!(suppressed, ["sarah@northwind.example"]);
    }
}
