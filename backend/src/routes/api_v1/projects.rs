use axum::Json;
use axum::extract::{Path, State};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::{check_project, strict};
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::models::{Project, ProjectSettings};
use crate::monthly_cap::{self, Cap};
use crate::routes::my_copy::{self, ProjectCopy};
use crate::routes::projects;
use crate::routes::settings;
use crate::state::AppState;

#[derive(Serialize, FromRow)]
pub(in crate::routes) struct ApiProject {
    #[serde(flatten)]
    #[sqlx(flatten)]
    project: Project,
    /// Hours a draft waits for review before it goes out on its own, or null when drafts wait for a person.
    review_window_hours: Option<i32>,
}

pub(in crate::routes) async fn list_projects(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<ApiProject>>> {
    let projects = sqlx::query_as::<_, ApiProject>(
        "select p.*, s.review_window_hours from projects p left join project_settings s on s.project_id = p.id
         where p.org_id = $1 order by p.created_at desc",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(projects))
}

pub(in crate::routes) async fn project_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(my_copy::project(&state.db, auth.org_id, id).await?))
}

pub(in crate::routes) async fn update_project_my_copy(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<Value>,
) -> AppResult<Json<ProjectCopy>> {
    Ok(Json(my_copy::update_project(&state.db, auth.org_id, id, strict(body)?).await?))
}

/// A project's sending controls, as PATCH /v1/projects/{id} takes them.
#[derive(Serialize)]
pub(in crate::routes) struct ProjectControls {
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
    /// A first email to a lead on Google or Microsoft mail goes out from a mailbox on the same provider when one has room.
    match_provider: bool,
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
            match_provider: s.match_provider,
            sender_name: s.sender_name,
        }
    }
}

/// A project as GET /v1/projects lists it, with its controls.
#[derive(Serialize)]
pub(in crate::routes) struct ProjectWithControls {
    #[serde(flatten)]
    project: Project,
    controls: ProjectControls,
}

/// The organization's project, else a 404.
async fn load_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<Project> {
    let project = sqlx::query_as::<_, Project>("select * from projects where id = $1 and org_id = $2").bind(id).bind(org_id);
    project.fetch_optional(db).await?.ok_or(AppError::NotFound)
}

pub(in crate::routes) async fn get_project(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<ProjectWithControls>> {
    let project = load_project(&state.db, id, auth.org_id).await?;
    Ok(Json(ProjectWithControls { project, controls: settings::load_or_create(&state.db, id).await?.into() }))
}

/// The app's New project: Splee researches the business from its website or description, then suggests campaigns. Poll run_id
/// with GET /v1/runs/{id}.
pub(in crate::routes) async fn create_project(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<projects::CreateBody>,
) -> AppResult<Json<Value>> {
    let (project, run_id) = projects::insert(&state.db, auth.org_id, auth.user_id, body).await?;
    Ok(Json(json!({ "project_id": project.id, "run_id": run_id })))
}

/// The cap PATCH /v1/projects/{id} sets as monthly_cap_micros, and what the project's emails cost since the 1st.
pub(in crate::routes) async fn project_monthly_cap(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Cap>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(monthly_cap::status(&state.db, id, chrono::Utc::now()).await?))
}

/// The project's sending controls, with the settings page's checks, and its Start and Pause sending buttons as `sending`.
/// An empty body changes nothing and returns them.
pub(in crate::routes) async fn update_project(
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
