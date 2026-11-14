//! Team briefs: every Monday from 8am in the organization's first project's timezone, organizations with at least two members
//! get one, emailed to owners and admins. The agent reads each teammate's month from Targets, their open and at-risk deals,
//! their coached and practice calls from the last 7 days, the practice assigned to them and their ramp, and when their last
//! 1:1 was prepped. It writes who needs what this week, in the order to talk to them. Owners and admins can also ask for
//! one any time, which isn't emailed.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::sdk::Call;
use super::{Ctx, Outcome, kind};
use crate::deals::amount;
use crate::models::Ts;
use crate::state::AppState;

const MANAGER: &str = "You brief a B2B sales manager using Splee, an outbound sales tool, on their team's week, like a sharp \
chief of staff. Use only the facts given. Name teammates and companies, never ids. Call each teammate by their first name or \
they, never a guessed he or she. Plain text only.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Person {
    pub user_id: Uuid,
    pub name: String,
    /// How they're doing, from the facts.
    pub note: String,
    /// The one thing the manager should do with them this week.
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Brief {
    pub headline: String,
    /// In the order to talk to them.
    pub people: Vec<Person>,
    pub risks: Vec<String>,
}

/// Queues a brief for every organization where it's Monday from 8am, with two or more members, the brief switched on, and
/// none from the last 6 days.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "with zone as (
           select o.id, coalesce((select s.timezone from projects p join project_settings s on s.project_id = p.id
                                  where p.org_id = o.id order by p.created_at limit 1), 'UTC') as tz
           from organizations o where o.team_brief and (select count(*) from memberships m where m.org_id = o.id) >= 2)
         insert into team_briefs (org_id, auto)
         select z.id, true from zone z
         where extract(isodow from now() at time zone z.tz) = 1 and extract(hour from now() at time zone z.tz) >= 8
           and not exists (select 1 from team_briefs b where b.org_id = z.id and b.auto and b.created_at > now() - interval '6 days')
         returning id, org_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, org_id) in &due {
        start(&mut tx, *id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Writes a brief for the organization now, not emailed. Returns the run.
pub async fn request(db: &PgPool, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into team_briefs (org_id) values ($1) returning id").bind(org_id).fetch_one(&mut *tx).await?;
    let run = start(&mut tx, id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::TEAM_BRIEF, None, Some(org_id), json!({ "brief_id": id })).await?;
    sqlx::query("update team_briefs set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "team briefs").await });
}

/// The organization's latest brief: `status` is ready, writing or failed, with the run's `error`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub brief: Option<Value>,
    pub auto: bool,
    pub created_at: Ts,
    pub status: String,
    pub error: Option<String>,
}

pub async fn latest(db: &PgPool, org_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select b.id, b.brief, b.auto, b.created_at,
                case when b.brief is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status,
                case when b.brief is null then nullif(r.error, '') end as error
         from team_briefs b left join agent_runs r on r.id = b.run_id where b.org_id = $1 order by b.created_at desc limit 1",
    )
    .bind(org_id)
    .fetch_optional(db)
    .await
}

/// A teammate's week, beyond their Targets row.
#[derive(sqlx::FromRow)]
struct Week {
    user_id: Uuid,
    open_deals: i64,
    /// Open deals rated at risk or stalled, like "Lumenfold (at risk)".
    at_risk: Option<String>,
    calls: i64,
    call_score: Option<f64>,
    practiced: i64,
    practice_score: Option<f64>,
    /// Practice assigned to them on open deals that isn't done, and their ramp's passed and total.
    assigned_open: i64,
    ramp_passed: i64,
    ramp_total: i64,
    last_one_on_one: Option<i32>,
}

#[derive(Deserialize)]
struct Input {
    brief_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    headline: String,
    people: Vec<AnswerPerson>,
    risks: Vec<String>,
}

#[derive(Deserialize)]
struct AnswerPerson {
    id: String,
    note: String,
    action: String,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    json!({
        "type": "object",
        "properties": {
            "headline": text,
            "people": { "type": "array", "items": { "type": "object", "properties": { "id": text, "note": text, "action": text }, "required": ["id", "note", "action"] } },
            "risks": { "type": "array", "items": text },
        },
        "required": ["headline", "people", "risks"]
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (org_id, auto, written, org): (Uuid, bool, bool, String) = sqlx::query_as(
        "select b.org_id, b.auto, b.brief is not null, o.name from team_briefs b join organizations o on o.id = b.org_id where b.id = $1",
    )
    .bind(input.brief_id)
    .fetch_optional(db)
    .await?
    .context("the team brief was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }

    let month = crate::routes::targets::month(db, org_id).await?;
    let weeks: Vec<Week> = sqlx::query_as(
        "with done as (
           select a.user_id, a.ramp_id is not null as ramp, d.status = 'open' as open,
                  coalesce((s.score->>'score')::int >= coalesce(a.pass_score, 0), false) as done
           from practice_assignments a join deals d on d.id = a.deal_id
           left join lateral (select score from practice_sessions where assignment_id = a.id and score is not null
                              order by (score->>'score')::int desc limit 1) s on true
           where d.org_id = $1)
         select m.user_id,
                (select count(*) from deals d join leads l on l.id = d.lead_id where d.org_id = $1 and d.status = 'open' and l.owner_id = m.user_id) as open_deals,
                (select string_agg(coalesce(nullif(l.company_name, ''), l.full_name) || ' (' || replace(h.health, '_', ' ') || ')', ', ')
                 from deals d join leads l on l.id = d.lead_id
                 join lateral (select health from deal_health where deal_id = d.id and health is not null order by created_at desc limit 1) h on true
                 where d.org_id = $1 and d.status = 'open' and l.owner_id = m.user_id and h.health in ('at_risk', 'stalled')) as at_risk,
                (select count(*) from call_coaching c join projects p on p.id = c.project_id
                 where p.org_id = $1 and c.owner_id = m.user_id and c.coaching is not null and c.created_at > now() - interval '7 days') as calls,
                (select avg((c.coaching->>'score')::float8) from call_coaching c join projects p on p.id = c.project_id
                 where p.org_id = $1 and c.owner_id = m.user_id and c.coaching is not null and c.created_at > now() - interval '7 days') as call_score,
                (select count(*) from practice_sessions s join projects p on p.id = s.project_id
                 where p.org_id = $1 and s.user_id = m.user_id and s.score is not null and s.created_at > now() - interval '7 days') as practiced,
                (select avg((s.score->>'score')::float8) from practice_sessions s join projects p on p.id = s.project_id
                 where p.org_id = $1 and s.user_id = m.user_id and s.score is not null and s.created_at > now() - interval '7 days') as practice_score,
                (select count(*) from done where done.user_id = m.user_id and done.open and not done.done) as assigned_open,
                (select count(*) from done where done.user_id = m.user_id and done.ramp and done.done) as ramp_passed,
                (select count(*) from done where done.user_id = m.user_id and done.ramp) as ramp_total,
                (select (current_date - max(o.created_at)::date)::int from one_on_ones o where o.org_id = $1 and o.user_id = m.user_id) as last_one_on_one
         from memberships m where m.org_id = $1",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;

    let cur = month.currency.to_uppercase();
    let money = |cents: i64| format!("{} {cur}", amount(cents));
    let people: Vec<(Uuid, String)> = month.rows.iter().filter_map(|r| r.user_id.map(|id| (id, r.name.clone()))).collect();
    anyhow::ensure!(people.len() >= 2, "a team brief needs at least two teammates");
    let lines: Vec<String> = month
        .rows
        .iter()
        .filter_map(|r| Some((r, r.user_id?)))
        .enumerate()
        .map(|(i, (r, id))| {
            let mut line = format!("- [p{}] {}: won {} from {} deals", i + 1, r.name, money(r.won_cents), r.won_deals);
            line += &match r.target_cents {
                Some(t) => format!(" of a {} target, {} due by today", money(t), money(t * i64::from(month.day) / i64::from(month.days.max(1)))),
                None => ", no target".to_string(),
            };
            line += &format!(". Commit {}, best case {}.", money(r.commit_cents), money(r.best_case_cents));
            // The deals that close their gap, as the Targets tab shows them, so the brief can name one to dig into.
            let deals: Vec<String> = r
                .deals
                .as_array()
                .into_iter()
                .flatten()
                .map(|d| {
                    let s = |k: &str| d[k].as_str().unwrap_or_default();
                    let who = if s("company").is_empty() { s("name") } else { s("company") };
                    format!("{who} ({}, {}, {})", money(d["value_cents"].as_i64().unwrap_or(0)), s("category").replace('_', " "), s("why"))
                })
                .collect();
            if !deals.is_empty() {
                line += &format!(" Deals that close the gap: {}.", deals.join("; "));
            }
            if let Some(w) = weeks.iter().find(|w| w.user_id == id) {
                line += &format!(" {} open deals", w.open_deals);
                line += &w.at_risk.as_ref().map_or(".".to_string(), |a| format!(", at risk or stalled: {a}."));
                line += &format!(" Last 7 days: {} coached calls", w.calls);
                if let Some(s) = w.call_score {
                    line += &format!(" averaging {s:.1}/10");
                }
                line += &format!(", {} scored practice calls", w.practiced);
                if let Some(s) = w.practice_score {
                    line += &format!(" averaging {s:.0}/100");
                }
                line += ".";
                if w.assigned_open > 0 {
                    line += &format!(" {} assigned practice calls not done.", w.assigned_open);
                }
                if w.ramp_total > 0 {
                    line += &format!(" Ramping: {} of {} practice calls passed.", w.ramp_passed, w.ramp_total);
                }
                line += &match w.last_one_on_one {
                    Some(d) => format!(" Last 1:1 prepped {d} days ago."),
                    None => " No 1:1 prepped yet.".to_string(),
                };
            }
            line
        })
        .collect();
    let prompt = format!(
        "Brief {org}'s sales manager on the team this week. Day {} of {} this month.\n\nThe team, each with their id:\n{}\n\n\
         1. headline: 2 sentences on how the team's month is going and what matters most this week.\n\
         2. people: every teammate above, in the order the manager should talk to them this week, most urgent first. id is \
         theirs, note is how they're doing in under 25 words citing the facts, and action is the one thing to do with them \
         this week in under 20 words, like a deal to inspect, a skill to practice, a ramp to push or a win to recognize.\n\
         3. risks: up to 3 things that could cost the team its month, each under 20 words. Empty if there are none.",
        month.day,
        month.days,
        lines.join("\n"),
    );
    ctx.log("reading the team's week…").await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(MANAGER, prompt, schema()) };
    let brief = tidy(ctx.ask::<Answer>(&call).await?, &people);
    let saved = sqlx::query("update team_briefs set brief = $2 where id = $1 and run_id = $3 and brief is null")
        .bind(input.brief_id)
        .bind(json!(brief))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    if saved.rows_affected() > 0 && auto {
        email(&ctx.state, org_id, &brief).await?;
    }
    Ok(Outcome::new(format!("Briefed the team on {} people", brief.people.len())))
}

/// Each teammate once, by a real id. Lines are clipped.
fn tidy(a: Answer, people: &[(Uuid, String)]) -> Brief {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let person = |id: &str| id.trim().trim_start_matches(['p', 'P']).parse::<usize>().ok().filter(|&i| i >= 1 && i <= people.len()).map(|i| &people[i - 1]);
    let mut seen = HashSet::new();
    Brief {
        headline: clip(&a.headline, 500),
        people: a
            .people
            .iter()
            .filter_map(|p| {
                let (user_id, name) = person(&p.id).filter(|(id, _)| seen.insert(*id))?;
                Some(Person { user_id: *user_id, name: name.clone(), note: clip(&p.note, 240), action: clip(&p.action, 200) })
            })
            .collect(),
        risks: a.risks.iter().map(|r| clip(r, 200)).filter(|r| !r.is_empty()).take(3).collect(),
    }
}

/// The Monday brief to each owner and admin.
async fn email(state: &AppState, org_id: Uuid, brief: &Brief) -> anyhow::Result<()> {
    let to: Vec<String> = sqlx::query_scalar("select u.email::text from memberships m join users u on u.id = m.user_id where m.org_id = $1 and m.role in ('owner', 'admin')")
        .bind(org_id)
        .fetch_all(&state.db)
        .await?;
    let mut body = format!("{}\n", brief.headline);
    for p in &brief.people {
        body += &format!("\n{}: {}\nThis week: {}\n", p.name, p.note, p.action);
    }
    if !brief.risks.is_empty() {
        body += &format!("\nRisks:\n{}\n", brief.risks.iter().map(|r| format!("- {r}")).collect::<Vec<_>>().join("\n"));
    }
    body += &format!("\nThe team's month: {}/app/pipeline?view=targets\n", state.cfg.public_url);
    for to in to {
        if let Err(e) = crate::mail::send_system_email(state, org_id, &to, "Your team this week", &body).await {
            tracing::warn!(error = ?e, "team brief email failed");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::agent::practice::fixture;
    use crate::models::AgentRun;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_brief_reads_each_teammates_week_and_keeps_each_once(db: PgPool) {
        let t = fixture::team(&db).await;
        sqlx::query("update organizations set currency = 'usd' where id = $1").bind(t.org).execute(&db).await.unwrap();
        sqlx::query("update memberships set monthly_target_cents = 1200000 where user_id = $1").bind(t.sam).execute(&db).await.unwrap();
        sqlx::query("insert into deal_health (project_id, deal_id, health, likelihood, why) values ($1, $2, 'at_risk', 30, 'Quiet')")
            .bind(t.project)
            .bind(t.deal)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("update deals set value_cents = 480000, currency = 'usd' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        let forecast = json!({ "headline": "", "month_end": "2999-12-31", "focus": [], "slipping": [],
                               "deals": [{ "deal_id": t.deal, "category": "commit", "why": "Signing Friday" }] });
        sqlx::query("insert into pipeline_forecasts (project_id, forecast) values ($1, $2)").bind(t.project).bind(forecast).execute(&db).await.unwrap();
        let prep = fixture::prep(&db, &t, true).await;
        sqlx::query(
            "with a as (insert into practice_assignments (one_on_one_id, deal_id, user_id, scenario, skill, drill) values ($1, $2, $3, 'pricing', 'Holding price', 'Hold it.') returning id)
             insert into practice_sessions (project_id, deal_id, user_id, scenario, assignment_id, score) select $4, $2, $3, 'pricing', null, '{\"score\": 64}' from a",
        )
        .bind(prep)
        .bind(t.deal)
        .bind(t.sam)
        .bind(t.project)
        .execute(&db)
        .await
        .unwrap();

        let run_id = request(&db, t.org).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run_id).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(run_id, |_| {
            json!({ "headline": "Sam is behind.", "risks": ["Lumenfold could slip", ""],
                    "people": [{ "id": "p2", "note": "Behind", "action": "Inspect Lumenfold" }, { "id": "p2", "note": "Twice", "action": "" },
                               { "id": "p9", "note": "Nobody", "action": "" }, { "id": "p1", "note": "Fine", "action": "Recognize" }] })
        });
        run(&ctx).await.unwrap();
        let prompt = super::super::fake::prompts(run_id).first().cloned().unwrap();
        assert!(prompt.contains("Sam Ito: won 0 USD from 0 deals of a 12,000 USD target"), "{prompt}");
        assert!(prompt.contains("Commit 4,800 USD, best case 0 USD. Deals that close the gap: Lumenfold (4,800 USD, commit, Signing Friday)."), "{prompt}");
        assert!(prompt.contains("1 open deals, at risk or stalled: Lumenfold (at risk). Last 7 days: 0 coached calls, 1 scored practice calls averaging 64/100. 1 assigned practice calls not done. Last 1:1 prepped 0 days ago."), "{prompt}");
        assert!(prompt.contains("Dana Reyes: won 0 USD from 0 deals, no target") && prompt.contains("No 1:1 prepped yet."), "{prompt}");

        let saved = latest(&db, t.org).await.unwrap().unwrap();
        assert_eq!((saved.status.as_str(), saved.auto), ("ready", false));
        let brief: Brief = serde_json::from_value(saved.brief.unwrap()).unwrap();
        let order: Vec<&str> = brief.people.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(order, ["Sam Ito", "Dana Reyes"], "each once, real teammates only, in the model's order");
        assert_eq!(brief.risks, vec!["Lumenfold could slip"]);
    }
}
