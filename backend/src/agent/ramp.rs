//! Ramp plans: the agent turns a project's playbook into a new teammate's first weeks: what to read and do each week, the
//! team's best coached calls to learn from, and practice calls on the team's open deals. Each practice call is assigned to
//! them (see practice::ASSIGNED) with PASS to reach, so it waits on their Today until they pass it. Written only when
//! someone asks. Writing a new plan for the same teammate replaces the old one and its practice.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::forecast::{OPEN_DEALS, OpenDeal, describe};
use super::practice::SCENARIOS;
use super::sdk::Call;
use super::{Ctx, Outcome, kind};
use crate::deals::LAST_TOUCH;
use crate::models::Ts;

/// The practice score that passes a ramp's practice call.
pub const PASS: i32 = 70;
const MAX_WEEKS: usize = 4;
const MAX_STEPS: usize = 4;
const MAX_PRACTICE: usize = 6;
const MAX_DEALS: i64 = 30;
const ONBOARDER: &str = "You onboard a new B2B salesperson on a team using Splee, an outbound sales tool, like its best sales \
leader. Use only the playbook and the facts given. Name companies and teammates, never deal ids. Call the new teammate by their \
first name or they, never a guessed he or she. Plain text only.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Practice {
    pub deal_id: Uuid,
    pub company: String,
    pub scenario: String,
    pub skill: String,
    pub drill: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Week {
    pub title: String,
    pub goal: String,
    pub steps: Vec<String>,
    pub practice: Vec<Practice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Plan {
    pub summary: String,
    pub weeks: Vec<Week>,
}

/// Writes a ramp plan for the teammate now. Returns the run.
pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid, user_id: Uuid, requested_by: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into ramps (project_id, user_id, requested_by) values ($1, $2, $3) returning id")
        .bind(project_id)
        .bind(user_id)
        .bind(requested_by)
        .fetch_one(&mut *tx)
        .await?;
    let run = super::enqueue(&mut *tx, kind::RAMP, Some(project_id), Some(org_id), json!({ "ramp_id": id })).await?;
    sqlx::query("update ramps set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// A teammate's latest ramp in the project: `status` is ready, writing or failed, with the run's `error`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub plan: Option<Value>,
    pub requested_by: Option<String>,
    pub created_at: Ts,
    pub status: String,
    pub error: Option<String>,
}

/// Each teammate's latest ramp in the project, or only `user`'s.
pub async fn latest(db: &PgPool, project_id: Uuid, user: Option<Uuid>) -> sqlx::Result<Vec<Saved>> {
    sqlx::query_as(
        "select distinct on (m.user_id) m.id, m.user_id, coalesce(nullif(u.name, ''), u.email::text) as name, m.plan,
                coalesce(nullif(b.name, ''), b.email::text) as requested_by, m.created_at,
                case when m.plan is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status,
                case when m.plan is null then nullif(r.error, '') end as error
         from ramps m join users u on u.id = m.user_id left join users b on b.id = m.requested_by left join agent_runs r on r.id = m.run_id
         where m.project_id = $1 and ($2::uuid is null or m.user_id = $2) order by m.user_id, m.created_at desc",
    )
    .bind(project_id)
    .bind(user)
    .fetch_all(db)
    .await
}

#[derive(Deserialize)]
struct Input {
    ramp_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    weeks: Vec<AnswerWeek>,
}

#[derive(Deserialize)]
struct AnswerWeek {
    title: String,
    goal: String,
    steps: Vec<String>,
    practice: Vec<AnswerPractice>,
}

#[derive(Deserialize)]
struct AnswerPractice {
    deal: String,
    scenario: String,
    skill: String,
    drill: String,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    let practice = json!({
        "type": "object",
        "properties": { "deal": text, "scenario": { "type": "string", "enum": SCENARIOS }, "skill": text, "drill": text },
        "required": ["deal", "scenario", "skill", "drill"]
    });
    json!({
        "type": "object",
        "properties": {
            "summary": text,
            "weeks": { "type": "array", "items": { "type": "object",
                "properties": { "title": text, "goal": text, "steps": { "type": "array", "items": text }, "practice": { "type": "array", "items": practice } },
                "required": ["title", "goal", "steps", "practice"] } },
        },
        "required": ["summary", "weeks"]
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (project_id, user_id, requested_by, created_at, written, name): (Uuid, Uuid, Option<Uuid>, Ts, bool, String) = sqlx::query_as(
        "select m.project_id, m.user_id, m.requested_by, m.created_at, m.plan is not null, coalesce(nullif(u.name, ''), u.email::text)
         from ramps m join users u on u.id = m.user_id where m.id = $1",
    )
    .bind(input.ramp_id)
    .fetch_optional(db)
    .await?
    .context("the ramp plan was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }
    let first = name.split_whitespace().next().unwrap_or(&name).to_string();
    let org_id: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project_id).fetch_one(db).await?;

    let playbook = super::playbook::written(db, project_id).await?.context("write the project's playbook first")?;
    let open: Vec<OpenDeal> = sqlx::query_as(&OPEN_DEALS.replace("{LAST_TOUCH}", LAST_TOUCH).replace("{WHERE}", "d.org_id = $1 and d.project_id = $3"))
        .bind(org_id)
        .bind(MAX_DEALS)
        .bind(project_id)
        .fetch_all(db)
        .await?;
    anyhow::ensure!(!open.is_empty(), "practice calls are on the team's open deals, and this project has none yet");
    let calls: Vec<(String, Option<String>, Value)> = sqlx::query_as(
        "select l.company_name, nullif(u.name, ''), c.coaching from call_coaching c join leads l on l.id = c.lead_id left join users u on u.id = c.owner_id
         where c.project_id = $1 and c.coaching is not null and c.owner_id is distinct from $2
         order by (c.coaching->>'score')::int desc, c.created_at desc limit 5",
    )
    .bind(project_id)
    .bind(user_id)
    .fetch_all(db)
    .await?;

    let deals = open.iter().enumerate().map(|(i, d)| describe(i, d)).collect::<Vec<_>>().join("\n");
    let calls = if calls.is_empty() {
        "None yet.".to_string()
    } else {
        calls
            .iter()
            .map(|(company, owner, c)| {
                let strengths = c["strengths"].as_array().map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                format!("- {} with {company}, {}/10. Did well: {strengths}", owner.as_deref().unwrap_or("A teammate"), c["score"])
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "Plan {name}'s first {MAX_WEEKS} weeks selling on this team.\n\n\
         The team's playbook:\n{playbook}\n\n\
         The team's open deals, each with its id:\n{deals}\n\n\
         The team's best coached calls to learn from:\n{calls}\n\n\
         1. summary: 2 sentences to {first} on how the ramp works and what ready looks like.\n\
         2. weeks: up to {MAX_WEEKS}, in order, each with a title in under 6 words, goal: what {first} can do by the end of it, \
         in under 20 words, steps: up to {MAX_STEPS} things to read, study or do that week, each under 25 words and tied to the \
         playbook, a call above or a deal, and practice: the practice calls for that week. A practice call is on one of the open \
         deals above with the agent playing its buyer: deal is its id and scenario is next_call (the next call as it stands), \
         pricing (price pushback) or competitor. skill is what it trains in under 6 words and drill what to do on the call in \
         under 25 words. Up to {MAX_PRACTICE} practice calls across the plan, each deal and scenario once, building from the \
         basics. {first} passes each by scoring {PASS} out of 100.",
        playbook = serde_json::to_string_pretty(&playbook)?,
    );
    ctx.log(format!("planning {first}'s ramp…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(ONBOARDER, prompt, schema()) };
    let plan = tidy(ctx.ask::<Answer>(&call).await?, &open);
    let practice: Vec<&Practice> = plan.weeks.iter().flat_map(|w| &w.practice).collect();

    let mut tx = db.begin().await?;
    let saved = sqlx::query("update ramps set plan = $2 where id = $1 and run_id = $3 and plan is null")
        .bind(input.ramp_id)
        .bind(json!(plan))
        .bind(ctx.run.id)
        .execute(&mut *tx)
        .await?;
    if saved.rows_affected() == 0 {
        return Ok(Outcome::new("Already written"));
    }
    // Older plans go with their practice. Their sessions stay, no longer counted toward any ramp.
    sqlx::query("delete from ramps where project_id = $1 and user_id = $2 and created_at < $3")
        .bind(project_id)
        .bind(user_id)
        .bind(created_at)
        .execute(&mut *tx)
        .await?;
    for p in &practice {
        sqlx::query(
            "insert into practice_assignments (ramp_id, deal_id, user_id, assigned_by, scenario, skill, drill, pass_score)
             values ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(input.ramp_id)
        .bind(p.deal_id)
        .bind(user_id)
        .bind(requested_by)
        .bind(&p.scenario)
        .bind(&p.skill)
        .bind(&p.drill)
        .bind(PASS)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(Outcome::new(format!("Planned {first}'s ramp with {} practice calls", practice.len())))
}

/// Practice must be on a real open deal in a known scenario, each deal and scenario once. Lists and lines are clipped.
fn tidy(a: Answer, deals: &[OpenDeal]) -> Plan {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let deal = |id: &str| id.trim().trim_start_matches(['d', 'D']).parse::<usize>().ok().filter(|&i| i >= 1 && i <= deals.len()).map(|i| &deals[i - 1]);
    let mut seen = HashSet::new();
    let mut weeks: Vec<Week> = a
        .weeks
        .into_iter()
        .take(MAX_WEEKS)
        .map(|w| Week {
            title: clip(&w.title, 80),
            goal: clip(&w.goal, 200),
            steps: w.steps.iter().map(|s| clip(s, 240)).filter(|s| !s.is_empty()).take(MAX_STEPS).collect(),
            practice: w
                .practice
                .iter()
                .filter(|p| SCENARIOS.contains(&p.scenario.as_str()))
                .filter_map(|p| {
                    let d = deal(&p.deal)?;
                    seen.insert((d.id, p.scenario.clone())).then(|| Practice {
                        deal_id: d.id,
                        company: d.company.clone(),
                        scenario: p.scenario.clone(),
                        skill: clip(&p.skill, 80),
                        drill: clip(&p.drill, 240),
                    })
                })
                .collect(),
        })
        .collect();
    let mut left = MAX_PRACTICE;
    for w in &mut weeks {
        w.practice.truncate(left);
        left -= w.practice.len();
    }
    Plan { summary: clip(&a.summary, 400), weeks }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::agent::practice::fixture;
    use crate::models::AgentRun;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_ramp_is_planned_from_the_playbook_and_its_practice_is_assigned_with_a_score_to_pass(db: PgPool) {
        let t = fixture::team(&db).await;
        sqlx::query("insert into playbooks (project_id, playbook) values ($1, $2)")
            .bind(t.project)
            .bind(json!({ "summary": "Sell to RevOps leads who log calls by hand.", "pitch": "Stop logging calls." }))
            .execute(&db)
            .await
            .unwrap();
        let old = request(&db, t.project, t.org, t.sam, t.dana).await.unwrap();
        sqlx::query("update ramps set plan = '{}' where run_id = $1").bind(old).execute(&db).await.unwrap();
        let run = request(&db, t.project, t.org, t.sam, t.dana).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        let practice = |deal: &str, scenario: &str| json!({ "deal": deal, "scenario": scenario, "skill": "Discovery", "drill": "Ask about call logging." });
        super::super::fake::answer_with(run, move |_| {
            json!({ "summary": "Four weeks to your first deals.", "weeks": [
                { "title": "Learn the buyer", "goal": "Pitch in a minute", "steps": ["Read the playbook", ""],
                  "practice": [practice("d1", "next_call"), practice("d1", "next_call"), practice("d9", "pricing")] },
                { "title": "Handle pushback", "goal": "Hold price", "steps": [], "practice": [practice("d1", "pricing"), practice("d1", "bogus")] },
            ] })
        });
        run_ctx(&ctx).await;
        let prompt = super::super::fake::prompts(run).first().cloned().unwrap();
        assert!(prompt.contains("Plan Sam Ito's first 4 weeks") && prompt.contains("Stop logging calls.") && prompt.contains("[d1] Lumenfold"), "{prompt}");

        let saved = latest(&db, t.project, Some(t.sam)).await.unwrap();
        assert_eq!(saved.len(), 1, "the older plan is replaced");
        let plan: Plan = serde_json::from_value(saved[0].plan.clone().unwrap()).unwrap();
        assert_eq!(plan.weeks[0].steps, vec!["Read the playbook"]);
        let kept: Vec<(usize, &str)> = plan.weeks.iter().enumerate().flat_map(|(i, w)| w.practice.iter().map(move |p| (i, p.scenario.as_str()))).collect();
        assert_eq!(kept, vec![(0, "next_call"), (1, "pricing")], "a deal and scenario once, real deals and scenarios only");
        let assigned: Vec<(String, Option<i32>, Option<Uuid>)> =
            sqlx::query_as("select scenario, pass_score, assigned_by from practice_assignments where ramp_id = $1 order by scenario").bind(saved[0].id).fetch_all(&db).await.unwrap();
        assert_eq!(assigned, vec![("next_call".into(), Some(PASS), Some(t.dana)), ("pricing".into(), Some(PASS), Some(t.dana))]);
    }

    async fn run_ctx(ctx: &Ctx) {
        run(ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(ctx.db()).await.unwrap();
    }
}
