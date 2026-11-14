//! Commissions: what each teammate earned on the deals they won in a month. A deal is theirs when they own its lead, and only
//! deals in the organization's currency count, like Targets. Deals count in the order they closed, so the part of a deal
//! that takes them past their monthly target earns the accelerator. Past months use today's targets and rates. Owners and
//! admins see everyone's and set the plan, everyone else sees only their own.

use axum::extract::{Path, Query, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/commissions", get(list))
        .route("/commissions/plan", put(set_plan))
        .route("/commissions/{user_id}", put(set_rate))
}

#[derive(Serialize)]
struct Deal {
    deal_id: Uuid,
    name: String,
    company: String,
    closed_at: Ts,
    value_cents: i64,
    earned_cents: i64,
    /// Some of it was past their target, so it earned the accelerator.
    accelerated: bool,
}

#[derive(Serialize)]
struct Row {
    user_id: Uuid,
    name: String,
    target_cents: Option<i64>,
    /// Their rate, or the organization's when they have none of their own. Null is no commission.
    rate_bps: Option<i32>,
    own_rate: bool,
    won_cents: i64,
    earned_cents: i64,
    deals: Vec<Deal>,
}

#[derive(Serialize)]
struct View {
    /// The first of the month.
    month: NaiveDate,
    currency: String,
    commission_bps: Option<i32>,
    accelerator_pct: i32,
    rows: Vec<Row>,
}

#[derive(Deserialize)]
struct MonthQuery {
    /// Like 2026-10. This month by default.
    month: Option<String>,
}

/// Each deal's commission in the order they closed: the part up to the target at the rate, the part past it at the rate
/// times the accelerator. Without a target it's all at the rate.
fn earn(values: &[i64], target: Option<i64>, bps: i32, accelerator_pct: i32) -> Vec<(i64, bool)> {
    let mut won = 0;
    values
        .iter()
        .map(|&v| {
            let base = target.map_or(v, |t| (t - won).clamp(0, v));
            won += v;
            let over = v - base;
            let cents = (i128::from(base) * i128::from(bps) * 100 + i128::from(over) * i128::from(bps) * i128::from(accelerator_pct)) / 1_000_000;
            (cents as i64, over > 0)
        })
        .collect()
}

async fn list(State(state): State<AppState>, auth: Auth, Query(q): Query<MonthQuery>) -> AppResult<Json<View>> {
    let month = match q.month.as_deref() {
        Some(m) => NaiveDate::parse_from_str(&format!("{m}-01"), "%Y-%m-%d").map_err(|_| AppError::BadRequest("Pick a month like 2026-10".into()))?,
        None => Utc::now().date_naive().with_day(1).unwrap_or_default(),
    };
    let (currency, commission_bps, accelerator_pct): (String, Option<i32>, i32) =
        sqlx::query_as("select currency, commission_bps, accelerator_pct from organizations where id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    let only = if auth.require_admin().is_ok() { None } else { Some(auth.user_id) };
    let people: Vec<(Uuid, String, Option<i64>, Option<i32>)> = sqlx::query_as(
        "select m.user_id, coalesce(nullif(u.name, ''), u.email::text), m.monthly_target_cents, m.commission_bps
         from memberships m join users u on u.id = m.user_id where m.org_id = $1 and ($2::uuid is null or m.user_id = $2)",
    )
    .bind(auth.org_id)
    .bind(only)
    .fetch_all(&state.db)
    .await?;
    let won: Vec<(Uuid, Uuid, String, String, Ts, i64)> = sqlx::query_as(
        "select l.owner_id, d.id, l.full_name, l.company_name, d.closed_at, d.value_cents
         from deals d join leads l on l.id = d.lead_id
         where d.org_id = $1 and d.status = 'won' and lower(d.currency) = lower($2) and l.owner_id is not null
           and d.closed_at >= $3::date::timestamp at time zone 'utc' and d.closed_at < ($3::date + interval '1 month') at time zone 'utc'
         order by d.closed_at, d.id",
    )
    .bind(auth.org_id)
    .bind(&currency)
    .bind(month)
    .fetch_all(&state.db)
    .await?;

    let mut rows: Vec<Row> = people
        .into_iter()
        .map(|(user_id, name, target_cents, own)| {
            let rate_bps = own.or(commission_bps);
            let mine: Vec<_> = won.iter().filter(|w| w.0 == user_id).collect();
            let values: Vec<i64> = mine.iter().map(|w| w.5).collect();
            let earned = rate_bps.map(|bps| earn(&values, target_cents, bps, accelerator_pct)).unwrap_or_else(|| vec![(0, false); values.len()]);
            let deals: Vec<Deal> = mine
                .iter()
                .zip(earned)
                .map(|(w, (earned_cents, accelerated))| Deal {
                    deal_id: w.1,
                    name: w.2.clone(),
                    company: w.3.clone(),
                    closed_at: w.4,
                    value_cents: w.5,
                    earned_cents,
                    accelerated,
                })
                .collect();
            Row {
                user_id,
                name,
                target_cents,
                rate_bps,
                own_rate: own.is_some(),
                won_cents: values.iter().sum(),
                earned_cents: deals.iter().map(|d| d.earned_cents).sum(),
                deals,
            }
        })
        .collect();
    rows.sort_by(|a, b| b.earned_cents.cmp(&a.earned_cents).then(b.won_cents.cmp(&a.won_cents)).then(a.name.cmp(&b.name)));
    Ok(Json(View { month, currency, commission_bps, accelerator_pct, rows }))
}

fn rate(bps: Option<i32>) -> AppResult<Option<i32>> {
    if bps.is_some_and(|b| !(0..=10_000).contains(&b)) {
        return Err(AppError::BadRequest("A rate is between 0% and 100%".into()));
    }
    Ok(bps)
}

#[derive(Deserialize, Serialize)]
struct Plan {
    /// Null turns commissions off.
    commission_bps: Option<i32>,
    accelerator_pct: i32,
}

/// Sets the team's rate and accelerator. Owners and admins only.
async fn set_plan(State(state): State<AppState>, auth: Auth, Json(body): Json<Plan>) -> AppResult<Json<Plan>> {
    auth.require_admin()?;
    let bps = rate(body.commission_bps)?;
    if !(100..=500).contains(&body.accelerator_pct) {
        return Err(AppError::BadRequest("The accelerator is between 1× and 5×".into()));
    }
    sqlx::query("update organizations set commission_bps = $2, accelerator_pct = $3 where id = $1")
        .bind(auth.org_id)
        .bind(bps)
        .bind(body.accelerator_pct)
        .execute(&state.db)
        .await?;
    Ok(Json(Plan { commission_bps: bps, accelerator_pct: body.accelerator_pct }))
}

#[derive(Deserialize, Serialize)]
struct Rate {
    /// Null uses the team's.
    commission_bps: Option<i32>,
}

/// Gives a teammate their own rate, or puts them back on the team's. Owners and admins only.
async fn set_rate(State(state): State<AppState>, auth: Auth, Path(user_id): Path<Uuid>, Json(body): Json<Rate>) -> AppResult<Json<Rate>> {
    auth.require_admin()?;
    let bps = rate(body.commission_bps)?;
    let done = sqlx::query("update memberships set commission_bps = $3 where org_id = $1 and user_id = $2")
        .bind(auth.org_id)
        .bind(user_id)
        .bind(bps)
        .execute(&state.db)
        .await?;
    if done.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(Rate { commission_bps: bps }))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    #[test]
    fn the_part_past_the_target_earns_the_accelerator() {
        // 10% with 1.5× past a 10,000 target: 6,000 at 10%, then 4,000 at 10% and 2,000 at 15%, then 5,000 at 15%.
        assert_eq!(earn(&[600_000, 600_000, 500_000], Some(1_000_000), 1000, 150), vec![(60_000, false), (70_000, true), (75_000, true)]);
        assert_eq!(earn(&[600_000, 600_000], None, 1000, 150), vec![(60_000, false), (60_000, false)], "no target, no accelerator");
        assert_eq!(earn(&[333], Some(1_000_000), 250, 100), vec![(8, false)], "rounds down to the cent");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_month_counts_each_teammates_wins_at_their_rate_and_members_see_only_their_own(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        sqlx::query("update organizations set currency = 'usd', commission_bps = 1000, accelerator_pct = 200 where id = $1").bind(t.org).execute(&db).await.unwrap();
        sqlx::query("update memberships set monthly_target_cents = 500000 where user_id = $1").bind(t.sam).execute(&db).await.unwrap();
        sqlx::query("update memberships set commission_bps = 500 where user_id = $1").bind(t.dana).execute(&db).await.unwrap();
        // Sam won the fixture's 4,800 deal and a 3,000 one in October, and one in November that doesn't count. Dana won 2,000 in
        // October, and 9,000 in euros that doesn't count.
        let project = t.project;
        let deal = |owner: Uuid, cents: i64, currency: &'static str, closed: &'static str| {
            let db = db.clone();
            async move {
                let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_name, owner_id) values ($1, 'Lead', 'Co', $2) returning id")
                    .bind(project)
                    .bind(owner)
                    .fetch_one(&db)
                    .await
                    .unwrap();
                sqlx::query(
                    "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
                     select org_id, $1, $2, (select id from deal_stages where org_id = p.org_id limit 1), 'won', $3, $4, $5::timestamptz from projects p where p.id = $1",
                )
                .bind(project)
                .bind(lead)
                .bind(cents)
                .bind(currency)
                .bind(closed)
                .execute(&db)
                .await
                .unwrap();
            }
        };
        sqlx::query("update deals set status = 'won', closed_at = '2026-10-03T09:00:00Z' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        deal(t.sam, 300_000, "usd", "2026-10-20T09:00:00Z").await;
        deal(t.sam, 100_000, "usd", "2026-11-01T00:00:00Z").await;
        deal(t.dana, 200_000, "usd", "2026-10-31T23:59:00Z").await;
        deal(t.dana, 900_000, "eur", "2026-10-10T09:00:00Z").await;

        let state = crate::autopilot::tests::state(db);
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: t.org, role: role.into(), token_hash: String::new() };
        let october = || Query(MonthQuery { month: Some("2026-10".into()) });
        let v = list(State(state.clone()), auth(t.dana, "owner"), october()).await.unwrap().0;
        let sam = v.rows.iter().find(|r| r.user_id == t.sam).unwrap();
        // 4,800 then 3,000 past a 5,000 target at 10% with 2×: 480, then 20 + 560.
        assert_eq!((sam.won_cents, sam.earned_cents, sam.rate_bps, sam.own_rate), (780_000, 106_000, Some(1000), false));
        assert_eq!(sam.deals.iter().map(|d| (d.earned_cents, d.accelerated)).collect::<Vec<_>>(), vec![(48_000, false), (58_000, true)]);
        let dana = v.rows.iter().find(|r| r.user_id == t.dana).unwrap();
        assert_eq!((dana.won_cents, dana.earned_cents, dana.own_rate), (200_000, 10_000, true), "Dana's own 5%, no target");

        let mine = list(State(state.clone()), auth(t.sam, "member"), october()).await.unwrap().0;
        assert_eq!(mine.rows.iter().map(|r| r.user_id).collect::<Vec<_>>(), vec![t.sam]);
        assert!(matches!(set_rate(State(state.clone()), auth(t.sam, "member"), Path(t.sam), Json(Rate { commission_bps: Some(9000) })).await, Err(AppError::Forbidden(_))));
        assert!(matches!(list(State(state), auth(t.sam, "member"), Query(MonthQuery { month: Some("October".into()) })).await, Err(AppError::BadRequest(_))));
    }
}
