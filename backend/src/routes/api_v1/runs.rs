use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::ApiKeyAuth;
use crate::error::AppResult;
use crate::models::{AgentRun, Segment, Ts};
use crate::routes::runs;
use crate::routes::segments::{self, FindPeopleBody, WriteEmailsBody};
use crate::routes::util_input;
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::routes::util_scope;
use crate::state::AppState;

pub(in crate::routes) async fn find_people(
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
pub(in crate::routes) async fn write_emails(
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
pub(in crate::routes) struct RunStatus {
    #[serde(flatten)]
    run: AgentRun,
    /// The latest progress line, or why it failed.
    message: Option<String>,
}

/// For waiting on a run: status goes queued, running, then succeeded, failed or cancelled.
pub(in crate::routes) async fn get_run(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<RunStatus>> {
    let run = runs::load(&state.db, auth.org_id, id).await?;
    let message = runs::last_message(&state.db, id).await?;
    Ok(Json(RunStatus { run, message }))
}

/// Work that goes on in the background after a request returns: an agent run, a find-email batch or a campaign import.
#[derive(Serialize, FromRow)]
pub(in crate::routes) struct Task {
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
pub(in crate::routes) struct TasksQuery {
    status: Option<String>,
    #[serde(rename = "type")]
    task_type: Option<String>,
    /// A run kind, since every reply's classify_reply run would bury the ones an agent started.
    kind: Option<String>,
    #[serde(default, deserialize_with = "util_input::blank_as_none")]
    project_id: Option<Uuid>,
}

/// The organization's background work of every kind, newest first, so a lost task id can be found again.
pub(in crate::routes) async fn list_tasks(
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
