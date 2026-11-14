//! Deal health: the agent reads an open deal's conversation and what the buyer did lately, and rates it on track, at risk
//! or stalled, with how likely it is to close, why, and the next step. Every hour, open deals that have no rating, changed
//! since their last one or were rated more than STALE_DAYS ago get a new one, in projects that keep it on.

use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{company_signals, lead_brief};
use super::{Ctx, Outcome, briefs, kind};
use crate::deals::LAST_TOUCH;
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub const STALE_DAYS: i32 = 3;
/// Ratings queued per pass, so a big pipeline's first day spreads over a few hours.
const PER_PASS: i64 = 50;
const RATER: &str = "You assess B2B sales deals for Splee, an outbound sales tool, like a sharp sales manager reviewing the \
pipeline. Judge from the evidence you're given only, and say plainly when a deal is slipping.";

/// Queues a rating for every open deal that needs one. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with due as (
           insert into deal_health (project_id, deal_id)
           select d.project_id, d.id from deals d
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.deal_health
           left join lateral (select h.created_at, r.status from deal_health h left join agent_runs r on r.id = h.run_id
                              where h.deal_id = d.id order by h.created_at desc limit 1) h on true
           where d.status = 'open'
             and (h.created_at is null or (coalesce(h.status, '') not in ('queued', 'running')
                  and (h.created_at < now() - make_interval(days => $1) or {LAST_TOUCH} > h.created_at)))
           order by h.created_at nulls first limit $2
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(STALE_DAYS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Rates the deal now. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into deal_health (project_id, deal_id) values ($1, $2) returning id").bind(project_id).bind(deal_id).fetch_one(&mut *tx).await?;
    let run = start(&mut tx, id, project_id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, health_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::DEAL_HEALTH, Some(project_id), Some(org_id), json!({ "health_id": health_id })).await?;
    sqlx::query("update deal_health set run_id = $2 where id = $1").bind(health_id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "deal health").await
    });
}

#[derive(Deserialize)]
struct Input {
    health_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct DealNow {
    lead_id: Uuid,
    status: String,
    stage: String,
    probability: i32,
    value_cents: i64,
    currency: String,
    close_date: Option<chrono::NaiveDate>,
    notes: String,
    created_at: Ts,
    last_touch: Ts,
}

#[derive(Deserialize)]
struct Rating {
    health: String,
    likelihood: i32,
    why: String,
    next_step: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "health": { "type": "string", "enum": ["on_track", "at_risk", "stalled"] },
            "likelihood": { "type": "integer" }, "why": { "type": "string" }, "next_step": { "type": "string" }
        },
        "required": ["health", "likelihood", "why", "next_step"]
    })
}

#[derive(sqlx::FromRow)]
struct SentProposal {
    accepted_at: Option<Ts>,
    last_viewed_at: Option<Ts>,
    valid_until: Option<chrono::NaiveDate>,
    views: i32,
}

/// What the buyer did with the close plan and proposal, in a line each, or nothing.
async fn shared(db: &PgPool, deal_id: Uuid) -> sqlx::Result<String> {
    let mut lines = vec![];
    let plan: Option<(i64, i64, i64, Option<Ts>)> = sqlx::query_as(
        "select count(s.id), count(s.id) filter (where s.done_at is not null),
                count(s.id) filter (where s.done_at is null and s.due_date < current_date), p.last_viewed_at
         from close_plans p left join close_plan_steps s on s.plan_id = p.id where p.deal_id = $1 and p.goal is not null group by p.id",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    if let Some((total, done, late, seen)) = plan {
        let seen = seen.map_or("they haven't opened it".to_string(), |at| format!("they last opened it on {}", at.format("%-d %b")));
        lines.push(format!("Close plan shared with them: {done} of {total} steps done, {late} past their date, and {seen}."));
    }
    let proposal: Option<SentProposal> =
        sqlx::query_as("select accepted_at, last_viewed_at, valid_until, views from proposals where deal_id = $1 and title is not null").bind(deal_id).fetch_optional(db).await?;
    if let Some(SentProposal { accepted_at: accepted, last_viewed_at: seen, valid_until: until, views }) = proposal {
        lines.push(match (accepted, seen) {
            (Some(at), _) => format!("They accepted our proposal on {}.", at.format("%-d %b")),
            (None, Some(at)) => format!("Our proposal: opened {views} times, last on {}, not accepted{}.", at.format("%-d %b"), until.map(|d| format!(", open until {d}")).unwrap_or_default()),
            (None, None) => "Our proposal: sent, never opened.".to_string(),
        });
    }
    Ok(lines.join("\n"))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let deal_id: Uuid = sqlx::query_scalar("select deal_id from deal_health where id = $1 and project_id = $2")
        .bind(input.health_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the rating was deleted")?;
    let deal = sqlx::query_as::<_, DealNow>(&format!(
        "select d.lead_id, d.status, s.name as stage, s.probability, d.value_cents, d.currency, d.close_date, d.notes, d.created_at,
                {LAST_TOUCH} as last_touch
         from deals d join deal_stages s on s.id = d.stage_id where d.id = $1"
    ))
    .bind(deal_id)
    .fetch_one(db)
    .await?;
    anyhow::ensure!(deal.status == "open", "the deal is closed");
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(deal.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let shared = shared(db, deal_id).await?;

    let now = Utc::now();
    let today = now.date_naive();
    let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
    let close = match deal.close_date {
        Some(d) if d < today => format!("Its expected close date, {d}, has passed."),
        Some(d) => format!("It's expected to close by {d}."),
        None => "It has no expected close date.".to_string(),
    };
    let notes = if deal.notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", deal.notes.trim()) };
    let shared = if shared.is_empty() { String::new() } else { format!("\n{shared}") };
    let meeting = match lead.meeting_at {
        Some(at) if at > now => format!("\nA call with them is booked for {}.", at.format("%-d %b")),
        _ => String::new(),
    };
    let prompt = format!(
        "Today is {today}. {us} has an open deal with this lead in the {stage} stage{value}, which the pipeline counts as {probability}% \
         likely. {close} It opened {opened} days ago, and the last email either way, call or stage change was {quiet} days ago.{notes}{meeting}{shared}\n\n\
         What we sell: {name}. {description}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}\n\n\
         Rate the deal.\n\
         1. health: on_track when they're engaged and there's an agreed next step, at_risk when it's slipping (slow or vague \
         replies, missed dates, an open objection, a passed close date), stalled when nothing has moved for weeks or there's \
         no next step.\n\
         2. likelihood: how likely it closes as won, from 0 to 100. Start from the stage's {probability}% and move it by what \
         the evidence says.\n\
         3. why: under 20 words, naming the evidence that matters most, like what they said or did and when.\n\
         4. next_step: under 15 words, the one thing to do now, starting with a verb.",
        us = profile.name,
        stage = deal.stage,
        probability = deal.probability,
        opened = (now - deal.created_at).num_days(),
        quiet = (now - deal.last_touch).num_days(),
        name = profile.name,
        description = profile.description,
        about = serde_json::to_string_pretty(&about)?,
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading the deal with {first}…")).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.2, ..Call::writing(RATER, prompt, schema()) };
    let r: Rating = ctx.ask(&call).await?;
    anyhow::ensure!(["on_track", "at_risk", "stalled"].contains(&r.health.as_str()), "the agent gave an unknown health");

    sqlx::query("update deal_health set health = $2, likelihood = $3, why = $4, next_step = $5 where id = $1 and run_id = $6")
        .bind(input.health_id)
        .bind(&r.health)
        .bind(r.likelihood.clamp(0, 100))
        .bind(r.why.trim().chars().take(200).collect::<String>())
        .bind(r.next_step.trim().chars().take(160).collect::<String>())
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Rated the deal with {first} {}, {}% likely", r.health.replace('_', " "), r.likelihood.clamp(0, 100))))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn open_deals_are_rated_once_until_they_change_and_the_rating_shows_on_the_deal(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, email, company_name) values ($1, 'Lukas Weber', 'Lukas', 'l@q.example', 'Quanta Forms') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = 'Proposal'").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, value_cents, close_date, stage_changed_at) values ($1, $2, $3, $4, 1800000, current_date - 2, now() - interval '20 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into close_plans (project_id, deal_id, token, goal) values ($1, $2, 'tok', 'Live by January')").bind(project).bind(deal).execute(&db).await.unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 1);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "not while one is being written");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'deal_health' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| json!({ "health": "at_risk", "likelihood": 140, "why": " Quiet 20 days past the close date. ", "next_step": "Call Lukas" }));
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("has passed") && prompt.contains("20 days ago") && prompt.contains("Close plan shared with them"), "{prompt}");
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(&db).await.unwrap();

        let shown = crate::deals::get(&db, org, deal).await.unwrap().health.unwrap();
        assert_eq!((shown["health"].as_str(), shown["likelihood"].as_i64(), shown["why"].as_str()), (Some("at_risk"), Some(100), Some("Quiet 20 days past the close date.")));
        assert_eq!(queue_due(&db).await.unwrap(), 0, "rated and nothing changed");
        sqlx::query("update deals set stage_changed_at = now() where id = $1").bind(deal).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "it moved, so it's rated again");
        sqlx::query("update project_settings set deal_health = false").execute(&db).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded'").execute(&db).await.unwrap();
        sqlx::query("update deal_health set created_at = now() - interval '10 days'").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "switched off");
    }
}
