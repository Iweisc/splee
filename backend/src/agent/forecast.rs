//! Pipeline forecasts: the agent reads a project's open deals with what it knows about each (health, qualification, the
//! close plan and proposal) and sorts them into commit (closes this month), best case (could) and later, then picks up to 3
//! deals to work on this week and up to 3 that are slipping. Totals come from the deals' values, never the model. Every
//! Monday morning in the project's timezone, projects with open deals get one, posted to their chats.

use std::collections::HashSet;
use std::time::Duration;

use anyhow::Context;
use chrono::{Datelike, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, kind};
use crate::deals::{LAST_TOUCH, amount};
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;

pub const CATEGORIES: [&str; 3] = ["commit", "best_case", "later"];
/// Open deals one forecast reads, the ones closing soonest first.
const MAX_DEALS: i64 = 60;
const MAX_PICKS: usize = 3;
const FORECASTER: &str = "You run the weekly pipeline review for a B2B sales team using Splee, an outbound sales tool, like a \
sharp, honest sales manager. Use only the deals and facts given, and be conservative: a deal is commit only when the evidence \
says it will sign this month.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Placed {
    pub deal_id: Uuid,
    pub category: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pick {
    pub deal_id: Uuid,
    /// What to do this week for a focus deal, or why it's slipping.
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Forecast {
    pub headline: String,
    pub month_end: NaiveDate,
    pub deals: Vec<Placed>,
    pub focus: Vec<Pick>,
    pub slipping: Vec<Pick>,
}

/// Queues a forecast for every project where it's Monday from 8am, that has open deals and none from the last 6 days.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "with due as (
           insert into pipeline_forecasts (project_id, auto)
           select p.id, true from projects p join project_settings s on s.project_id = p.id and s.pipeline_forecast
           where p.org_id is not null
             and extract(isodow from now() at time zone s.timezone) = 1 and extract(hour from now() at time zone s.timezone) >= 8
             and exists (select 1 from deals d where d.project_id = p.id and d.status = 'open')
             and not exists (select 1 from pipeline_forecasts f where f.project_id = p.id and f.created_at > now() - interval '6 days')
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id",
    )
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Writes a forecast for the project now. Returns the run.
pub async fn request(db: &PgPool, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid = sqlx::query_scalar("insert into pipeline_forecasts (project_id) values ($1) returning id").bind(project_id).fetch_one(&mut *tx).await?;
    let run = start(&mut tx, id, project_id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::PIPELINE_FORECAST, Some(project_id), Some(org_id), json!({ "forecast_id": id })).await?;
    sqlx::query("update pipeline_forecasts set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "pipeline forecasts").await
    });
}

/// The project's latest forecast: `status` is ready, writing or failed.
#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub forecast: Option<Value>,
    pub created_at: Ts,
    pub status: String,
}

pub async fn latest(db: &PgPool, project_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select f.id, f.forecast, f.created_at,
                case when f.forecast is not null then 'ready' when r.status in ('queued', 'running') then 'writing' else 'failed' end as status
         from pipeline_forecasts f left join agent_runs r on r.id = f.run_id
         where f.project_id = $1 order by f.created_at desc limit 1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await
}

#[derive(Deserialize)]
struct Input {
    forecast_id: Uuid,
}

/// An open deal with what Splee knows about it.
#[derive(sqlx::FromRow)]
struct OpenDeal {
    id: Uuid,
    company: String,
    lead: String,
    title: String,
    stage: String,
    probability: i32,
    value_cents: i64,
    currency: String,
    close_date: Option<NaiveDate>,
    quiet_days: i32,
    owner: Option<String>,
    health: Option<String>,
    likelihood: Option<i32>,
    health_why: Option<String>,
    confirmed: Option<i64>,
    unknown: Option<String>,
    plan: Option<String>,
    proposal: Option<String>,
}

/// `{LAST_TOUCH}` is filled in where it's used, like the board's Quiet badge counts from it.
const OPEN_DEALS: &str = "select d.id, l.company_name as company, l.full_name as lead, l.title, s.name as stage, s.probability, d.value_cents, d.currency,
            d.close_date, extract(day from now() - {LAST_TOUCH})::int as quiet_days,
            coalesce(nullif(u.name, ''), u.email::text) as owner, h.health, h.likelihood, h.why as health_why,
            (select count(*) from jsonb_array_elements(q.items) i where i->>'status' = 'confirmed') as confirmed,
            (select string_agg(i->>'key', ', ') from jsonb_array_elements(q.items) i where i->>'status' = 'unknown') as unknown,
            (select count(*) filter (where st.done_at is not null) || ' of ' || count(*) || ' steps done'
               from close_plans cp join close_plan_steps st on st.plan_id = cp.id where cp.deal_id = d.id having count(*) > 0) as plan,
            (select case when pr.accepted_at is not null then 'accepted' when pr.views > 0 then 'opened ' || pr.views || ' times, not accepted'
                         else 'sent, never opened' end from proposals pr where pr.deal_id = d.id and pr.title is not null) as proposal
     from deals d join deal_stages s on s.id = d.stage_id join leads l on l.id = d.lead_id left join users u on u.id = l.owner_id
     left join lateral (select health, likelihood, why from deal_health where deal_id = d.id and health is not null order by created_at desc limit 1) h on true
     left join lateral (select items from deal_qualification where deal_id = d.id and items is not null order by created_at desc limit 1) q on true
     where d.project_id = $1 and d.status = 'open'
     order by d.close_date nulls last, d.value_cents desc limit $2";

#[derive(Deserialize)]
struct Answer {
    headline: String,
    deals: Vec<AnswerPlaced>,
    focus: Vec<AnswerPick>,
    slipping: Vec<AnswerPick>,
}

#[derive(Deserialize)]
struct AnswerPlaced {
    id: String,
    category: String,
    why: String,
}

#[derive(Deserialize)]
struct AnswerPick {
    id: String,
    text: String,
}

fn schema() -> Value {
    let pick = json!({ "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "text": { "type": "string" } }, "required": ["id", "text"] } });
    json!({
        "type": "object",
        "properties": {
            "headline": { "type": "string" },
            "deals": { "type": "array", "items": { "type": "object", "properties": {
                "id": { "type": "string" }, "category": { "type": "string", "enum": CATEGORIES }, "why": { "type": "string" }
            }, "required": ["id", "category", "why"] } },
            "focus": pick, "slipping": pick,
        },
        "required": ["headline", "deals", "focus", "slipping"]
    })
}

fn month_end(today: NaiveDate) -> NaiveDate {
    let (y, m) = if today.month() == 12 { (today.year() + 1, 1) } else { (today.year(), today.month() + 1) };
    NaiveDate::from_ymd_opt(y, m, 1).and_then(|d| d.pred_opt()).unwrap_or(today)
}

fn describe(i: usize, d: &OpenDeal) -> String {
    let mut line = format!("[d{}] {} ({}, {}). {} stage, {}% by stage", i + 1, d.company, d.lead, d.title, d.stage, d.probability);
    if d.value_cents > 0 {
        line += &format!(", {} {}", amount(d.value_cents), d.currency.to_uppercase());
    }
    line += &match d.close_date {
        Some(date) => format!(", expected to close {date}"),
        None => ", no close date".to_string(),
    };
    line += &format!(", last touch {} days ago", d.quiet_days);
    if let Some(owner) = &d.owner {
        line += &format!(", owned by {owner}");
    }
    line += ".";
    if let (Some(h), Some(l)) = (&d.health, d.likelihood) {
        line += &format!(" Our rating: {} at {l}%: {}.", h.replace('_', " "), d.health_why.as_deref().unwrap_or_default().trim_end_matches('.'));
    }
    if let Some(n) = d.confirmed {
        line += &format!(" MEDDICC: {n} of 8 confirmed{}.", d.unknown.as_deref().map(|u| format!(", unknown {u}")).unwrap_or_default());
    }
    if let Some(plan) = &d.plan {
        line += &format!(" Close plan: {plan}.");
    }
    if let Some(proposal) = &d.proposal {
        line += &format!(" Proposal: {proposal}.");
    }
    line
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (project_id, auto, written): (Uuid, bool, bool) = sqlx::query_as("select project_id, auto, forecast is not null from pipeline_forecasts where id = $1 and project_id = $2")
        .bind(input.forecast_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the forecast was deleted")?;
    if written {
        return Ok(Outcome::new("Already written"));
    }
    let deals: Vec<OpenDeal> = sqlx::query_as(&OPEN_DEALS.replace("{LAST_TOUCH}", LAST_TOUCH)).bind(project_id).bind(MAX_DEALS).fetch_all(db).await?;
    anyhow::ensure!(!deals.is_empty(), "the project has no open deals");
    let closed: Vec<(String, String, i64, String, Option<String>)> = sqlx::query_as(
        "select l.company_name, d.status, d.value_cents, d.currency, r.reason from deals d join leads l on l.id = d.lead_id
         left join deal_reviews r on r.deal_id = d.id and r.outcome = d.status
         where d.project_id = $1 and d.status <> 'open' and d.closed_at > now() - interval '7 days' order by d.closed_at desc limit 20",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let (_, profile) = load_project(ctx, project_id).await?;

    let today = Utc::now().date_naive();
    let end = month_end(today);
    let listed = deals.iter().enumerate().map(|(i, d)| describe(i, d)).collect::<Vec<_>>().join("\n");
    let closed = if closed.is_empty() {
        "None.".to_string()
    } else {
        closed
            .iter()
            .map(|(company, status, cents, currency, reason)| {
                format!("- {status} {company}, {} {}{}", amount(*cents), currency.to_uppercase(), reason.as_deref().map(|r| format!(", on {}", r.replace('_', " "))).unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let prompt = format!(
        "Today is {today}, and the month ends {end}. {us}'s open deals, each with its id:\n{listed}\n\n\
         Deals won or lost in the last 7 days:\n{closed}\n\n\
         1. deals: every deal id above, with a category and why in under 15 words. commit: the evidence says it signs by {end}. \
         best_case: it could sign by {end} if things go well. later: it won't sign this month.\n\
         2. focus: up to {MAX_PICKS} deals where work this week matters most for the month, with the one action to take, under 20 words, starting with a verb.\n\
         3. slipping: up to {MAX_PICKS} deals that are slipping, with why in under 20 words, citing the evidence. Empty if none are.\n\
         4. headline: 2 sentences on what the month looks like and the biggest risk to it. Name companies, not deal ids.",
        us = profile.name,
    );
    ctx.log(format!("reviewing {} open deals…", deals.len())).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.5, ..Call::writing(FORECASTER, prompt, schema()) };
    let forecast = tidy(ctx.ask::<Answer>(&call).await?, &deals, end);

    let saved = sqlx::query("update pipeline_forecasts set forecast = $2 where id = $1 and run_id = $3 and forecast is null")
        .bind(input.forecast_id)
        .bind(json!(forecast))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    if saved.rows_affected() > 0 && auto {
        announce(&ctx.state, project_id, &forecast, &deals).await;
    }
    let commit = forecast.deals.iter().filter(|p| p.category == "commit").count();
    Ok(Outcome::new(format!("Forecast {} open deals: {commit} to commit", deals.len())))
}

/// The value of each category's deals, per currency, like "12,000 USD + 900 EUR".
pub fn totals(category: &str, forecast: &Forecast, values: &[(Uuid, i64, String)]) -> String {
    let mut sums: Vec<(String, i64)> = vec![];
    for placed in forecast.deals.iter().filter(|p| p.category == category) {
        if let Some((_, cents, currency)) = values.iter().find(|(id, ..)| *id == placed.deal_id) {
            match sums.iter_mut().find(|(c, _)| c == currency) {
                Some((_, sum)) => *sum += cents,
                None => sums.push((currency.clone(), *cents)),
            }
        }
    }
    if sums.is_empty() {
        return "0".to_string();
    }
    sums.iter().map(|(currency, cents)| format!("{} {}", amount(*cents), currency.to_uppercase())).collect::<Vec<_>>().join(" + ")
}

async fn announce(state: &AppState, project_id: Uuid, forecast: &Forecast, deals: &[OpenDeal]) {
    let values: Vec<(Uuid, i64, String)> = deals.iter().map(|d| (d.id, d.value_cents, d.currency.clone())).collect();
    let summary = format!(
        "Commit {}, best case {} by {}. {}",
        totals("commit", forecast, &values),
        totals("best_case", forecast, &values),
        forecast.month_end.format("%-d %b"),
        forecast.headline
    );
    let company = |id: Uuid| deals.iter().find(|d| d.id == id).map(|d| d.company.clone()).unwrap_or_default();
    let focus = forecast.focus.iter().map(|p| format!("{}: {}", company(p.deal_id), p.text)).collect::<Vec<_>>().join("\n");
    let quote = (!focus.is_empty()).then(|| format!("This week:\n{focus}"));
    let link = format!("{}/app/pipeline?view=forecast", state.cfg.public_url);
    let alert = ChatAlert { label: "Pipeline forecast", summary: &summary, quote: quote.as_deref(), link: &link, link_text: "Open the forecast" };
    hooks::post_chat(&state.db, project_id, &alert).await;
}

/// Keeps one placement per real deal, the first winning, and puts any deal the model skipped in later, so the totals cover
/// the whole pipeline. Picks must name a real deal, once each.
fn tidy(a: Answer, deals: &[OpenDeal], month_end: NaiveDate) -> Forecast {
    let deal = |id: &str| id.trim().trim_start_matches(['d', 'D']).parse::<usize>().ok().filter(|&i| i >= 1 && i <= deals.len()).map(|i| deals[i - 1].id);
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let mut placed: Vec<Placed> = vec![];
    for p in &a.deals {
        if let Some(id) = deal(&p.id).filter(|id| !placed.iter().any(|x| x.deal_id == *id)) {
            let category = if CATEGORIES.contains(&p.category.as_str()) { p.category.clone() } else { "later".to_string() };
            placed.push(Placed { deal_id: id, category, why: clip(&p.why, 160) });
        }
    }
    for d in deals {
        if !placed.iter().any(|p| p.deal_id == d.id) {
            placed.push(Placed { deal_id: d.id, category: "later".to_string(), why: String::new() });
        }
    }
    let picks = |list: &[AnswerPick]| {
        let mut seen = HashSet::new();
        list.iter().filter_map(|p| deal(&p.id).filter(|id| seen.insert(*id)).map(|id| Pick { deal_id: id, text: clip(&p.text, 200) })).take(MAX_PICKS).collect()
    };
    Forecast { headline: clip(&a.headline, 600), month_end, deals: placed, focus: picks(&a.focus), slipping: picks(&a.slipping) }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn the_month_ends_on_its_last_day() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert_eq!(month_end(d(2026, 11, 14)), d(2026, 11, 30));
        assert_eq!(month_end(d(2026, 12, 1)), d(2026, 12, 31));
        assert_eq!(month_end(d(2028, 2, 3)), d(2028, 2, 29));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_forecast_places_every_open_deal_and_totals_come_from_their_values(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = 'Proposal'").bind(org).fetch_one(&db).await.unwrap();
        let mut ids = vec![];
        for (name, company, cents) in [("Noor Haddad", "Quill Labs", 1_200_000i64), ("Owen Fairley", "Tidewell", 500_000), ("Ada Okafor", "Tessel", 300_000)] {
            let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, company_name) values ($1, $2, $3) returning id")
                .bind(project)
                .bind(name)
                .bind(company)
                .fetch_one(&db)
                .await
                .unwrap();
            let deal: Uuid = sqlx::query_scalar("insert into deals (org_id, project_id, lead_id, stage_id, value_cents, close_date) values ($1, $2, $3, $4, $5, current_date + 10) returning id")
                .bind(org)
                .bind(project)
                .bind(lead)
                .bind(stage)
                .bind(cents)
                .fetch_one(&db)
                .await
                .unwrap();
            ids.push(deal);
        }
        sqlx::query("insert into deal_health (project_id, deal_id, health, likelihood, why) values ($1, $2, 'at_risk', 35, 'Quiet for 3 weeks')")
            .bind(project)
            .bind(ids[0])
            .execute(&db)
            .await
            .unwrap();

        request(&db, project, org).await.unwrap();
        assert_eq!(latest(&db, project).await.unwrap().unwrap().status, "writing");
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'pipeline_forecast' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "headline": " Tidewell carries the month. ",
                "deals": [
                    { "id": "d2", "category": "commit", "why": "Verbal yes" },
                    { "id": "D1", "category": "best_case", "why": "Quiet" },
                    { "id": "d2", "category": "later", "why": "A repeat that must not win" },
                    { "id": "d9", "category": "commit", "why": "No such deal" }
                ],
                "focus": [{ "id": "d1", "text": "Call Noor" }, { "id": "d1", "text": "Twice" }, { "id": "d7", "text": "Nobody" }],
                "slipping": [{ "id": "d1", "text": "Quiet for 3 weeks" }]
            })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("[d1] Quill Labs") && prompt.contains("12,000 USD") && prompt.contains("Our rating: at risk at 35%: Quiet for 3 weeks."), "{prompt}");

        let saved = latest(&db, project).await.unwrap().unwrap();
        assert_eq!(saved.status, "ready");
        let f: Forecast = serde_json::from_value(saved.forecast.unwrap()).unwrap();
        assert_eq!(f.headline, "Tidewell carries the month.");
        assert_eq!(f.deals.len(), 3, "every open deal once, and no made-up one");
        let category = |id: Uuid| f.deals.iter().find(|p| p.deal_id == id).unwrap().category.clone();
        assert_eq!((category(ids[0]), category(ids[1]), category(ids[2])), ("best_case".into(), "commit".into(), "later".into()), "a skipped deal is later");
        assert_eq!((f.focus.len(), f.slipping.len()), (1, 1), "picks name a real deal once");
        let values = vec![(ids[0], 1_200_000, "USD".to_string()), (ids[1], 500_000, "USD".to_string()), (ids[2], 300_000, "USD".to_string())];
        assert_eq!((totals("commit", &f, &values), totals("best_case", &f, &values)), ("5,000 USD".to_string(), "12,000 USD".to_string()));
        assert_eq!(queue_due(&db).await.unwrap(), 0, "one from today already");
    }
}
