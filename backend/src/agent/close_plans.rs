//! Drafts a deal's close plan (crate::close_plans) from the conversation: a one-line goal and the steps both sides take to a
//! signed deal, with dates up to the expected close. Writing it again keeps the steps already done and replaces the rest.

use std::time::Duration;

use anyhow::Context;
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind};
use crate::models::Lead;

const PLANNER: &str = "You write mutual close plans for B2B deals, for Splee, an outbound sales tool. The buyer reads the plan, \
so write to them plainly. Use only what you're given. Never invent facts, names, people or commitments.";
/// Steps the agent writes. The team can add more, up to close_plans::MAX_STEPS.
const MAX_WRITTEN: usize = 8;
/// How far ahead a plan reaches when the deal has no expected close date, or one that has passed.
const DEFAULT_DAYS: i64 = 30;

/// Starts writing the deal's plan, making the plan first if it has none. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let plan = crate::close_plans::ensure(&mut tx, project_id, deal_id).await?;
    let run = super::enqueue(&mut *tx, kind::CLOSE_PLAN, Some(project_id), Some(org_id), json!({ "plan_id": plan })).await?;
    sqlx::query("update close_plans set run_id = $2 where id = $1").bind(plan).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

#[derive(Deserialize)]
struct Input {
    plan_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct DealNow {
    lead_id: Uuid,
    status: String,
    stage: String,
    value_cents: i64,
    currency: String,
    close_date: Option<NaiveDate>,
    notes: String,
}

#[derive(Deserialize)]
struct Draft {
    goal: String,
    steps: Vec<DraftStep>,
}

#[derive(Deserialize)]
struct DraftStep {
    title: String,
    #[serde(default)]
    detail: String,
    side: String,
    #[serde(default)]
    due: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "goal": { "type": "string" },
            "steps": { "type": "array", "items": { "type": "object", "properties": {
                "title": { "type": "string" }, "detail": { "type": "string" },
                "side": { "type": "string", "enum": ["us", "them"] }, "due": { "type": "string" }
            }, "required": ["title", "detail", "side", "due"] } }
        },
        "required": ["goal", "steps"]
    })
}

/// The steps worth saving, in date order: titled, on a known side, at most MAX_WRITTEN, with dates kept between `from` and
/// `to`. A date that doesn't parse is dropped rather than guessed.
fn tidy(steps: Vec<DraftStep>, from: NaiveDate, to: NaiveDate) -> Vec<(String, String, &'static str, Option<NaiveDate>)> {
    let mut out: Vec<_> = steps
        .into_iter()
        .filter_map(|s| {
            let title = s.title.trim().chars().take(120).collect::<String>();
            let side = match s.side.as_str() {
                "us" => "us",
                "them" => "them",
                _ => return None,
            };
            let due = NaiveDate::parse_from_str(s.due.trim(), "%Y-%m-%d").ok().map(|d| d.clamp(from, to));
            (!title.is_empty()).then(|| (title, s.detail.trim().chars().take(300).collect(), side, due))
        })
        .take(MAX_WRITTEN)
        .collect();
    // Stable, so steps on the same day keep the model's order, and undated ones go last.
    out.sort_by_key(|s| s.3.unwrap_or(NaiveDate::MAX));
    out
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let deal_id: Uuid = sqlx::query_scalar("select deal_id from close_plans where id = $1 and project_id = $2")
        .bind(input.plan_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the plan was deleted")?;
    let deal = sqlx::query_as::<_, DealNow>(
        "select d.lead_id, d.status, s.name as stage, d.value_cents, d.currency, d.close_date, d.notes
         from deals d join deal_stages s on s.id = d.stage_id where d.id = $1",
    )
    .bind(deal_id)
    .fetch_one(db)
    .await?;
    anyhow::ensure!(deal.status == "open", "the deal is closed");
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(deal.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let materials = materials(ctx, lead.project_id).await?;
    let done: Vec<String> = sqlx::query_scalar("select title from close_plan_steps where plan_id = $1 and done_at is not null order by position")
        .bind(input.plan_id)
        .fetch_all(db)
        .await?;

    let today = Utc::now().date_naive();
    let end = deal.close_date.filter(|d| *d > today).unwrap_or(today + chrono::Duration::days(DEFAULT_DAYS));
    let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
    let close = match deal.close_date {
        Some(d) if d > today => format!("They expect to close by {d}."),
        _ => "There's no expected close date.".to_string(),
    };
    let notes = if deal.notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", deal.notes.trim()) };
    let done = if done.is_empty() { String::new() } else { format!("\n\nSteps already done, so leave them out: {}.", done.join("; ")) };
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let prompt = format!(
        "{us} has an open deal with this lead in the {stage} stage{value}. {close}{notes}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}{done}\n\n\
         Today is {today}. Write a close plan to share with the buyer: the steps both sides take from here to a signed deal \
         and getting started, so nothing stalls.\n\
         1. goal: under 15 words, naming their company, what they get and by when, like \"Acme's 8 reps live on Fieldnote by \
         5 January\".\n\
         2. steps: 4 to {max} steps in order. Each has a title of under 8 words starting with a verb, like \"Run a security \
         review with IT\"; a detail of one short sentence on what it takes or who's needed, or empty; side \"them\" for the \
         buyer's steps and \"us\" for ours; and due, a date as YYYY-MM-DD from {today} to {end}.\n\
         Build it from what was said: who else must agree, what they asked for, their deadlines. Add the usual steps for a \
         deal like this, like a trial, a proposal, a security review or signing, only where nothing says otherwise. Leave out \
         anything internal, like our notes on them or why it stalled. {language}",
        stage = deal.stage,
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        max = MAX_WRITTEN,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading the deal with {first} and writing a close plan…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(PLANNER, prompt, schema()) };
    let draft: Draft = ctx.ask(&call).await?;
    let steps = tidy(draft.steps, today, end);
    anyhow::ensure!(!draft.goal.trim().is_empty() && !steps.is_empty(), "the agent wrote an empty plan");

    let mut tx = db.begin().await?;
    sqlx::query("delete from close_plan_steps where plan_id = $1 and done_at is null").bind(input.plan_id).execute(&mut *tx).await?;
    let after: i32 = sqlx::query_scalar("select coalesce(max(position) + 1, 0) from close_plan_steps where plan_id = $1").bind(input.plan_id).fetch_one(&mut *tx).await?;
    for (i, (title, detail, side, due)) in steps.iter().enumerate() {
        sqlx::query("insert into close_plan_steps (plan_id, position, title, detail, side, due_date) values ($1, $2, $3, $4, $5, $6)")
            .bind(input.plan_id)
            .bind(after + i as i32)
            .bind(title)
            .bind(detail)
            .bind(side)
            .bind(due)
            .execute(&mut *tx)
            .await?;
    }
    sqlx::query("update close_plans set goal = $2 where id = $1").bind(input.plan_id).bind(draft.goal.trim().chars().take(200).collect::<String>()).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Outcome::new(format!("Wrote a close plan with {} steps", steps.len())))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    fn step(title: &str, side: &str, due: &str) -> DraftStep {
        DraftStep { title: title.into(), detail: String::new(), side: side.into(), due: due.into() }
    }

    #[test]
    fn steps_are_kept_in_date_order_within_the_plan_and_bad_ones_dropped() {
        let from = NaiveDate::from_ymd_opt(2026, 11, 14).unwrap();
        let to = NaiveDate::from_ymd_opt(2026, 12, 14).unwrap();
        let steps = tidy(
            vec![
                step("Sign the order form", "them", "2026-12-01"),
                step("Send the proposal", "us", "2026-11-01"),
                step(" ", "us", "2026-11-20"),
                step("Pick a plan", "both", "2026-11-20"),
                step("Kick off", "us", "2027-02-01"),
                step("Loop in finance", "them", "next week"),
            ],
            from,
            to,
        );
        let got: Vec<_> = steps.iter().map(|s| (s.0.as_str(), s.3.map(|d| d.to_string()))).collect();
        assert_eq!(
            got,
            vec![
                ("Send the proposal", Some("2026-11-14".into())),
                ("Sign the order form", Some("2026-12-01".into())),
                ("Kick off", Some("2026-12-14".into())),
                ("Loop in finance", None),
            ]
        );
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_plan_is_written_from_the_deal_and_writing_it_again_keeps_the_done_steps(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar(
            "insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\", \"description\": \"Call notes for sales teams\"}') returning id",
        )
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar(
            "insert into leads (project_id, full_name, first_name, email, company_name) values ($1, 'Lukas Weber', 'Lukas', 'lukas@quanta.example', 'Quanta Forms') returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        let stage: Uuid = sqlx::query_scalar("insert into deal_stages (org_id, name, kind, position) values ($1, 'Proposal', 'open', 0) returning id").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, value_cents, notes, close_date) values ($1, $2, $3, $4, 1800000, 'IT must sign off', current_date + 20) returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(&db)
        .await
        .unwrap();
        let today = Utc::now().date_naive();
        let write = |goal: &'static str, titles: &'static [&'static str]| {
            let db = db.clone();
            async move {
                let run_id = request(&db, deal, project, org).await.unwrap();
                let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run_id).fetch_one(&db).await.unwrap();
                let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
                let due = (today + chrono::Duration::days(60)).to_string();
                super::super::fake::answer_with(ctx.run.id, move |_| {
                    json!({ "goal": goal, "steps": titles.iter().map(|t| json!({ "title": t, "detail": "", "side": "them", "due": due })).collect::<Vec<_>>() })
                });
                run(&ctx).await.unwrap();
                sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(run_id).execute(&db).await.unwrap();
                super::super::fake::prompts(ctx.run.id)
            }
        };

        let prompts = write("Quanta Forms live by January", &["Run a security review", "Sign the order form"]).await;
        assert!(prompts[0].contains("IT must sign off") && prompts[0].contains("Proposal"), "{}", prompts[0]);
        let plan = crate::close_plans::for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        assert_eq!((plan.status.as_str(), plan.goal.as_deref()), ("ready", Some("Quanta Forms live by January")));
        assert_eq!(plan.steps[0].due_date, Some(today + chrono::Duration::days(20)), "kept by the expected close");

        let token = plan.token.clone();
        let review = plan.steps[0].id;
        assert!(crate::close_plans::tick(&db, &token, review, true).await.unwrap().is_some(), "the buyer ticks their step");
        assert!(crate::close_plans::tick(&db, &token, review, true).await.unwrap().is_none(), "once");

        let prompts = write("Quanta Forms live by January", &["Sign the order form", "Book the kickoff"]).await;
        assert!(prompts[0].contains("Steps already done, so leave them out: Run a security review."), "{}", prompts[0]);
        let plan = crate::close_plans::for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        let titles: Vec<_> = plan.steps.iter().map(|s| (s.title.as_str(), s.done_by.as_deref())).collect();
        assert_eq!(titles, vec![("Run a security review", Some("them")), ("Sign the order form", None), ("Book the kickoff", None)]);
        assert_eq!(plan.token, token, "the link the buyer has keeps working");
    }
}
