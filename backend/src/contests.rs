//! Sales contests: an owner or admin starts one on a metric, with a prize, from now until a date. Everyone in the
//! organization is in it, and their score is what they did between its start and end. Revenue, deals and pipeline count
//! deals by whoever owns the lead, in the organization's currency like Targets. Calls count by whoever made them, and
//! practice calls by whoever scored at least the ramp's pass score. When one ends, the winner is posted to the team's chats.

use std::time::Duration;

use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::ramp::PASS;
use crate::deals::amount;
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

pub const METRICS: [&str; 5] = ["won_value", "deals_won", "pipeline", "calls", "practice"];

#[derive(Debug, Serialize, FromRow)]
pub struct Contest {
    pub id: Uuid,
    pub name: String,
    pub metric: String,
    pub prize: String,
    pub starts_at: Ts,
    pub ends_at: Ts,
    pub created_by: Option<String>,
    #[sqlx(skip)]
    pub standings: Vec<Standing>,
}

/// A teammate's score: cents for won_value and pipeline, a count otherwise.
#[derive(Debug, Serialize, FromRow, PartialEq)]
pub struct Standing {
    pub user_id: Uuid,
    pub name: String,
    pub value: i64,
}

/// Each metric's scores as (user_id, value) between $2 and $3, for organization $1 in currency $4.
fn scores(metric: &str) -> String {
    let deals = |value: &str, at: &str, won: &str| {
        format!(
            "select l.owner_id as user_id, {value} as value from deals d join leads l on l.id = d.lead_id
             where d.org_id = $1 {won} and lower(d.currency) = lower($4) and d.{at} >= $2 and d.{at} < $3 group by 1"
        )
    };
    match metric {
        "won_value" => deals("sum(d.value_cents)", "closed_at", "and d.status = 'won'"),
        "deals_won" => deals("count(*)", "closed_at", "and d.status = 'won'"),
        "pipeline" => deals("sum(d.value_cents)", "created_at", ""),
        "calls" => "select c.called_by as user_id, count(*) as value from calls c join projects p on p.id = c.project_id
                    where p.org_id = $1 and c.status = 'done' and not c.inbound and c.closed_at >= $2 and c.closed_at < $3 group by 1"
            .into(),
        _ => format!(
            "select s.user_id, count(*) as value from practice_sessions s join projects p on p.id = s.project_id
             where p.org_id = $1 and (s.score->>'score')::int >= {PASS} and s.created_at >= $2 and s.created_at < $3 group by 1"
        ),
    }
}

/// Every member's score in the contest, the highest first.
async fn standings(db: &PgPool, org_id: Uuid, currency: &str, c: &Contest) -> sqlx::Result<Vec<Standing>> {
    sqlx::query_as(&format!(
        "with scores as ({}) select m.user_id, coalesce(nullif(u.name, ''), u.email::text) as name, coalesce(s.value, 0)::bigint as value
         from memberships m join users u on u.id = m.user_id left join scores s on s.user_id = m.user_id
         where m.org_id = $1 order by value desc, name",
        scores(&c.metric)
    ))
    .bind(org_id)
    .bind(c.starts_at)
    .bind(c.ends_at)
    .bind(currency)
    .fetch_all(db)
    .await
}

/// The organization's running contests and those that ended in the last 30 days, with their standings. Running ones
/// first, the soonest to end first, then the ones that ended, the latest first.
pub async fn list(db: &PgPool, org_id: Uuid) -> sqlx::Result<(String, Vec<Contest>)> {
    let currency: String = sqlx::query_scalar("select currency from organizations where id = $1").bind(org_id).fetch_one(db).await?;
    let mut contests: Vec<Contest> = sqlx::query_as(
        "select c.id, c.name, c.metric, c.prize, c.starts_at, c.ends_at, coalesce(nullif(u.name, ''), u.email::text) as created_by
         from contests c left join users u on u.id = c.created_by
         where c.org_id = $1 and c.ends_at > now() - interval '30 days'
         order by c.ends_at <= now(), case when c.ends_at > now() then c.ends_at end, c.ends_at desc",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    for c in &mut contests {
        c.standings = standings(db, org_id, &currency, c).await?;
    }
    Ok((currency, contests))
}

/// "$12,400 won" or "3 calls", a score as the chats say it.
fn score(metric: &str, value: i64, currency: &str) -> String {
    let cur = currency.to_uppercase();
    let n = |one: &str, many: &str| format!("{value} {}", if value == 1 { one } else { many });
    match metric {
        "won_value" => format!("{} {cur} won", amount(value)),
        "pipeline" => format!("{} {cur} of new pipeline", amount(value)),
        "deals_won" => n("deal won", "deals won"),
        "calls" => n("call", "calls"),
        _ => n("practice call passed", "practice calls passed"),
    }
}

/// The winners, everyone tied at the top with more than nothing.
fn winners(standings: &[Standing]) -> Vec<&Standing> {
    let top = standings.first().map_or(0, |s| s.value);
    standings.iter().filter(|s| top > 0 && s.value == top).collect()
}

/// What the chats say when a contest ends.
fn result_line(c: &Contest, currency: &str) -> String {
    let won = winners(&c.standings);
    let prize = if c.prize.is_empty() { String::new() } else { format!(" They win {}.", c.prize) };
    match won.as_slice() {
        [] => format!("{} is over, and nobody scored.", c.name),
        [w] => format!("{} won {} with {}.{prize}", w.name, c.name, score(&c.metric, w.value, currency)),
        many => {
            let names: Vec<&str> = many.iter().map(|w| w.name.as_str()).collect();
            format!("{} tied in {} with {} each.{prize}", names.join(" and "), c.name, score(&c.metric, many[0].value, currency))
        }
    }
}

/// Posts to the organization's chats, linking to the Targets tab.
pub async fn post(state: &AppState, org_id: Uuid, label: &str, summary: &str) {
    let link = format!("{}/app/pipeline?view=targets", state.cfg.public_url.trim_end_matches('/'));
    hooks::post_org_chat(&state.db, org_id, &ChatAlert { label, summary, quote: None, link: &link, link_text: "See the leaderboard" }).await;
}

/// Claims each contest that ended and wasn't announced, and posts its winner.
pub async fn announce_ended(state: &AppState) -> anyhow::Result<()> {
    let ended: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "update contests set announced_at = now()
         where id in (select id from contests where announced_at is null and ends_at <= now() for update skip locked)
         returning id, org_id",
    )
    .fetch_all(&state.db)
    .await?;
    for (id, org_id) in ended {
        let (currency, contests) = list(&state.db, org_id).await?;
        if let Some(c) = contests.iter().find(|c| c.id == id) {
            post(state, org_id, "Contest over", &result_line(c, &currency)).await;
        }
    }
    Ok(())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(300), || announce_ended(&state), "contests").await });
}

#[cfg(test)]
mod tests {
    use chrono::Utc;

    use super::*;

    fn contest(metric: &str, prize: &str, standings: Vec<(&str, i64)>) -> Contest {
        Contest {
            id: Uuid::nil(),
            name: "November push".into(),
            metric: metric.into(),
            prize: prize.into(),
            starts_at: Utc::now(),
            ends_at: Utc::now(),
            created_by: None,
            standings: standings.into_iter().map(|(name, value)| Standing { user_id: Uuid::new_v4(), name: name.into(), value }).collect(),
        }
    }

    #[test]
    fn the_result_names_the_winner_ties_and_nobody() {
        let one = contest("won_value", "a day off", vec![("Sam Ito", 1_240_000), ("Dana Reyes", 980_000)]);
        assert_eq!(result_line(&one, "usd"), "Sam Ito won November push with 12,400 USD won. They win a day off.");
        let tie = contest("calls", "", vec![("Sam Ito", 3), ("Dana Reyes", 3), ("Theo Marsh", 1)]);
        assert_eq!(result_line(&tie, "usd"), "Sam Ito and Dana Reyes tied in November push with 3 calls each.");
        let nobody = contest("practice", "", vec![("Sam Ito", 0), ("Dana Reyes", 0)]);
        assert_eq!(result_line(&nobody, "usd"), "November push is over, and nobody scored.");
        assert_eq!(score("deals_won", 1, "usd"), "1 deal won");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_metric_counts_only_the_window_and_the_currency(db: PgPool) {
        use crate::agent::practice::fixture;
        let t = fixture::team(&db).await;
        sqlx::query("update organizations set currency = 'usd' where id = $1").bind(t.org).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("select lead_id from deals where id = $1").bind(t.deal).fetch_one(&db).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select stage_id from deals where id = $1").bind(t.deal).fetch_one(&db).await.unwrap();
        // Sam owns the fixture's lead. One win in the window, one before it, one in euros.
        for (value, currency, closed) in [(500_000, "usd", "1 day"), (700_000, "usd", "20 days"), (900_000, "eur", "1 day")] {
            sqlx::query(&format!(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
                 values ($1, $2, $3, $4, 'won', $5, $6, now() - interval '{closed}')"
            ))
            .bind(t.org)
            .bind(t.project)
            .bind(lead)
            .bind(stage)
            .bind(value)
            .bind(currency)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query(
            "insert into calls (project_id, lead_id, kind, status, called_by, closed_at) values
             ($1, $2, 'manual', 'done', $3, now() - interval '1 day'), ($1, $2, 'manual', 'done', $3, now() - interval '2 days'),
             ($1, $2, 'manual', 'open', $3, null), ($1, $2, 'manual', 'done', $4, now() - interval '1 day')",
        )
        .bind(t.project)
        .bind(lead)
        .bind(t.dana)
        .bind(t.sam)
        .execute(&db)
        .await
        .unwrap();
        sqlx::query(
            "insert into practice_sessions (project_id, deal_id, user_id, scenario, score) values
             ($1, $2, $3, 'pricing', '{\"score\": 70}'), ($1, $2, $3, 'pricing', '{\"score\": 69}')",
        )
        .bind(t.project)
        .bind(t.deal)
        .bind(t.sam)
        .execute(&db)
        .await
        .unwrap();
        for metric in METRICS {
            sqlx::query("insert into contests (org_id, name, metric, starts_at, ends_at) values ($1, $2, $2, now() - interval '7 days', now() + interval '1 day')")
                .bind(t.org)
                .bind(metric)
                .execute(&db)
                .await
                .unwrap();
        }

        let (currency, contests) = list(&db, t.org).await.unwrap();
        assert_eq!(currency, "usd");
        let top = |metric: &str| {
            let c = contests.iter().find(|c| c.metric == metric).unwrap();
            c.standings.iter().map(|s| (s.name.as_str(), s.value)).collect::<Vec<_>>()
        };
        assert_eq!(top("won_value"), [("Sam Ito", 500_000), ("Dana Reyes", 0)]);
        assert_eq!(top("deals_won"), [("Sam Ito", 1), ("Dana Reyes", 0)]);
        assert_eq!(top("pipeline")[0].0, "Sam Ito", "the deals made just now count as new pipeline");
        assert_eq!(top("calls"), [("Dana Reyes", 2), ("Sam Ito", 1)]);
        assert_eq!(top("practice"), [("Sam Ito", 1), ("Dana Reyes", 0)]);
    }
}
