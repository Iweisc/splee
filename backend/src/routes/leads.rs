use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_input::{self, LEAD_STATUSES};
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::agent::{self, kind};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{Company, EmailMessage, Lead, LinkedinMessage};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/companies", get(list_companies))
        .route("/companies/{id}", patch(update_company))
        .route("/projects/{id}/leads", get(list_leads))
        .route("/projects/{id}/leads/export", get(export_leads))
        .route("/leads/{id}", get(get_lead).patch(update_lead).delete(remove_lead))
        .route("/projects/{id}/linkedin", get(list_linkedin))
        .route("/leads/{id}/linkedin", post(generate_linkedin))
        .route("/linkedin/{id}", patch(update_linkedin))
}

#[derive(Deserialize)]
struct ListQuery {
    segment_id: Option<Uuid>,
    status: Option<String>,
    q: Option<String>,
}

const COMPANY_FILTER: &str = "from companies where project_id = $1
       and ($2::uuid is null or segment_id = $2)
       and ($3::text is null or status = $3)
       and ($4::text is null or name ilike $4 or domain::text ilike $4)";

async fn list_companies(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Company>>> {
    auth.project(&state.db, id).await?;
    let like = util_input::like_pattern(q.q.as_deref());
    let status = q.status.filter(|s| !s.is_empty());
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {COMPANY_FILTER}"))
        .bind(id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Company>(&format!(
        "select * {COMPANY_FILTER} order by fit_score desc, created_at desc limit $5 offset $6"
    ))
    .bind(id)
    .bind(q.segment_id)
    .bind(&status)
    .bind(&like)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Paginated { items, total }))
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
}

/// Select list for LeadRow. Callers append `where` on `l` (leads) and `s` (segments).
pub const LEAD_ROW_SELECT: &str = "select l.*, s.name as segment_name,
       (select e.status from email_messages e where e.lead_id = l.id and e.direction = 'outbound'
         order by e.created_at desc limit 1) as last_email_status
     from leads l left join segments s on s.id = l.segment_id";

pub async fn lead_rows(db: &PgPool, ids: &[Uuid]) -> AppResult<Vec<LeadRow>> {
    Ok(sqlx::query_as::<_, LeadRow>(&format!("{LEAD_ROW_SELECT} where l.id = any($1)")).bind(ids).fetch_all(db).await?)
}

const LEAD_FILTER: &str = "where l.project_id = $1
       and ($2::uuid is null or l.segment_id = $2)
       and ($3::text is null or l.status = $3)
       and ($4::text is null or l.full_name ilike $4 or l.email::text ilike $4
            or l.company_name ilike $4 or l.title ilike $4)";

async fn list_leads(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<LeadRow>>> {
    auth.project(&state.db, id).await?;
    let like = util_input::like_pattern(q.q.as_deref());
    let status = q.status.filter(|s| !s.is_empty());
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from leads l {LEAD_FILTER}"))
        .bind(id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, LeadRow>(&format!(
        "{LEAD_ROW_SELECT} {LEAD_FILTER} order by l.created_at desc limit $5 offset $6"
    ))
    .bind(id)
    .bind(q.segment_id)
    .bind(&status)
    .bind(&like)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Paginated { items, total }))
}

/// Every lead matching the list filters, as a CSV download.
async fn export_leads(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<ListQuery>) -> AppResult<Response> {
    let project = auth.project(&state.db, id).await?;
    let like = util_input::like_pattern(q.q.as_deref());
    let status = q.status.filter(|s| !s.is_empty());
    let rows = sqlx::query_as::<_, LeadRow>(&format!("{LEAD_ROW_SELECT} {LEAD_FILTER} order by l.created_at desc"))
        .bind(id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;

    let invalid = |e: csv::Error| AppError::Internal(e.into());
    let mut out = csv::Writer::from_writer(Vec::new());
    out.write_record([
        "full_name", "first_name", "last_name", "title", "email", "email_status", "company_name", "company_domain", "linkedin_url",
        "x_url", "city", "country_code", "campaign", "status", "last_email_status", "last_contacted_at", "created_at", "notes",
    ])
    .map_err(invalid)?;
    for r in &rows {
        let l = &r.lead;
        let last_contacted = l.last_contacted_at.map(|t| t.to_rfc3339()).unwrap_or_default();
        let created = l.created_at.to_rfc3339();
        let record = [
            l.full_name.as_str(),
            &l.first_name,
            &l.last_name,
            &l.title,
            l.email.as_deref().unwrap_or_default(),
            &l.email_status,
            &l.company_name,
            l.company_domain.as_deref().unwrap_or_default(),
            &l.linkedin_url,
            &l.x_url,
            &l.city,
            &l.country_code,
            r.segment_name.as_deref().unwrap_or_default(),
            &l.status,
            r.last_email_status.as_deref().unwrap_or_default(),
            &last_contacted,
            &created,
            &l.notes,
        ];
        out.write_record(record.map(spreadsheet_safe)).map_err(invalid)?;
    }
    let body = out.into_inner().map_err(|e| AppError::Internal(e.into_error().into()))?;

    let slug: String = project.domain.as_deref().unwrap_or(&project.name).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' }).collect();
    let disposition = format!("attachment; filename=\"{}-leads.csv\"", slug.trim_matches('-'));
    Ok(([(header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()), (header::CONTENT_DISPOSITION, disposition)], body).into_response())
}

/// Lead fields come from the web and from imports. A cell starting with one of these runs as a formula in Excel or Sheets.
fn spreadsheet_safe(cell: &str) -> String {
    match cell.chars().next() {
        Some('=' | '+' | '-' | '@' | '\t' | '\r') => format!("'{cell}"),
        _ => cell.to_string(),
    }
}

#[derive(Serialize)]
pub struct LeadDetail {
    #[serde(flatten)]
    pub lead: Lead,
    pub segment_name: Option<String>,
    pub company: Option<Company>,
    pub emails: Vec<EmailMessage>,
}

/// Lead with its company and every email in both directions, oldest first. The caller checks org access.
pub async fn detail(db: &PgPool, lead: Lead) -> AppResult<LeadDetail> {
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
    Ok(LeadDetail { lead, segment_name, company, emails })
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
    company_name: Option<String>,
    notes: Option<String>,
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
           notes = coalesce($12, notes)
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
    .fetch_one(&state.db)
    .await?;
    crate::hooks::status_changed(&state, &before, &lead).await;
    Ok(Json(detail(&state.db, lead).await?))
}

async fn remove_lead(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    sqlx::query("delete from leads where id = $1").bind(lead.id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct LinkedinQuery {
    segment_id: Option<Uuid>,
}

#[derive(Serialize)]
struct LinkedinThread {
    lead: LeadRow,
    messages: Vec<LinkedinMessage>,
}

/// Leads with a LinkedIn profile or a generated sequence, so the page can offer "generate" for the rest.
async fn list_linkedin(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<LinkedinQuery>,
) -> AppResult<Json<Vec<LinkedinThread>>> {
    auth.project(&state.db, id).await?;
    let leads = sqlx::query_as::<_, LeadRow>(&format!(
        "{LEAD_ROW_SELECT}
         where l.project_id = $1 and ($2::uuid is null or l.segment_id = $2)
           and (l.linkedin_url <> '' or exists (select 1 from linkedin_messages m where m.lead_id = l.id))
         order by l.created_at desc limit 500"
    ))
    .bind(id)
    .bind(q.segment_id)
    .fetch_all(&state.db)
    .await?;
    let ids: Vec<Uuid> = leads.iter().map(|l| l.lead.id).collect();
    let messages = sqlx::query_as::<_, LinkedinMessage>(
        "select * from linkedin_messages where lead_id = any($1) order by step, created_at",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut by_lead: HashMap<Uuid, Vec<LinkedinMessage>> = HashMap::new();
    for message in messages {
        by_lead.entry(message.lead_id).or_default().push(message);
    }
    let threads = leads
        .into_iter()
        .map(|lead| {
            let messages = by_lead.remove(&lead.lead.id).unwrap_or_default();
            LinkedinThread { lead, messages }
        })
        .collect();
    Ok(Json(threads))
}

async fn generate_linkedin(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let lead: Lead = util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    let run_id =
        agent::enqueue(&state.db, kind::LINKEDIN_SEQUENCE, Some(lead.project_id), Some(auth.org_id), json!({ "lead_id": lead.id }))
            .await?;
    Ok(Json(json!({ "run_id": run_id })))
}

#[derive(Deserialize)]
struct LinkedinBody {
    status: String,
    body: Option<String>,
}

async fn update_linkedin(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Json(body): Json<LinkedinBody>,
) -> AppResult<Json<LinkedinMessage>> {
    let message: LinkedinMessage = util_scope::load(&state.db, "linkedin_messages", id, auth.org_id).await?;
    util_input::one_of("status", &body.status, &["todo", "done", "skipped"])?;
    let message = sqlx::query_as::<_, LinkedinMessage>(
        "update linkedin_messages set status = $2, body = coalesce($3, body) where id = $1 returning *",
    )
    .bind(message.id)
    .bind(&body.status)
    .bind(body.body)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(message))
}

#[cfg(test)]
mod tests {
    use super::spreadsheet_safe;

    #[test]
    fn formula_cells_are_quoted() {
        assert_eq!(spreadsheet_safe("=HYPERLINK(\"x\")"), "'=HYPERLINK(\"x\")");
        assert_eq!(spreadsheet_safe("-1+2"), "'-1+2");
        assert_eq!(spreadsheet_safe("Head of @Growth"), "Head of @Growth");
        assert_eq!(spreadsheet_safe(""), "");
    }
}
