use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::check_project;
use crate::auth::ApiKeyAuth;
use crate::deals::{self, Deal, DealFilter, Move, NewDeal, Stage};
use crate::email_lookups::{self, Batch, BatchPerson, Found, Person, PhoneFound, PhoneQuery};
use crate::error::{AppError, AppResult};
use crate::jobchanges::{self, AddBody, Overview};
use crate::ownership;
use crate::routes::calls::{self, CallRow, LogBody};
use crate::routes::meetings::{self, MeetingRow};
use crate::routes::research;
use crate::routes::sheets::{self, ImportedNow};
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::state::AppState;

/// The pipeline's stages in board order, with how many deals each holds.
pub(in crate::routes) async fn list_stages(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<Stage>>> {
    Ok(Json(deals::stages(&state.db, auth.org_id).await?))
}

/// The organization's deals, the latest first: status is open, won or lost.
pub(in crate::routes) async fn list_deals(
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
pub(in crate::routes) async fn create_deal(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<NewDeal>) -> AppResult<Json<Deal>> {
    Ok(Json(deals::create(&state, auth.org_id, None, "api", body).await?))
}

/// Moves a deal to a stage by stage_id or stage name. A won stage takes the final value, a lost one the reason.
pub(in crate::routes) async fn move_deal(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Move>,
) -> AppResult<Json<Deal>> {
    Ok(Json(deals::move_to(&state, auth.org_id, id, body).await?))
}

/// Open calls and the ones made in the last 14 days, for a dialer or CRM to work from. `status` is open or done, `owner`
/// a member's id or email, or unassigned.
pub(in crate::routes) async fn list_calls(
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
pub(in crate::routes) async fn log_call(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<LogBody>,
) -> AppResult<Json<Value>> {
    Ok(Json(calls::log_for_lead(&state, auth.org_id, id, &body).await?))
}

/// The lead's sales meetings from Fireflies, Fathom, Gong, Zoom or tl;dv, oldest first, with the agent's summary, next steps and
/// suggested stage. Transcripts stay in Splee.
pub(in crate::routes) async fn list_meetings(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<MeetingRow>>> {
    Ok(Json(meetings::of_lead(&state.db, auth.org_id, id).await?))
}

/// The project's Google Sheets: the tabs leads import from and the ones lead events go to, with how each last went.
pub(in crate::routes) async fn list_sheets(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<crate::sheets::Connection>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(crate::sheets::Connection::list(&state.db, id).await?))
}

/// Reads an import tab now and adds its new rows as leads, like "Import now" on the integrations page.
pub(in crate::routes) async fn import_sheet(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path((id, conn)): Path<(Uuid, Uuid)>,
) -> AppResult<Json<ImportedNow>> {
    Ok(Json(sheets::import_for(&state, auth.org_id, id, conn).await?))
}

/// Moves seen on LinkedIn among the people the project watches, newest first, with the lead Splee made at each new company,
/// and how many people it watches.
pub(in crate::routes) async fn list_job_changes(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Overview>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(jobchanges::overview(&state.db, id).await?))
}

/// Funding filings and company news at the project's companies, newest first, like the Companies page's News tab.
pub(in crate::routes) async fn list_signals(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<crate::routes::signals::SignalsQuery>,
) -> AppResult<Json<Vec<crate::signals::Listed>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(crate::routes::signals::listed(&state.db, id, &q).await?))
}

/// The project's research questions and each company's answers, best score first, like the Companies page's Research view.
pub(in crate::routes) async fn list_research(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<research::GridQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<research::Grid>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::grid_page(&state.db, id, &q, &page).await?))
}

pub(in crate::routes) async fn add_research_question(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<research::QuestionBody>,
) -> AppResult<Json<crate::models::ResearchQuestion>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::create(&state.db, id, body).await?))
}

/// Queues the agent to answer the project's research questions for its companies.
pub(in crate::routes) async fn start_research(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<research::StartBody>,
) -> AppResult<Json<research::Started>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(research::start_research(&state.db, id, auth.org_id, body).await?))
}

/// Adds past customers or champions to the project's watch list, from a list or a CSV, like the Job changes tab.
pub(in crate::routes) async fn add_to_watch_list(
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
pub(in crate::routes) async fn find_email(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Person>,
) -> AppResult<Json<Found>> {
    check_project(&state.db, id, auth.org_id).await?;
    let person = body.checked().map_err(AppError::BadRequest)?;
    email_lookups::check_connected(&state.db, id).await?;
    email_lookups::claim_one(&state.db, auth.org_id).await?;
    Ok(Json(email_lookups::find(&state, id, &person).await))
}

/// Finds the mobile of the person behind a LinkedIn profile with the project's finders that return mobiles, without adding
/// them as a lead. Each call counts against the organization's hourly email searches.
pub(in crate::routes) async fn find_phone(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<PhoneQuery>,
) -> AppResult<Json<PhoneFound>> {
    check_project(&state.db, id, auth.org_id).await?;
    let query = body.checked().map_err(AppError::BadRequest)?;
    email_lookups::check_phone_finder(&state.db, id).await?;
    email_lookups::claim_one(&state.db, auth.org_id).await?;
    Ok(Json(email_lookups::find_phone(&state, auth.org_id, id, &query).await))
}

#[derive(Deserialize)]
pub(in crate::routes) struct BatchBody {
    people: Vec<BatchPerson>,
}

/// Queues up to 500 people for the same search, in the background and within the hourly limit, for GET
/// /v1/enrich/email/batch/{task_id} to follow.
pub(in crate::routes) async fn find_emails(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<BatchBody>,
) -> AppResult<Json<Value>> {
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

pub(in crate::routes) async fn email_batch(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Batch>> {
    Ok(Json(email_lookups::load(&state.db, auth.org_id, id).await?))
}

/// The organization's last 30 data exports, newest first, with the files each wrote. See exports/.
pub(in crate::routes) async fn list_exports(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<crate::exports::Run>>> {
    Ok(Json(crate::exports::recent(&state.db, auth.org_id, 30).await?))
}
