use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use super::segments::{self, SegmentWithStats};
use crate::auth::Auth;
use crate::error::AppResult;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/stats", get(project_stats))
        .route("/public/stats", get(public_stats))
}

#[derive(Deserialize)]
struct StatsQuery {
    days: Option<i32>,
}

#[derive(Serialize, FromRow)]
struct Totals {
    leads: i64,
    drafts: i64,
    scheduled: i64,
    sent: i64,
    replies: i64,
    hot: i64,
    meetings: i64,
    ai_cost_micros: i64,
    email_cost_micros: i64,
    #[sqlx(skip)]
    spend_micros: i64,
    #[sqlx(skip)]
    cost_per_hot_micros: Option<i64>,
}

#[derive(Serialize, FromRow)]
struct Day {
    date: String,
    sent: i64,
    replies: i64,
    hot: i64,
}

#[derive(Serialize)]
struct ProjectStats {
    totals: Totals,
    daily: Vec<Day>,
    segments: Vec<SegmentWithStats>,
}

async fn project_stats(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
) -> AppResult<Json<ProjectStats>> {
    auth.project(&state.db, id).await?;
    let days = q.days.unwrap_or(30).clamp(1, 365);

    let mut totals = sqlx::query_as::<_, Totals>(
        "select
           (select count(*) from leads where project_id = $1) as leads,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'draft') as drafts,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status in ('approved', 'scheduled')) as scheduled,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'sent') as sent,
           (select count(*) from leads l where l.project_id = $1
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound')) as replies,
           (select count(*) from leads where project_id = $1 and status in ('hot', 'meeting_booked')) as hot,
           (select count(*) from leads where project_id = $1 and status = 'meeting_booked') as meetings,
           (select (coalesce(sum(cost_usd), 0) * 1000000)::bigint from agent_runs where project_id = $1) as ai_cost_micros,
           (select coalesce(sum(cost_micros), 0)::bigint from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'sent') as email_cost_micros",
    )
    .bind(id)
    .fetch_one(&state.db)
    .await?;
    totals.spend_micros = totals.ai_cost_micros + totals.email_cost_micros;
    totals.cost_per_hot_micros = (totals.hot > 0).then(|| totals.spend_micros / totals.hot);

    // Days are UTC. A reply or hot lead is counted on the day its inbound message arrived.
    let daily = sqlx::query_as::<_, Day>(
        "select to_char(local_d, 'YYYY-MM-DD') as date,
           (select count(*) from email_messages e
             where e.project_id = $1 and e.direction = 'outbound' and e.status = 'sent'
               and e.sent_at >= d and e.sent_at < d + interval '1 day') as sent,
           (select count(distinct e.lead_id) from email_messages e
             where e.project_id = $1 and e.direction = 'inbound'
               and e.created_at >= d and e.created_at < d + interval '1 day') as replies,
           (select count(distinct e.lead_id) from email_messages e
             where e.project_id = $1 and e.direction = 'inbound' and e.classification = 'hot'
               and e.created_at >= d and e.created_at < d + interval '1 day') as hot
         from generate_series(
           date_trunc('day', now() at time zone 'utc') - make_interval(days => $2 - 1),
           date_trunc('day', now() at time zone 'utc'),
           interval '1 day'
         ) as g(local_d), lateral (select local_d at time zone 'utc' as d) as day
         order by local_d",
    )
    .bind(id)
    .bind(days)
    .fetch_all(&state.db)
    .await?;

    Ok(Json(ProjectStats { totals, daily, segments: segments::with_stats(&state.db, id, None).await? }))
}

async fn public_stats(State(state): State<AppState>) -> AppResult<Json<Value>> {
    let (hot_leads, companies): (i64, i64) = sqlx::query_as(
        "select (select count(*) from leads where status in ('hot', 'meeting_booked')),
                (select count(*) from organizations)",
    )
    .fetch_one(&state.db)
    .await?;
    Ok(Json(json!({ "hot_leads": hot_leads, "companies": companies })))
}
