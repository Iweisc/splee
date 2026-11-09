use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::routing::{get, patch, post};
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
        .route("/projects/{id}/leads/move", post(move_leads))
        .route("/projects/{id}/leads/delete", post(delete_leads))
        .route("/projects/{id}/meetings", get(upcoming_meetings))
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
        "{LEAD_ROW_SELECT} {LEAD_FILTER} order by l.created_at desc, l.id desc limit $5 offset $6"
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
    let rows = sqlx::query_as::<_, LeadRow>(&format!("{LEAD_ROW_SELECT} {LEAD_FILTER} order by l.created_at desc, l.id desc"))
        .bind(id)
        .bind(q.segment_id)
        .bind(&status)
        .bind(&like)
        .fetch_all(&state.db)
        .await?;

    let header = [
        "full_name", "first_name", "last_name", "title", "email", "email_status", "company_name", "company_domain", "linkedin_url",
        "x_url", "city", "country_code", "campaign", "status", "last_email_status", "last_contacted_at", "created_at", "notes",
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

/// Leads picked on the Leads page. Ids from another project are ignored.
#[derive(Deserialize)]
struct PickedLeads {
    ids: Vec<Uuid>,
    /// For a move, the campaign they join, or null for none.
    segment_id: Option<Uuid>,
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
