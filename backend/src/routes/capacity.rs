//! Capacity plan: whether the team can book its monthly target over the next 12 months, and when each rep it still needs
//! has to start. The assumptions come from the organization's last year of deals in its currency, and owners and admins
//! can change any of them to see what happens. A rep is a member with a monthly target or a deal opened in the last 180
//! days, and a deal is theirs when they own its lead, like Targets. Teammates who joined in the organization's first 30
//! days were on the team before it moved to Splee, so they count as at full speed.
//!
//! The model: a rep at full speed opens `deals_per_rep` deals a month, which win at `win_rate` for `deal_cents` each and
//! close `cycle_days` later. A new rep opens none in their first month and climbs in a straight line to full speed after
//! `ramp_months`. The target grows `growth_pct` a month, which is what spreads hires out.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{Datelike, Months, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/capacity", get(capacity))
}

const HORIZON: u32 = 12;
const DAYS_PER_MONTH: f64 = 30.44;
/// Enough to stop a plan with impossible assumptions, far past any real team.
const MAX_HIRES: usize = 100;
/// Closed deals before the win rate is the team's own rather than a starting guess.
const MIN_CLOSED: i64 = 5;
/// The tenure of someone who was on the team before it moved to Splee, past any ramp.
const FULL_SPEED: f64 = 120.0;

/// What the plan assumes. In history, a field is null when there's too little to tell.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Assumptions {
    pub win_rate: Option<f64>,
    pub deal_cents: Option<i64>,
    pub cycle_days: Option<i32>,
    pub deals_per_rep: Option<f64>,
    pub ramp_months: Option<i32>,
    pub target_cents: Option<i64>,
    /// How much the monthly target grows each month. Never from history.
    pub growth_pct: Option<f64>,
}

impl Assumptions {
    /// Each change over history, and a starting guess where neither has one. Without an average deal there's nothing to
    /// plan, so it stays 0.
    fn over(&self, history: &Assumptions) -> Used {
        Used {
            win_rate: self.win_rate.or(history.win_rate).unwrap_or(0.2).clamp(0.0, 1.0),
            deal_cents: self.deal_cents.or(history.deal_cents).unwrap_or(0).max(0),
            cycle_days: self.cycle_days.or(history.cycle_days).unwrap_or(45).clamp(0, 730),
            deals_per_rep: self.deals_per_rep.or(history.deals_per_rep).unwrap_or(4.0).clamp(0.0, 1000.0),
            ramp_months: self.ramp_months.or(history.ramp_months).unwrap_or(3).clamp(0, 24),
            target_cents: self.target_cents.or(history.target_cents).unwrap_or(0).max(0),
            growth_pct: self.growth_pct.unwrap_or(0.0).clamp(-50.0, 100.0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Used {
    pub win_rate: f64,
    pub deal_cents: i64,
    pub cycle_days: i32,
    pub deals_per_rep: f64,
    pub ramp_months: i32,
    pub target_cents: i64,
    pub growth_pct: f64,
}

impl Used {
    /// What a rep at full speed books a month.
    fn per_rep(&self) -> f64 {
        self.deals_per_rep * self.win_rate * self.deal_cents as f64
    }

    /// Whole months between opening a deal and winning it.
    fn lag(&self) -> i32 {
        (self.cycle_days as f64 / DAYS_PER_MONTH).round() as i32
    }

    /// The target `m` months from now.
    fn target(&self, m: i32) -> f64 {
        self.target_cents as f64 * (1.0 + self.growth_pct / 100.0).powi(m)
    }

    /// How close to full speed a rep is `months` after starting, from 0 to 1.
    fn speed(&self, months: f64) -> f64 {
        if months < 0.0 {
            0.0
        } else if self.ramp_months == 0 {
            1.0
        } else {
            (months / self.ramp_months as f64).min(1.0)
        }
    }
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Month {
    /// Its first day.
    pub month: NaiveDate,
    pub target_cents: i64,
    /// What the team you have books.
    pub team_cents: i64,
    /// What the hires add.
    pub hires_cents: i64,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Hire {
    /// The latest they can start and still be at full speed when they're first needed, or today when that's already past.
    pub start_by: NaiveDate,
    pub full_speed: NaiveDate,
    /// The month their first deals close.
    pub closing_from: NaiveDate,
}

#[derive(Debug, PartialEq, Serialize)]
pub struct Plan {
    pub per_rep_cents: i64,
    pub months: Vec<Month>,
    pub hires: Vec<Hire>,
    /// Months under the target even with the hires: too soon for anyone starting today, or a dip while a teammate ramps
    /// that's gone by the end of the plan.
    pub short: Vec<NaiveDate>,
}

fn add_months(d: NaiveDate, n: i32) -> NaiveDate {
    d.checked_add_months(Months::new(n.max(0) as u32)).unwrap_or(d)
}

/// The next 12 months for reps who've been on the team `tenures` months as of `today`, hiring, month by month, just enough
/// reps to book each month's target. A hire starts as late as they can and still be at full speed that month, or today.
/// Only a gap still there in the last month gets a hire, so a dip while someone ramps doesn't. Month 0 is this month,
/// which the plan leaves out since it's mostly decided.
pub fn plan(a: &Used, tenures: &[f64], today: NaiveDate) -> Plan {
    let this_month = today.with_day(1).unwrap_or(today);
    let per_rep = a.per_rep();
    let lag = a.lag();
    // Bookings in month m from a rep who was `tenure` months in today, or starts in month -tenure.
    let books = |tenure: f64, m: i32| per_rep * a.speed(tenure + (m - lag) as f64);
    let team = |m: i32| tenures.iter().map(|t| books(*t, m)).sum::<f64>();
    let hired = |starts: &[i32], m: i32| starts.iter().map(|h| books(-*h as f64, m)).sum::<f64>();
    let short = |starts: &[i32], m: i32| team(m) + hired(starts, m) < a.target(m) - 0.5;
    let last = HORIZON as i32;
    let mut starts: Vec<i32> = vec![];
    for m in 1..=last {
        while short(&starts, m) && short(&starts, last) && starts.len() < MAX_HIRES {
            let start = (m - lag - a.ramp_months).max(0);
            if books(-start as f64, m) <= 0.0 {
                break;
            }
            starts.push(start);
        }
    }
    let months = (1..=last)
        .map(|m| Month {
            month: add_months(this_month, m),
            target_cents: a.target(m).round() as i64,
            team_cents: team(m).round() as i64,
            hires_cents: hired(&starts, m).round() as i64,
        })
        .collect();
    let short = (1..=last).filter(|m| short(&starts, *m)).map(|m| add_months(this_month, m)).collect();
    let hires = starts
        .iter()
        .map(|&h| Hire {
            start_by: if h == 0 { today } else { add_months(this_month, h) },
            full_speed: add_months(this_month, h + a.ramp_months),
            closing_from: add_months(this_month, h + lag + 1),
        })
        .collect();
    Plan { per_rep_cents: per_rep.round() as i64, months, hires, short }
}

#[derive(sqlx::FromRow)]
struct Member {
    joined: chrono::DateTime<Utc>,
    founding: bool,
    target_cents: Option<i64>,
    /// Deals opened in the last 90 and 180 days.
    opened: i64,
    recent: i64,
    first_win: Option<chrono::DateTime<Utc>>,
}

fn months_between(from: chrono::DateTime<Utc>, to: chrono::DateTime<Utc>) -> f64 {
    (to - from).num_hours() as f64 / 24.0 / DAYS_PER_MONTH
}

fn median(mut xs: Vec<f64>) -> Option<f64> {
    xs.sort_by(f64::total_cmp);
    let n = xs.len();
    (n > 0).then(|| if n % 2 == 1 { xs[n / 2] } else { (xs[n / 2 - 1] + xs[n / 2]) / 2.0 })
}

/// The assumptions the organization's own last year gives, and its reps' tenure in months.
async fn history(db: &sqlx::PgPool, org_id: Uuid, currency: &str) -> sqlx::Result<(Assumptions, Vec<f64>)> {
    let (won, closed, deal, cycle, first_ever): (i64, i64, Option<f64>, Option<f64>, Option<chrono::DateTime<Utc>>) = sqlx::query_as(
        "select count(*) filter (where status = 'won' and closed_at > now() - interval '365 days'),
                count(*) filter (where closed_at > now() - interval '365 days'),
                avg(value_cents) filter (where status = 'won' and closed_at > now() - interval '365 days')::float8,
                percentile_cont(0.5) within group (order by extract(epoch from closed_at - created_at) / 86400)
                  filter (where status = 'won' and closed_at > now() - interval '365 days'),
                min(closed_at) filter (where status = 'won')
         from deals where org_id = $1 and lower(currency) = lower($2) and status in ('won', 'lost') and closed_at is not null",
    )
    .bind(org_id)
    .bind(currency)
    .fetch_one(db)
    .await?;
    let members = sqlx::query_as::<_, Member>(
        "select m.created_at as joined, m.created_at <= o.created_at + interval '30 days' as founding, m.monthly_target_cents as target_cents,
                count(d.id) filter (where d.created_at > now() - interval '90 days') as opened,
                count(d.id) filter (where d.created_at > now() - interval '180 days') as recent,
                min(d.closed_at) filter (where d.status = 'won') as first_win
         from memberships m join organizations o on o.id = m.org_id
         left join (deals d join leads l on l.id = d.lead_id) on l.owner_id = m.user_id and d.org_id = m.org_id and lower(d.currency) = lower($2)
         where m.org_id = $1 group by m.user_id, m.created_at, o.created_at, m.monthly_target_cents order by m.created_at",
    )
    .bind(org_id)
    .bind(currency)
    .fetch_all(db)
    .await?;
    let now = Utc::now();
    let reps: Vec<&Member> = members.iter().filter(|m| m.target_cents.is_some_and(|t| t > 0) || m.recent > 0).collect();
    let lag = cycle.map(|c| (c / DAYS_PER_MONTH).round()).unwrap_or(0.0);
    // Time to a first win for reps who joined a team that was already winning, less the cycle every deal waits out anyway.
    let ramps = reps
        .iter()
        .filter_map(|m| {
            let first = m.first_win?;
            (first_ever.is_some_and(|f| m.joined > f) && first > m.joined).then(|| months_between(m.joined, first))
        })
        .collect();
    let tenure = |m: &Member| if m.founding { FULL_SPEED } else { months_between(m.joined, now) };
    // Someone who joined last week opened their deals in about a month's time, not a week's.
    let rep_months: f64 = reps.iter().map(|m| tenure(m).clamp(1.0, 3.0)).sum();
    let opened: i64 = reps.iter().map(|m| m.opened).sum();
    let target: i64 = reps.iter().filter_map(|m| m.target_cents).sum();
    let history = Assumptions {
        win_rate: (closed >= MIN_CLOSED).then(|| won as f64 / closed as f64),
        deal_cents: deal.map(|d| d.round() as i64),
        cycle_days: cycle.map(|c| c.round() as i32),
        // No deals opened is a measure once the team has a year's history. Before that it's a team that just started.
        deals_per_rep: (rep_months > 0.0 && (opened > 0 || closed > 0)).then(|| opened as f64 / rep_months),
        ramp_months: median(ramps).map(|r| (r - lag).round().clamp(1.0, 12.0) as i32),
        target_cents: (target > 0).then_some(target),
        growth_pct: None,
    };
    Ok((history, reps.iter().map(|m| tenure(m)).collect()))
}

#[derive(Serialize)]
struct View {
    currency: String,
    reps: usize,
    history: Assumptions,
    used: Used,
    plan: Plan,
}

/// The plan, with any assumption changed by the query. Owners and admins only.
async fn capacity(State(state): State<AppState>, auth: Auth, Query(changes): Query<Assumptions>) -> AppResult<Json<View>> {
    auth.require_admin()?;
    if changes.win_rate.is_some_and(|w| !(0.0..=1.0).contains(&w)) || changes.deals_per_rep.is_some_and(|d| !(0.0..=1000.0).contains(&d)) {
        return Err(AppError::BadRequest("Check the win rate and deals a month".into()));
    }
    let currency: String = sqlx::query_scalar("select currency from organizations where id = $1").bind(auth.org_id).fetch_one(&state.db).await?;
    let (history, tenures) = history(&state.db, auth.org_id, &currency).await?;
    let used = changes.over(&history);
    let plan = plan(&used, &tenures, Utc::now().date_naive());
    Ok(Json(View { currency, reps: tenures.len(), history, used, plan }))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;

    fn used() -> Used {
        // A ramped rep books 4 x 25% x 10,000 = 10,000 a month, a month after opening.
        Used { win_rate: 0.25, deal_cents: 10_000, cycle_days: 30, deals_per_rep: 4.0, ramp_months: 3, target_cents: 30_000, growth_pct: 0.0 }
    }

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn a_team_that_covers_the_target_needs_nobody() {
        let p = plan(&used(), &[12.0, 12.0, 12.0], day(2026, 11, 14));
        assert_eq!(p.per_rep_cents, 10_000);
        assert!(p.hires.is_empty() && p.short.is_empty());
        assert_eq!((p.months[0].month, p.months[11].month), (day(2026, 12, 1), day(2027, 11, 1)));
        assert!(p.months.iter().all(|m| m.team_cents == 30_000 && m.hires_cents == 0));
    }

    #[test]
    fn hires_cover_a_lasting_gap_but_not_the_months_too_soon_or_a_dip_while_someone_ramps() {
        let mut a = used();
        a.target_cents = 40_000;
        let p = plan(&a, &[12.0, 12.0, 12.0], day(2026, 11, 14));
        // A rep starting now opens nothing this month, so nothing they open closes before January, and then only a third
        // of a rep's month. One hire covers the gap for good from March.
        assert_eq!(p.hires, vec![Hire { start_by: day(2026, 11, 14), full_speed: day(2027, 2, 1), closing_from: day(2027, 1, 1) }]);
        assert_eq!(p.short, vec![day(2026, 12, 1), day(2027, 1, 1), day(2027, 2, 1)]);
        assert_eq!((p.months[1].team_cents, p.months[1].hires_cents), (30_000, 3_333));

        // A rep who joined a month ago is a third of the way up when their December deals open, and covers the target alone
        // from February. That's a dip, not a reason to hire.
        a.target_cents = 30_000;
        let p = plan(&a, &[12.0, 12.0, 1.0], day(2026, 11, 14));
        assert_eq!((p.months[0].team_cents, p.months[1].team_cents, p.months[2].team_cents), (23_333, 26_667, 30_000));
        assert!(p.hires.is_empty());
        assert_eq!(p.short, vec![day(2026, 12, 1), day(2027, 1, 1)]);
    }

    #[test]
    fn a_growing_target_spreads_hires_out_to_a_cycle_and_a_ramp_before_each_is_needed() {
        let a = Used { growth_pct: 5.0, ..used() };
        let p = plan(&a, &[12.0, 12.0, 12.0], day(2026, 11, 14));
        // 31,500 in December is already past 30,000. The next gaps open in May (40,203) and October (51,310).
        let starts: Vec<_> = p.hires.iter().map(|h| h.start_by).collect();
        assert_eq!(starts, vec![day(2026, 11, 14), day(2027, 1, 1), day(2027, 6, 1)]);
        assert_eq!(p.short, vec![day(2026, 12, 1)]);
        assert_eq!(p.months[5].target_cents, 40_203);
        assert!(p.months[1..].iter().all(|m| m.team_cents + m.hires_cents >= m.target_cents));
    }

    #[test]
    fn nothing_to_plan_without_an_average_deal() {
        let a = Used { deal_cents: 0, ..used() };
        let p = plan(&a, &[12.0], day(2026, 11, 14));
        assert!(p.hires.is_empty() && p.short.len() == 12);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_assumptions_come_from_the_teams_last_year_and_any_can_be_changed(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        sqlx::query("update organizations set currency = 'usd', created_at = now() - interval '3 years' where id = $1").bind(t.org).execute(&db).await.unwrap();
        // Dana started the organization in Splee, with a target. Sam joined 5 months ago, after the first win, and won 3 months in.
        sqlx::query("update memberships set created_at = now() - interval '3 years', monthly_target_cents = 2000000 where user_id = $1").bind(t.dana).execute(&db).await.unwrap();
        sqlx::query("update memberships set created_at = now() - interval '152 days' where user_id = $1").bind(t.sam).execute(&db).await.unwrap();
        let lead = |owner: Uuid| {
            let db = db.clone();
            async move {
                sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, owner_id) values ($1, 'Lead', $2) returning id")
                    .bind(t.project)
                    .bind(owner)
                    .fetch_one(&db)
                    .await
                    .unwrap()
            }
        };
        let stage: Uuid = sqlx::query_scalar("select stage_id from deals where id = $1").bind(t.deal).fetch_one(&db).await.unwrap();
        // Six closed deals in the last year, 3 won at 10,000 to 20,000 after 20 to 40 days, and one euro deal that doesn't count.
        for (owner, status, cents, currency, opened_days_ago, cycle) in [
            (t.dana, "won", 1_000_000, "usd", 400, 20),
            (t.dana, "won", 1_500_000, "usd", 200, 30),
            (t.sam, "won", 2_000_000, "usd", 101, 40),
            (t.dana, "lost", 500_000, "usd", 120, 30),
            (t.dana, "lost", 500_000, "usd", 60, 10),
            (t.sam, "lost", 500_000, "usd", 40, 10),
            (t.sam, "won", 9_900_000, "eur", 30, 5),
        ] {
            sqlx::query(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, created_at, closed_at)
                 values ($1, $2, $3, $4, $5, $6, $7, now() - make_interval(days => $8), now() - make_interval(days => $8 - $9))",
            )
            .bind(t.org)
            .bind(t.project)
            .bind(lead(owner).await)
            .bind(stage)
            .bind(status)
            .bind(cents)
            .bind(currency)
            .bind(opened_days_ago)
            .bind(cycle)
            .execute(&db)
            .await
            .unwrap();
        }
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |user: Uuid, role: &str| Auth { user_id: user, org_id: t.org, role: role.into(), token_hash: String::new() };
        assert!(capacity(State(state.clone()), auth(t.sam, "member"), Query(Assumptions::default())).await.is_err(), "owners and admins only");

        let Json(v) = capacity(State(state.clone()), auth(t.dana, "owner"), Query(Assumptions::default())).await.unwrap();
        // The first deal won 380 days ago is outside the year but still the team's first win. The fixture's open deal is Sam's.
        let h = &v.history;
        assert_eq!((h.win_rate, h.deal_cents, h.cycle_days, h.target_cents), (Some(0.4), Some(1_750_000), Some(35), Some(2_000_000)));
        assert_eq!(v.reps, 2);
        // Opened in the last 90 days: Dana 1 and Sam 1 plus the fixture's open deal, over 3 months each.
        assert_eq!(h.deals_per_rep, Some(0.5));
        // Sam won 91 days after joining, about three months, less a one-month cycle.
        assert_eq!(h.ramp_months, Some(2));
        assert_eq!(v.plan.months.len(), 12);
        let (_, tenures) = history(&db, t.org, "usd").await.unwrap();
        assert_eq!(tenures.iter().map(|t| t.round()).collect::<Vec<_>>(), vec![FULL_SPEED, 5.0], "Dana was on the team before Splee");

        let changes = Assumptions { deal_cents: Some(5_000_000), target_cents: Some(1), ..Default::default() };
        let Json(v) = capacity(State(state.clone()), auth(t.dana, "owner"), Query(changes)).await.unwrap();
        assert_eq!((v.used.deal_cents, v.used.target_cents, v.used.win_rate), (5_000_000, 1, 0.4));
        assert!(v.plan.hires.is_empty());
        let bad = Assumptions { win_rate: Some(1.5), ..Default::default() };
        assert!(capacity(State(state), auth(t.dana, "owner"), Query(bad)).await.is_err());

        sqlx::query("update deals set created_at = created_at - interval '200 days'").execute(&db).await.unwrap();
        let (h, _) = history(&db, t.org, "usd").await.unwrap();
        assert_eq!(h.deals_per_rep, Some(0.0), "a team with history that opened nothing lately opens nothing");
    }
}
