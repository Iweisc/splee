//! Win/loss reviews: once a deal is won or lost, the agent reads its whole story and says why, with the turning point, the
//! competitor if one comes up, and a lesson for the next deal. The pipeline's Win/loss view groups the reasons and
//! competitors. Deals closed in the board's 90 days get one by themselves, and a deal closed again gets a new one.

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
use super::{Ctx, Outcome, briefs, health, kind};
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub const WON_REASONS: [&str; 7] = ["value", "fit", "champion", "timing", "price", "proof", "other"];
pub const LOST_REASONS: [&str; 7] = ["price", "competitor", "timing", "no_decision", "fit", "went_dark", "other"];
/// Like the board, which shows deals closed this recently.
const DAYS: i32 = 90;
const PER_PASS: i64 = 50;
const REVIEWER: &str = "You run win/loss reviews of B2B sales deals for Splee, an outbound sales tool, like a sharp sales manager \
after a deal closes. Judge from the evidence you're given only. Never invent what the buyer said, a competitor or a date.";

/// Writes a fresh review into the deal's row and returns its id: the first, or a new one after the deal closed again.
const UPSERT: &str = "on conflict (deal_id) do update set outcome = excluded.outcome, run_id = null, reason = null, story = '', turning_point = '',
    competitor = '', lesson = '', created_at = now()";

/// Queues a review for every deal closed lately that has none, or was closed again since its review. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with due as (
           insert into deal_reviews (project_id, deal_id, outcome)
           select d.project_id, d.id, d.status from deals d
           join projects p on p.id = d.project_id and p.org_id is not null
           left join deal_reviews r on r.deal_id = d.id left join agent_runs ar on ar.id = r.run_id
           where d.status in ('won', 'lost') and d.closed_at > now() - make_interval(days => $1)
             and (r.id is null or (r.created_at < d.closed_at and coalesce(ar.status, '') not in ('queued', 'running')))
           order by d.closed_at desc limit $2
           {UPSERT}
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(DAYS)
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Reviews the closed deal now, over any review it had. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid, outcome: &str) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar(&format!("insert into deal_reviews (project_id, deal_id, outcome) values ($1, $2, $3) {UPSERT} returning id"))
        .bind(project_id)
        .bind(deal_id)
        .bind(outcome)
        .fetch_one(&mut *tx)
        .await?;
    let run = start(&mut tx, id, project_id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, review_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::DEAL_REVIEW, Some(project_id), Some(org_id), json!({ "review_id": review_id })).await?;
    sqlx::query("update deal_reviews set run_id = $2 where id = $1").bind(review_id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(15 * 60), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "deal reviews").await
    });
}

#[derive(Deserialize)]
struct Input {
    review_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Closed {
    deal_id: Uuid,
    outcome: String,
    status: String,
    lead_id: Uuid,
    value_cents: i64,
    currency: String,
    close_date: Option<chrono::NaiveDate>,
    notes: String,
    lost_reason: String,
    created_at: Ts,
    closed_at: Option<Ts>,
}

#[derive(Deserialize)]
struct Answer {
    reason: String,
    story: String,
    turning_point: String,
    competitor: String,
    lesson: String,
}

fn schema() -> Value {
    let mut reasons: Vec<&str> = WON_REASONS.to_vec();
    reasons.extend(LOST_REASONS.iter().filter(|r| !WON_REASONS.contains(r)));
    let text = json!({ "type": "string" });
    json!({
        "type": "object",
        "properties": { "reason": { "type": "string", "enum": reasons }, "story": text, "turning_point": text, "competitor": text, "lesson": text },
        "required": ["reason", "story", "turning_point", "competitor", "lesson"]
    })
}

/// The agent's last rating before the deal closed, in a line, or nothing.
async fn last_rating(db: &PgPool, deal_id: Uuid) -> sqlx::Result<String> {
    let rating: Option<(String, i32, String, Ts)> = sqlx::query_as(
        "select health, likelihood, why, created_at from deal_health where deal_id = $1 and health is not null order by created_at desc limit 1",
    )
    .bind(deal_id)
    .fetch_optional(db)
    .await?;
    Ok(rating.map_or(String::new(), |(h, likelihood, why, at)| {
        format!("\nOn {} we rated it {} at {likelihood}% likely: {why}", at.format("%-d %b"), h.replace('_', " "))
    }))
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let deal = sqlx::query_as::<_, Closed>(
        "select r.deal_id, r.outcome, d.status, d.lead_id, d.value_cents, d.currency, d.close_date, d.notes, d.lost_reason, d.created_at, d.closed_at
         from deal_reviews r join deals d on d.id = r.deal_id where r.id = $1 and r.project_id = $2",
    )
    .bind(input.review_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the review was deleted")?;
    anyhow::ensure!(deal.status == deal.outcome, "the deal is {} now", deal.status);
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(deal.lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let shared = health::shared(db, deal.deal_id).await?;
    let rating = last_rating(db, deal.deal_id).await?;
    let rivals: Vec<(String, String)> =
        sqlx::query_as("select name, domain::text from competitors where project_id = $1 order by created_at").bind(lead.project_id).fetch_all(db).await?;

    let closed_at = deal.closed_at.unwrap_or_else(Utc::now);
    let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
    let planned = deal.close_date.map(|d| format!(" It was expected to close by {d}.")).unwrap_or_default();
    let reason = if deal.lost_reason.trim().is_empty() { String::new() } else { format!("\nThe reason our team gave: {}", deal.lost_reason.trim()) };
    let notes = if deal.notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", deal.notes.trim()) };
    let shared = if shared.is_empty() { String::new() } else { format!("\n{shared}") };
    let rivals_line = if rivals.is_empty() {
        String::new()
    } else {
        let list = rivals.iter().map(|(name, domain)| if name.is_empty() { domain.clone() } else { format!("{name} ({domain})") }).collect::<Vec<_>>();
        format!("\nCompetitors we track: {}.", list.join(", "))
    };
    let reasons = if deal.outcome == "won" { WON_REASONS } else { LOST_REASONS };
    let prompt = format!(
        "{us} {outcome} a deal with this lead on {closed}{value}, {days} days after it opened.{planned}{reason}{notes}{rating}{shared}{rivals_line}\n\n\
         What we sell: {name}. {description}\n\n\
         The lead and their company:\n{about}\n\nThe deal's story:\n{context}\n\n\
         Review why we {outcome} it.\n\
         1. reason: the main reason, one of {reasons}. {meanings}\n\
         2. story: 2 or 3 sentences on how the deal went, naming what they said or did and when.\n\
         3. turning_point: under 20 words, the moment that decided it, with its date when the evidence has one. Empty if no moment stands out.\n\
         4. competitor: the name of the competitor they chose or compared us with, only if the evidence names one. Otherwise empty.\n\
         5. lesson: under 25 words, what to repeat or do differently in the next deal like this, as an instruction.",
        us = profile.name,
        outcome = if deal.outcome == "won" { "won" } else { "lost" },
        closed = closed_at.format("%-d %b %Y"),
        days = (closed_at - deal.created_at).num_days(),
        name = profile.name,
        description = profile.description,
        about = serde_json::to_string_pretty(&about)?,
        reasons = reasons.join(", "),
        meanings = if deal.outcome == "won" {
            "value: they saw a clear return. fit: it matched what they needed. champion: someone inside pushed it through. timing: \
             it came at the right moment. price: the price won it. proof: a trial, demo or case study convinced them."
        } else {
            "price: too expensive. competitor: they chose someone else. timing: not now. no_decision: they never decided. fit: it \
             didn't match what they needed. went_dark: they stopped answering."
        },
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reviewing the deal with {first}…")).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.25, ..Call::writing(REVIEWER, prompt.clone(), schema()) };
    let a: Answer = ctx.ask(&call).await?;
    let reason = if reasons.contains(&a.reason.as_str()) { a.reason } else { "other".to_string() };
    // The list of competitors we track isn't evidence that this buyer talked about one.
    let competitor = competitor(&a.competitor, &rivals, &prompt.replace(&rivals_line, ""));
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();

    sqlx::query("update deal_reviews set reason = $2, story = $3, turning_point = $4, competitor = $5, lesson = $6 where id = $1 and run_id = $7")
        .bind(input.review_id)
        .bind(&reason)
        .bind(clip(&a.story, 600))
        .bind(clip(&a.turning_point, 200))
        .bind(&competitor)
        .bind(clip(&a.lesson, 240))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Reviewed the {} deal with {first}: {}", deal.outcome, reason.replace('_', " "))))
}

/// The competitor the agent named, kept only when the evidence it read names it, so the Win/loss view never counts a
/// made-up rival, and spelled as we track it when it's one of ours.
fn competitor(named: &str, rivals: &[(String, String)], evidence: &str) -> String {
    let named = named.trim();
    if named.is_empty() {
        return String::new();
    }
    let lower = named.to_lowercase();
    let evidence = evidence.to_lowercase();
    let root = |domain: &str| domain.split('.').next().unwrap_or_default().to_lowercase();
    if let Some((name, domain)) = rivals.iter().find(|(name, domain)| name.to_lowercase() == lower || domain.to_lowercase() == lower || root(domain) == lower) {
        let named_in_evidence = [name.to_lowercase(), domain.to_lowercase(), root(domain)].iter().any(|w| !w.is_empty() && evidence.contains(w.as_str()));
        if !named_in_evidence {
            return String::new();
        }
        return if name.is_empty() { domain.clone() } else { name.clone() };
    }
    if evidence.contains(&lower) { named.chars().take(80).collect() } else { String::new() }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn a_competitor_is_spelled_as_tracked_or_dropped_when_nothing_names_it() {
        let rivals = vec![("Gong".to_string(), "gong.io".to_string()), (String::new(), "chorus.ai".to_string())];
        assert_eq!(competitor(" gong ", &rivals, "They renewed GONG instead."), "Gong");
        assert_eq!(competitor("Chorus", &rivals, "they compared us with chorus"), "chorus.ai");
        assert_eq!(competitor("Gong", &rivals, "they went quiet"), "", "a tracked rival the evidence never names");
        assert_eq!(competitor("Outreach", &rivals, "they went with outreach in the end"), "Outreach");
        assert_eq!(competitor("Salesloft", &rivals, "they went quiet"), "", "the evidence never names it");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_closed_deal_is_reviewed_once_and_again_when_it_closes_again(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, email, company_name) values ($1, 'Noor Haddad', 'Noor', 'n@q.example', 'Quill Labs') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let lost: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and kind = 'lost' limit 1").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, status, value_cents, lost_reason, closed_at, created_at)
             values ($1, $2, $3, $4, 'lost', 900000, 'Picked Gong', now() - interval '1 day', now() - interval '30 days') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(lost)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query("insert into competitors (project_id, domain, name) values ($1, 'gong.io', 'Gong')").bind(project).execute(&db).await.unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 1);
        assert_eq!(queue_due(&db).await.unwrap(), 0, "not while one is being written");
        let shown = crate::deals::get(&db, org, deal).await.unwrap().review.unwrap();
        assert_eq!((shown["reason"].is_null(), shown["writing"].as_bool()), (true, Some(true)));

        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'deal_review' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({ "reason": "proof", "story": " They compared us with Gong. ", "turning_point": "", "competitor": "gong.io", "lesson": "Lead with the pilot." })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("lost a deal") && prompt.contains("29 days after") && prompt.contains("The reason our team gave: Picked Gong"), "{prompt}");
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(&db).await.unwrap();

        let shown = crate::deals::get(&db, org, deal).await.unwrap().review.unwrap();
        assert_eq!(shown["reason"], "other", "proof is a reason to win, not to lose");
        assert_eq!((shown["competitor"].as_str(), shown["story"].as_str(), shown["writing"].as_bool()), (Some("Gong"), Some("They compared us with Gong."), Some(false)));
        assert_eq!(queue_due(&db).await.unwrap(), 0, "reviewed");

        sqlx::query("update deals set status = 'won', closed_at = now() where id = $1").bind(deal).execute(&db).await.unwrap();
        assert!(crate::deals::get(&db, org, deal).await.unwrap().review.is_none(), "a review of the loss doesn't show on the win");
        assert_eq!(queue_due(&db).await.unwrap(), 1, "closed again, so reviewed again");
        let outcome: String = sqlx::query_scalar("select outcome from deal_reviews where deal_id = $1").bind(deal).fetch_one(&db).await.unwrap();
        assert_eq!(outcome, "won");
    }
}
