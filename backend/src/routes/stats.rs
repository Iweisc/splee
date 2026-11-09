use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::segments::{self, SegmentWithStats};
use crate::auth::Auth;
use crate::error::AppResult;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/stats", get(project_stats))
        .route("/projects/{id}/stats/steps", get(step_stats))
        .route("/projects/{id}/stats/weekdays", get(weekday_stats))
        .route("/public/stats", get(public_stats))
}

#[derive(Deserialize)]
struct StatsQuery {
    days: Option<i32>,
}

#[derive(Serialize, FromRow)]
pub(super) struct Totals {
    leads: i64,
    drafts: i64,
    scheduled: i64,
    sent: i64,
    contacted: i64,
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

/// Lifetime funnel and cost of a project. Spend is real cost: Claude's reported cost per agent run plus each sent email's mailbox price.
pub(super) async fn totals(db: &PgPool, id: Uuid) -> sqlx::Result<Totals> {
    let mut totals = sqlx::query_as::<_, Totals>(
        "select
           (select count(*) from leads where project_id = $1) as leads,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'draft') as drafts,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status in ('approved', 'scheduled')) as scheduled,
           (select count(*) from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'sent') as sent,
           -- Leads with at least one email sent. The sender sets last_contacted_at with every send.
           (select count(*) from leads where project_id = $1 and last_contacted_at is not null) as contacted,
           (select count(*) from leads l where l.project_id = $1
             and exists (select 1 from email_messages e where e.lead_id = l.id and e.direction = 'inbound'
                         and e.classification is distinct from 'bounce')) as replies,
           (select count(*) from leads where project_id = $1 and status in ('hot', 'meeting_booked')) as hot,
           (select count(*) from leads where project_id = $1 and status = 'meeting_booked') as meetings,
           (select (coalesce(sum(cost_usd), 0) * 1000000)::bigint from agent_runs where project_id = $1) as ai_cost_micros,
           (select coalesce(sum(cost_micros), 0)::bigint from email_messages
             where project_id = $1 and direction = 'outbound' and status = 'sent') as email_cost_micros",
    )
    .bind(id)
    .fetch_one(db)
    .await?;
    totals.spend_micros = totals.ai_cost_micros + totals.email_cost_micros;
    totals.cost_per_hot_micros = (totals.hot > 0).then(|| totals.spend_micros / totals.hot);
    Ok(totals)
}

/// Sends, replies and hot replies per UTC day for the last `days` days, oldest first, with empty days included.
async fn daily(db: &PgPool, id: Uuid, days: i32) -> sqlx::Result<Vec<Day>> {
    // Days are UTC. A reply or hot lead is counted on the day its inbound message arrived. One grouped pass over the
    // window's emails rather than three lookups per day, which took 430 ms on a 50k-lead project.
    sqlx::query_as::<_, Day>(
        "with days as (
           select g as d from generate_series(
             date_trunc('day', now() at time zone 'utc') - make_interval(days => $2 - 1),
             date_trunc('day', now() at time zone 'utc'),
             interval '1 day'
           ) as g
         ), sent as (
           select date_trunc('day', e.sent_at at time zone 'utc') as d, count(*) as sent
           from email_messages e
           where e.project_id = $1 and e.direction = 'outbound' and e.status = 'sent'
             and e.sent_at >= (select min(d) from days) at time zone 'utc'
           group by 1
         ), replies as (
           select date_trunc('day', e.created_at at time zone 'utc') as d,
             count(distinct e.lead_id) as replies,
             count(distinct e.lead_id) filter (where e.classification = 'hot') as hot
           from email_messages e
           where e.project_id = $1 and e.direction = 'inbound' and e.classification is distinct from 'bounce'
             and e.created_at >= (select min(d) from days) at time zone 'utc'
           group by 1
         )
         select to_char(days.d, 'YYYY-MM-DD') as date, coalesce(sent.sent, 0) as sent, coalesce(replies.replies, 0) as replies,
           coalesce(replies.hot, 0) as hot
         from days left join sent using (d) left join replies using (d)
         order by days.d",
    )
    .bind(id)
    .bind(days)
    .fetch_all(db)
    .await
}

/// One email of the sequence: step 0 is the first email, step n is follow-up n.
#[derive(Serialize, FromRow)]
pub(super) struct StepStats {
    step: i32,
    sent: i64,
    /// Leads whose first reply came after this step and before the next one.
    replied: i64,
}

/// The first emails and follow-ups sent, and the leads who answered one, grouped by `key`, an expression over the sent
/// email `e` and the project's settings `ps`, returned as the column `name`. A lead's first real reply counts for the last
/// first email or follow-up sent to them before it. Later replies, and bounces, don't count.
fn replies_by(key: &str, name: &str) -> String {
    format!(
        "with sent as (
           select {key} as k, count(*) as sent from email_messages e left join project_settings ps on ps.project_id = e.project_id
           where e.project_id = $1 and e.direction = 'outbound' and e.status = 'sent' and e.kind in ('initial', 'follow_up')
           group by 1
         ), first_reply as (
           select distinct on (lead_id) lead_id, created_at from email_messages
           where project_id = $1 and direction = 'inbound' and classification is distinct from 'bounce'
           order by lead_id, created_at
         ), replied as (
           select o.k, count(*) as replied from first_reply r
           join lateral (select {key} as k from email_messages e left join project_settings ps on ps.project_id = e.project_id
                         where e.lead_id = r.lead_id and e.direction = 'outbound' and e.status = 'sent'
                           and e.kind in ('initial', 'follow_up') and e.sent_at <= r.created_at
                         order by e.sent_at desc limit 1) o on true
           group by o.k
         )
         select s.k as {name}, s.sent, coalesce(r.replied, 0) as replied from sent s left join replied r using (k) order by 1"
    )
}

/// Which email in the sequence leads answer, to judge whether another follow-up is worth sending.
pub(super) async fn steps(db: &PgPool, id: Uuid) -> sqlx::Result<Vec<StepStats>> {
    sqlx::query_as::<_, StepStats>(&replies_by("e.step", "step")).bind(id).fetch_all(db).await
}

/// One day of the week in the project's time zone, 1 for Monday to 7 for Sunday.
#[derive(Serialize, FromRow)]
pub(super) struct WeekdayStats {
    weekday: i32,
    sent: i64,
    /// Leads whose first reply came after an email sent that day, counted for the last email before the reply.
    replied: i64,
}

/// Which send days get answers, next to the days the Schedule page lets people pick. Days use the project's current time
/// zone, the one the send window uses.
pub(super) async fn weekdays(db: &PgPool, id: Uuid) -> sqlx::Result<Vec<WeekdayStats>> {
    let key = "extract(isodow from e.sent_at at time zone coalesce(ps.timezone, 'UTC'))::int";
    sqlx::query_as::<_, WeekdayStats>(&replies_by(key, "weekday")).bind(id).fetch_all(db).await
}

async fn step_stats(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<StepStats>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(steps(&state.db, id).await?))
}

async fn weekday_stats(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<WeekdayStats>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(weekdays(&state.db, id).await?))
}

async fn project_stats(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<StatsQuery>,
) -> AppResult<Json<ProjectStats>> {
    auth.project(&state.db, id).await?;
    let days = q.days.unwrap_or(30).clamp(1, 365);

    let totals = totals(&state.db, id).await?;

    let daily = daily(&state.db, id, days).await?;

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

#[cfg(test)]
pub(crate) mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_reply_counts_for_the_last_step_before_it(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        let lead = || sqlx::query_scalar::<_, Uuid>("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db);
        let (early, late, bounced, twice) = (lead().await.unwrap(), lead().await.unwrap(), lead().await.unwrap(), lead().await.unwrap());
        // (lead, kind, step, days ago, classification). Outbound kinds are sent, inbound ones received.
        let rows: [(Uuid, &str, i32, i32, Option<&str>); 13] = [
            (early, "initial", 0, 5, None),
            (early, "inbound", 0, 4, Some("question")),
            (late, "initial", 0, 5, None),
            (late, "follow_up", 1, 2, None),
            (late, "inbound", 0, 1, Some("hot")),
            (bounced, "initial", 0, 5, None),
            (bounced, "inbound", 0, 4, Some("bounce")),
            (twice, "initial", 0, 5, None),
            (twice, "follow_up", 1, 3, None),
            (twice, "inbound", 0, 2, Some("question")),
            (twice, "manual_reply", 0, 1, None),
            (twice, "inbound", 0, 0, Some("hot")),
            (twice, "follow_up", 2, 0, None),
        ];
        for (lead, kind, step, days, classification) in rows {
            let inbound = kind == "inbound";
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, step, status, classification, sent_at, created_at)
                 values ($1, $2, $3, $4, $5, $6, $7, case when $3 = 'outbound' then now() - make_interval(days => $8) end,
                         now() - make_interval(days => $8))",
            )
            .bind(project)
            .bind(lead)
            .bind(if inbound { "inbound" } else { "outbound" })
            .bind(kind)
            .bind(step)
            .bind(if inbound { "received" } else { "sent" })
            .bind(classification)
            .bind(days)
            .execute(&db)
            .await
            .unwrap();
        }
        let steps: Vec<_> = super::steps(&db, project).await.unwrap().into_iter().map(|s| (s.step, s.sent, s.replied)).collect();
        assert_eq!(steps, vec![(0, 4, 1), (1, 2, 2), (2, 1, 0)]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn weekdays_use_the_project_time_zone(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(&db).await.unwrap();
        sqlx::query("insert into project_settings (project_id, timezone) values ($1, 'Asia/Tokyo')").bind(project).execute(&db).await.unwrap();
        let lead = || sqlx::query_scalar::<_, Uuid>("insert into leads (project_id) values ($1) returning id").bind(project).fetch_one(&db);
        let (answered, quiet) = (lead().await.unwrap(), lead().await.unwrap());
        // Monday 20:00 UTC is Tuesday morning in Tokyo.
        let rows = [
            (answered, "outbound", "initial", "sent", "2026-11-09 20:00Z"),
            (answered, "inbound", "inbound", "received", "2026-11-10 01:00Z"),
            (quiet, "outbound", "initial", "sent", "2026-11-11 03:00Z"),
        ];
        for (lead, direction, kind, status, at) in rows {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, direction, kind, step, status, classification, sent_at, created_at)
                 values ($1, $2, $3, $4, 0, $5, case when $3 = 'inbound' then 'question' end,
                         case when $3 = 'outbound' then $6::timestamptz end, $6::timestamptz)",
            )
            .bind(project)
            .bind(lead)
            .bind(direction)
            .bind(kind)
            .bind(status)
            .bind(at)
            .execute(&db)
            .await
            .unwrap();
        }
        let days: Vec<_> = super::weekdays(&db, project).await.unwrap().into_iter().map(|d| (d.weekday, d.sent, d.replied)).collect();
        assert_eq!(days, vec![(2, 1, 1), (3, 1, 0)]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn daily_counts_each_day_once(db: PgPool) {
        let seeded = seed_costs(&db).await;
        let lead: Uuid = sqlx::query_scalar("select id from leads where project_id = $1 and status = 'hot'")
            .bind(seeded.project)
            .fetch_one(&db)
            .await
            .unwrap();
        // Two replies from one lead today count once. Yesterday gets one send. Two days ago stays empty.
        sqlx::query("update email_messages set sent_at = now() - interval '1 day' where lead_id = $1 and status = 'sent'")
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status, classification) values ($1, $2, 'inbound', 'inbound', 'received', 'hot')")
            .bind(seeded.project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();

        // A bounce notice arrives as an inbound message too, but nobody replied.
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, classification)
             select $1, id, 'inbound', 'inbound', 'received', 'bounce' from leads where project_id = $1 and id <> $2",
        )
        .bind(seeded.project)
        .bind(lead)
        .execute(&db)
        .await
        .unwrap();

        let days = super::daily(&db, seeded.project, 3).await.unwrap();
        let counts: Vec<_> = days.iter().map(|d| (d.sent, d.replies, d.hot)).collect();
        // The other emails seed_costs sends have no sent_at, so they never count.
        assert_eq!(counts, vec![(0, 0, 0), (1, 0, 0), (0, 1, 1)]);
        assert_eq!(totals(&db, seeded.project).await.unwrap().replies, 1);

        sqlx::query("update leads set last_contacted_at = now() where id = $1").bind(lead).execute(&db).await.unwrap();
        assert_eq!(totals(&db, seeded.project).await.unwrap().contacted, 1);
    }

    use super::totals;

    pub(crate) struct Seeded {
        pub project: Uuid,
        pub cheap: Uuid,
        pub pricey: Uuid,
    }

    /// One project, two campaigns. Costs are picked so every sum below can be checked by hand.
    pub(crate) async fn seed_costs(db: &PgPool) -> Seeded {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Test') returning id").bind(org).fetch_one(db).await.unwrap();
        let segment = |name: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into segments (project_id, name) values ($1, $2) returning id").bind(project).bind(name)
        };
        let cheap = segment("Cheap").fetch_one(db).await.unwrap();
        let pricey = segment("Pricey").fetch_one(db).await.unwrap();

        let lead = |segment: Uuid, status: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, segment_id, status) values ($1, $2, $3) returning id")
                .bind(project)
                .bind(segment)
                .bind(status)
        };
        let hot = lead(cheap, "hot").fetch_one(db).await.unwrap();
        let cold = lead(cheap, "contacted").fetch_one(db).await.unwrap();
        let other = lead(pricey, "contacted").fetch_one(db).await.unwrap();

        // Only sent outbound emails cost anything. The approved one carries a cost to prove it isn't counted.
        for (lead, segment, direction, kind, status, cost) in [
            (hot, cheap, "outbound", "initial", "sent", 500),
            (cold, cheap, "outbound", "initial", "sent", 500),
            (other, pricey, "outbound", "initial", "sent", 700),
            (cold, cheap, "outbound", "follow_up", "approved", 999),
            (hot, cheap, "inbound", "inbound", "received", 0),
        ] {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, cost_micros) values ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(project)
            .bind(lead)
            .bind(segment)
            .bind(direction)
            .bind(kind)
            .bind(status)
            .bind(cost as i64)
            .execute(db)
            .await
            .unwrap();
        }

        // The explore run researches the whole project, so it has no segment and only counts in the project's total.
        for (kind, segment, usd) in [("explore", None, 1.0), ("find_people", Some(cheap), 0.25), ("write_emails", Some(pricey), 0.5)] {
            let input = segment.map_or(serde_json::json!({}), |s: Uuid| serde_json::json!({ "segment_id": s }));
            sqlx::query("insert into agent_runs (kind, project_id, org_id, input, status, cost_usd) values ($1, $2, $3, $4, 'succeeded', $5)")
                .bind(kind)
                .bind(project)
                .bind(org)
                .bind(input)
                .bind(usd)
                .execute(db)
                .await
                .unwrap();
        }
        Seeded { project, cheap, pricey }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn spend_is_agent_runs_plus_sent_email_cost(db: PgPool) {
        let seeded = seed_costs(&db).await;
        let t = totals(&db, seeded.project).await.unwrap();
        assert_eq!((t.sent, t.hot), (3, 1));
        assert_eq!(t.ai_cost_micros, 1_750_000);
        assert_eq!(t.email_cost_micros, 1_700);
        assert_eq!(t.spend_micros, 1_751_700);
        assert_eq!(t.cost_per_hot_micros, Some(1_751_700));
    }
}
