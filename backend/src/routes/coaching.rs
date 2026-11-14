//! The Coaching page: every coached meeting and call of the last 90 days, newest first. See agent::coaching.

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::AppResult;
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/coaching", get(list))
}

/// `status` is ready, writing or failed. `kind` is meeting or call.
#[derive(Serialize, sqlx::FromRow)]
struct Item {
    id: Uuid,
    kind: String,
    title: String,
    happened_at: Ts,
    duration_secs: Option<i32>,
    lead_id: Uuid,
    lead_name: String,
    company_name: String,
    owner_id: Option<Uuid>,
    owner_name: Option<String>,
    talk_share: Option<i32>,
    questions: i32,
    coaching: Option<Value>,
    status: String,
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    items: Vec<Item>,
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select coach_calls from project_settings where project_id = $1), true)").bind(id).fetch_one(&state.db).await?;
    let items = sqlx::query_as::<_, Item>(
        "select c.id, case when c.meeting_id is null then 'call' else 'meeting' end as kind,
                coalesce(m.title, '') as title, coalesce(m.started_at, k.called_at, k.created_at) as happened_at,
                coalesce(m.duration_secs, k.duration_secs) as duration_secs,
                l.id as lead_id, l.full_name as lead_name, l.company_name, c.owner_id, nullif(u.name, '') as owner_name,
                c.talk_share, c.questions, c.coaching,
                case when c.coaching is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from call_coaching c join leads l on l.id = c.lead_id
         left join meetings m on m.id = c.meeting_id left join calls k on k.id = c.call_id
         left join users u on u.id = c.owner_id left join agent_runs r on r.id = c.run_id
         where c.project_id = $1 and c.created_at > now() - interval '90 days'
         order by happened_at desc limit 300",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, items }))
}
