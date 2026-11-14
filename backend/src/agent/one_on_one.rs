//! 1:1 prep: the agent reads a teammate's month against their target, the open deals they own with what Splee knows about
//! each, what they won and lost lately, their call coaching and their practice calls, and writes what to bring to their
//! one-on-one: what to recognize, the deals to dig into with the question to ask, one skill to work on with the evidence and
//! a drill, and a commitment for the week. The numbers are kept from the Targets tab as they were, never asked of the model.
//! A drill that's a practice call on one of their deals can be assigned to them, and the next prep reads how it went.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::forecast::{OPEN_DEALS, OpenDeal, describe};
use super::sdk::Call;
use super::practice::SCENARIOS;
use super::{Ctx, Outcome, kind};
use crate::deals::{LAST_TOUCH, amount};
use crate::models::Ts;

/// How far back wins, losses, coaching and practice go.
const DAYS: i32 = 30;
const MAX_DEALS: i64 = 40;
const MAX_PICKS: usize = 4;
const MANAGER: &str = "You help a B2B sales manager using Splee, an outbound sales tool, prepare a one-on-one with a teammate, \
like a fair, direct sales coach. Use only the facts given. Name companies, never deal ids. Call the teammate by their first \
name or they, never a guessed he or she. Plain text only.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DealPick {
    pub deal_id: Uuid,
    pub company: String,
    pub why: String,
    /// The question to ask the teammate about it.
    pub ask: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Skill {
    pub name: String,
    pub evidence: String,
    pub drill: String,
    /// The practice call the drill is, when it's one on their open deals. See routes::one_on_ones::assign.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub practice: Option<Drill>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Drill {
    pub deal_id: Uuid,
    pub company: String,
    pub scenario: String,
}

/// The teammate's month when the prep was written, from the Targets tab.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Month {
    pub currency: String,
    pub day: i32,
    pub days: i32,
    pub target_cents: Option<i64>,
    pub won_cents: i64,
    pub commit_cents: i64,
    pub best_case_cents: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prep {
    pub headline: String,
    pub month: Month,
    pub recognize: Vec<String>,
    pub deals: Vec<DealPick>,
    pub skill: Skill,
    pub commitment: String,
}

/// Writes a prep for the teammate now. Returns the run.
pub async fn request(db: &PgPool, org_id: Uuid, user_id: Uuid, requested_by: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into one_on_ones (org_id, user_id, requested_by) values ($1, $2, $3) returning id")
        .bind(org_id)
        .bind(user_id)
        .bind(requested_by)
        .fetch_one(&mut *tx)
        .await?;
    let run = super::enqueue(&mut *tx, kind::ONE_ON_ONE, None, Some(org_id), json!({ "one_on_one_id": id })).await?;
    sqlx::query("update one_on_ones set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

/// The teammate's latest prep: `status` is ready, writing or failed, with the run's `error`.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub prep: Option<Value>,
    pub requested_by: Option<String>,
    pub created_at: Ts,
    pub status: String,
    pub error: Option<String>,
}

pub async fn latest(db: &PgPool, org_id: Uuid, user_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select o.id, o.prep, coalesce(nullif(u.name, ''), u.email::text) as requested_by, o.created_at,
                case when o.prep is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status,
                case when o.prep is null then nullif(r.error, '') end as error
         from one_on_ones o left join agent_runs r on r.id = o.run_id left join users u on u.id = o.requested_by
         where o.org_id = $1 and o.user_id = $2 order by o.created_at desc limit 1",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(db)
    .await
}

/// A deal they won or lost lately, with the review's reason and lesson.
#[derive(sqlx::FromRow)]
struct Closed {
    company: String,
    status: String,
    value_cents: i64,
    currency: String,
    reason: Option<String>,
    lesson: Option<String>,
}

#[derive(Deserialize)]
struct Input {
    one_on_one_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    headline: String,
    recognize: Vec<String>,
    deals: Vec<AnswerPick>,
    skill: AnswerSkill,
    commitment: String,
}

#[derive(Deserialize)]
struct AnswerSkill {
    name: String,
    evidence: String,
    drill: String,
    deal: String,
    scenario: String,
}

#[derive(Deserialize)]
struct AnswerPick {
    id: String,
    why: String,
    ask: String,
}

fn schema() -> Value {
    let text = json!({ "type": "string" });
    json!({
        "type": "object",
        "properties": {
            "headline": text,
            "recognize": { "type": "array", "items": text },
            "deals": { "type": "array", "items": { "type": "object", "properties": { "id": text, "why": text, "ask": text }, "required": ["id", "why", "ask"] } },
            "skill": { "type": "object",
                       "properties": { "name": text, "evidence": text, "drill": text, "deal": text,
                                       "scenario": { "type": "string", "enum": ["next_call", "pricing", "competitor", "none"] } },
                       "required": ["name", "evidence", "drill", "deal", "scenario"] },
            "commitment": text,
        },
        "required": ["headline", "recognize", "deals", "skill", "commitment"]
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (org_id, user_id, written, name, org): (Uuid, Uuid, bool, String, String) = sqlx::query_as(
        "select o.org_id, o.user_id, o.prep is not null, coalesce(nullif(u.name, ''), u.email::text), g.name
         from one_on_ones o join users u on u.id = o.user_id join organizations g on g.id = o.org_id where o.id = $1",
    )
    .bind(input.one_on_one_id)
    .fetch_optional(db)
    .await?
    .context("the 1:1 prep was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }
    let first = name.split_whitespace().next().unwrap_or(&name).to_string();

    let view = crate::routes::targets::month(db, org_id).await?;
    let row = view.rows.iter().find(|r| r.user_id == Some(user_id)).context("they're no longer in the organization")?;
    let month = Month {
        currency: view.currency.clone(),
        day: view.day,
        days: view.days,
        target_cents: row.target_cents,
        won_cents: row.won_cents,
        commit_cents: row.commit_cents,
        best_case_cents: row.best_case_cents,
    };
    let cur = month.currency.to_uppercase();
    let money = |cents: i64| format!("{} {cur}", amount(cents));
    let mut numbers = format!(
        "Day {} of {} this month. {first} won {} from {} deals, the forecast has {} in commit and {} in best case.",
        month.day,
        month.days,
        money(month.won_cents),
        row.won_deals,
        money(month.commit_cents),
        money(month.best_case_cents)
    );
    numbers += &match month.target_cents {
        Some(t) => format!(" Their target is {}, so {} was due by today.", money(t), money(t * i64::from(month.day) / i64::from(month.days.max(1)))),
        None => " They have no target this month.".to_string(),
    };

    let open: Vec<OpenDeal> = sqlx::query_as(&OPEN_DEALS.replace("{LAST_TOUCH}", LAST_TOUCH).replace("{WHERE}", "d.org_id = $1 and l.owner_id = $3"))
        .bind(org_id)
        .bind(MAX_DEALS)
        .bind(user_id)
        .fetch_all(db)
        .await?;
    let closed: Vec<Closed> = sqlx::query_as(
        "select l.company_name as company, d.status, d.value_cents, d.currency, r.reason, nullif(r.lesson, '') as lesson
         from deals d join leads l on l.id = d.lead_id left join deal_reviews r on r.deal_id = d.id and r.outcome = d.status
         where d.org_id = $1 and l.owner_id = $2 and d.status <> 'open' and d.closed_at > now() - make_interval(days => $3)
         order by d.closed_at desc limit 20",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(DAYS)
    .fetch_all(db)
    .await?;
    let coached: Vec<(String, Option<i32>, i32, Value)> = sqlx::query_as(
        "select l.company_name, c.talk_share, c.questions, c.coaching from call_coaching c join leads l on l.id = c.lead_id join projects p on p.id = c.project_id
         where p.org_id = $1 and c.owner_id = $2 and c.coaching is not null and c.created_at > now() - make_interval(days => $3)
         order by c.created_at desc limit 10",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(DAYS)
    .fetch_all(db)
    .await?;
    let practiced: Vec<(String, String, Value)> = sqlx::query_as(
        "select l.company_name, s.scenario, s.score from practice_sessions s join deals d on d.id = s.deal_id join leads l on l.id = d.lead_id
         where d.org_id = $1 and s.user_id = $2 and s.score is not null and s.created_at > now() - make_interval(days => $3)
         order by s.created_at desc limit 5",
    )
    .bind(org_id)
    .bind(user_id)
    .bind(DAYS)
    .fetch_all(db)
    .await?;
    let assigned: Option<(String, String, String, i32, Option<Value>)> = sqlx::query_as(
        "select a.skill, a.scenario, l.company_name, current_date - a.created_at::date, s.score
         from practice_assignments a join deals d on d.id = a.deal_id join leads l on l.id = d.lead_id
         left join lateral (select score from practice_sessions where assignment_id = a.id and score is not null order by created_at desc limit 1) s on true
         where d.org_id = $1 and a.user_id = $2 and a.one_on_one_id is not null order by a.created_at desc limit 1",
    )
    .bind(org_id)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    anyhow::ensure!(
        !open.is_empty() || !closed.is_empty() || !coached.is_empty() || !practiced.is_empty(),
        "{first} has no open deals, nothing won or lost, and no coached or practice calls in the last {DAYS} days, so there's nothing to prep from yet"
    );

    let or_none = |lines: Vec<String>| if lines.is_empty() { "None.".to_string() } else { lines.join("\n") };
    let listed = or_none(open.iter().enumerate().map(|(i, d)| describe(i, d)).collect());
    let closed = or_none(
        closed
            .iter()
            .map(|c| {
                let mut line = format!("- {} {}, {} {}", c.status, c.company, amount(c.value_cents), c.currency.to_uppercase());
                if let Some(r) = &c.reason {
                    line += &format!(", on {}", r.replace('_', " "));
                }
                if let Some(l) = &c.lesson {
                    line += &format!(". Lesson: {l}");
                }
                line
            })
            .collect(),
    );
    let text = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_string();
    let coached = or_none(
        coached
            .iter()
            .map(|(company, share, questions, c)| {
                let improve = c["improve"].as_array().map(|a| a.iter().map(|i| text(i, "moment")).filter(|m| !m.is_empty()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                let strengths = c["strengths"].as_array().map(|a| a.iter().filter_map(|s| s.as_str()).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                format!(
                    "- {company}: {}/10, talked {}% of the time, asked {questions} questions, {}. Did well: {strengths}. To change: {improve}",
                    c["score"],
                    share.map_or("?".to_string(), |s| s.to_string()),
                    if c["next_step_agreed"].as_bool() == Some(true) { "agreed a next step" } else { "no next step agreed" },
                )
            })
            .collect(),
    );
    let assigned = match assigned {
        None => "None.".to_string(),
        Some((skill, scenario, company, days, score)) => format!(
            "{skill}, a {} practice on {company}, {days} days ago. {}",
            scenario.replace('_', " "),
            match score {
                Some(s) => format!("Scored {}/100. {}", s["score"], [text(&s, "skill"), text(&s, "summary")].into_iter().find(|t| !t.is_empty()).unwrap_or_default()),
                None => "Not practiced yet.".to_string(),
            }
        ),
    };
    let practiced = or_none(practiced.iter().map(|(company, scenario, s)| format!("- {} with {company}: {}/100. {}", scenario.replace('_', " "), s["score"], text(s, "summary"))).collect());

    let prompt = format!(
        "Prepare {org}'s manager for a one-on-one with {name}.\n\n{numbers}\n\n\
         Open deals {first} owns, each with its id:\n{listed}\n\n\
         Won or lost in the last {DAYS} days:\n{closed}\n\n\
         Coached calls in the last {DAYS} days, newest first:\n{coached}\n\n\
         Practice calls in the last {DAYS} days:\n{practiced}\n\n\
         Practice assigned at the last one-on-one:\n{assigned}\n\n\
         1. headline: 2 sentences on how {first}'s month is going against the numbers and what matters most in this one-on-one.\n\
         2. recognize: up to 3 specific things {first} did well, each tied to a deal or call above, under 20 words. Empty if nothing earns it.\n\
         3. deals: up to {MAX_PICKS} open deal ids worth the manager's time, with why in under 20 words citing the evidence, and ask: \
         the one question to ask {first} about it, in their words to {first}.\n\
         4. skill: the one skill that would help {first} most: name in under 6 words, evidence from the coaching, practice or \
         deals in under 30 words, and drill: one practice to do this week, like a practice call on a named deal, in under 25 words. \
         When the drill is a practice call on one of the open deals above, deal is its id and scenario is next_call (the next call \
         as it stands), pricing (price pushback) or competitor. Otherwise deal is empty and scenario is none. If last time's \
         practice isn't done or went badly, the skill can stay the same.\n\
         5. commitment: one thing {first} should commit to by the next one-on-one, measurable, under 20 words.",
    );
    ctx.log(format!("reading {}'s month…", first)).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(MANAGER, prompt, schema()) };
    let prep = tidy(ctx.ask::<Answer>(&call).await?, &open, month);
    let picks = prep.deals.len();
    sqlx::query("update one_on_ones set prep = $2 where id = $1 and run_id = $3 and prep is null")
        .bind(input.one_on_one_id)
        .bind(json!(prep))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Prepped a 1:1 with {first}, {picks} deals to dig into")))
}

/// Picks must name a real open deal, once each. Lists and lines are clipped.
fn tidy(a: Answer, deals: &[OpenDeal], month: Month) -> Prep {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let deal = |id: &str| id.trim().trim_start_matches(['d', 'D']).parse::<usize>().ok().filter(|&i| i >= 1 && i <= deals.len()).map(|i| &deals[i - 1]);
    let mut seen = HashSet::new();
    let picks = a
        .deals
        .iter()
        .filter_map(|p| deal(&p.id).filter(|d| seen.insert(d.id)).map(|d| DealPick { deal_id: d.id, company: d.company.clone(), why: clip(&p.why, 200), ask: clip(&p.ask, 240) }))
        .take(MAX_PICKS)
        .collect();
    Prep {
        headline: clip(&a.headline, 600),
        month,
        recognize: a.recognize.iter().map(|r| clip(r, 200)).filter(|r| !r.is_empty()).take(3).collect(),
        deals: picks,
        skill: Skill {
            name: clip(&a.skill.name, 80),
            evidence: clip(&a.skill.evidence, 300),
            drill: clip(&a.skill.drill, 240),
            practice: deal(&a.skill.deal)
                .filter(|_| SCENARIOS.contains(&a.skill.scenario.as_str()))
                .map(|d| Drill { deal_id: d.id, company: d.company.clone(), scenario: a.skill.scenario.clone() }),
        },
        commitment: clip(&a.commitment, 240),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_prep_reads_the_teammates_month_deals_coaching_and_practice_and_keeps_only_real_deals(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name, currency) values ('Fieldnote', 'usd') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Fieldnote') returning id").bind(org).fetch_one(&db).await.unwrap();
        let mut users = vec![];
        for (name, role, target) in [("Dana Reyes", "owner", None), ("Sam Ito", "member", Some(1_200_000i64))] {
            let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values (gen_random_uuid() || '@fieldnote.test', $1, 'x') returning id")
                .bind(name)
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role, monthly_target_cents) values ($1, $2, $3, $4)")
                .bind(org)
                .bind(user)
                .bind(role)
                .bind(target)
                .execute(&db)
                .await
                .unwrap();
            users.push(user);
        }
        let (dana, sam) = (users[0], users[1]);
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = 'Proposal'").bind(org).fetch_one(&db).await.unwrap();
        let mut deals = vec![];
        // Sam owns an open deal and won one this month. Dana's open deal isn't Sam's.
        for (owner, company, status, cents) in [(sam, "Lumenfold", "open", 960_000i64), (sam, "Ledgerly", "won", 720_000), (dana, "Northpine", "open", 1_440_000)] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_name, owner_id) values ($1, 'Ines Duarte', $2, $3) returning id")
                .bind(project)
                .bind(company)
                .bind(owner)
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar(
                "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, currency, closed_at)
                 values ($1, $2, $3, $4, $5, $6, 'usd', case when $5 = 'won' then greatest(date_trunc('month', now()), now() - interval '1 minute') end) returning id",
            )
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .bind(status)
            .bind(cents)
            .fetch_one(&db)
            .await
            .unwrap();
            deals.push((deal, lead));
        }
        let (lumenfold, lumenfold_lead) = deals[0];
        sqlx::query("insert into deal_health (project_id, deal_id, health, likelihood, why) values ($1, $2, 'at_risk', 35, 'Quiet for 3 weeks')")
            .bind(project)
            .bind(lumenfold)
            .execute(&db)
            .await
            .unwrap();
        let call: Uuid = sqlx::query_scalar("insert into calls (project_id, lead_id, kind, status, called_at) values ($1, $2, 'manual', 'done', now()) returning id")
            .bind(project)
            .bind(lumenfold_lead)
            .fetch_one(&db)
            .await
            .unwrap();
        let coaching = json!({ "score": 6, "next_step_agreed": false, "next_step": "", "strengths": ["Named the HubSpot pain"], "covered": [],
                               "improve": [{ "moment": "Pricing came up and Sam discounted at once", "quote": "", "instead": "" }] });
        sqlx::query("insert into call_coaching (project_id, lead_id, call_id, owner_id, talk_share, questions, coaching) values ($1, $2, $3, $4, 71, 2, $5)")
            .bind(project)
            .bind(lumenfold_lead)
            .bind(call)
            .bind(sam)
            .bind(coaching)
            .execute(&db)
            .await
            .unwrap();
        // Last month's 1:1 assigned Sam a pricing practice, which went badly.
        let last: Uuid = sqlx::query_scalar("insert into one_on_ones (org_id, user_id, created_at) values ($1, $2, now() - interval '9 days') returning id")
            .bind(org)
            .bind(sam)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query(
            "with a as (insert into practice_assignments (one_on_one_id, deal_id, user_id, scenario, skill, drill, created_at)
                        values ($1, $2, $3, 'pricing', 'Holding price', 'Practice price pushback', now() - interval '9 days') returning id)
             insert into practice_sessions (project_id, deal_id, user_id, scenario, persona, score, assignment_id) select $4, $2, $3, 'pricing', '{}', $5, id from a",
        )
        .bind(last)
        .bind(lumenfold)
        .bind(sam)
        .bind(project)
        .bind(json!({ "score": 42, "summary": "Never answered the two-tools worry.", "skill": "Dropped the price at the first push." }))
            .execute(&db)
            .await
            .unwrap();

        let run = request(&db, org, sam, dana).await.unwrap();
        assert_eq!(latest(&db, org, sam).await.unwrap().unwrap().status, "writing");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(run, |_| {
            json!({ "headline": "Ahead of pace on one win.", "recognize": ["Closed Ledgerly", ""],
                    "deals": [{ "id": "d1", "why": "At risk and quiet", "ask": "What does Ines need to see?" }, { "id": "d1", "why": "Twice", "ask": "" },
                              { "id": "d7", "why": "Not a deal", "ask": "" }],
                    "skill": { "name": "Holding price", "evidence": "Discounted at once on Lumenfold", "drill": "A pricing practice on Lumenfold",
                               "deal": "d1", "scenario": "pricing" },
                    "commitment": "Book the Lumenfold follow-up by Friday" })
        });
        run_ctx(&ctx).await;
        let prompt = super::super::fake::prompts(run).first().cloned().unwrap();
        assert!(prompt.contains("Sam won 7,200 USD from 1 deals") && prompt.contains("Their target is 12,000 USD, so "), "{prompt}");
        assert!(prompt.contains("[d1] Lumenfold") && !prompt.contains("Northpine") && !prompt.contains("MEDDICC"), "only Sam's deals, and no MEDDICC line for one never qualified: {prompt}");
        assert!(prompt.contains("won Ledgerly, 7,200 USD"), "{prompt}");
        assert!(prompt.contains("- Lumenfold: 6/10, talked 71% of the time, asked 2 questions, no next step agreed. Did well: Named the HubSpot pain. To change: Pricing came up"), "{prompt}");
        assert!(prompt.contains("- pricing with Lumenfold: 42/100. Never answered the two-tools worry."), "{prompt}");
        assert!(prompt.contains("Holding price, a pricing practice on Lumenfold, 9 days ago. Scored 42/100. Dropped the price at the first push."), "{prompt}");

        let saved = latest(&db, org, sam).await.unwrap().unwrap();
        assert_eq!(saved.status, "ready");
        assert_eq!(saved.requested_by.as_deref(), Some("Dana Reyes"));
        let prep: Prep = serde_json::from_value(saved.prep.unwrap()).unwrap();
        assert_eq!(prep.deals, vec![DealPick { deal_id: lumenfold, company: "Lumenfold".into(), why: "At risk and quiet".into(), ask: "What does Ines need to see?".into() }]);
        assert_eq!(prep.recognize, vec!["Closed Ledgerly"]);
        assert_eq!(prep.skill.practice, Some(Drill { deal_id: lumenfold, company: "Lumenfold".into(), scenario: "pricing".into() }), "the drill names a real deal");
        assert_eq!((prep.month.target_cents, prep.month.won_cents), (Some(1_200_000), 720_000), "the numbers come from Targets, not the model");
    }

    async fn run_ctx(ctx: &Ctx) {
        run(ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(ctx.db()).await.unwrap();
    }
}
