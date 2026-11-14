//! The deal desk's read on a discount ask (crate::deal_desk): the agent reads the deal, its proposal against the price list,
//! the reason given, what Splee knows about the buyer, the deal's latest health, and how the organization's discounted
//! proposals did over the last year, and recommends approving, countering at a smaller discount, or declining, with what to
//! ask the buyer for in return.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, briefs};
use crate::deals::amount;
use crate::models::Lead;
use crate::proposals::{Item, first_year};

const DESK: &str = "You run the deal desk for a B2B sales team using Splee, an outbound sales tool. You protect margin without \
losing deals worth winning. Use only the facts given and never invent any. Call people by name or they, never a guessed he or \
she. Plain text only.";

#[derive(Deserialize)]
struct Input {
    approval_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Ask {
    discount_pct: f64,
    reason: String,
    asked_by: Option<String>,
    items: Json<Vec<Item>>,
    lead_id: Uuid,
    org_id: Uuid,
    stage: String,
    currency: String,
    created_days: i32,
    close_date: Option<chrono::NaiveDate>,
    limit: Option<i32>,
    health: Option<String>,
    likelihood: Option<i32>,
    health_why: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Read {
    /// approve, counter or decline.
    pub verdict: String,
    /// The discount to offer instead, when countering.
    pub counter_pct: Option<f64>,
    pub summary: String,
    pub reasons: Vec<String>,
    /// What to get from the buyer in return, like a longer term.
    pub ask_for: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "verdict": { "type": "string", "enum": ["approve", "counter", "decline"] },
            "counter_pct": { "type": "number" },
            "summary": { "type": "string" },
            "reasons": { "type": "array", "items": { "type": "string" } },
            "ask_for": { "type": "string" }
        },
        "required": ["verdict", "counter_pct", "summary", "reasons", "ask_for"]
    })
}

/// How the organization's closed proposals of the last year did, by discount: none, up to the limit, past it.
fn history(closed: &[(String, f64)], limit: Option<i32>) -> String {
    let cut = f64::from(limit.unwrap_or(0));
    let line = |name: &str, keep: &dyn Fn(f64) -> bool| {
        let (won, lost) = closed.iter().filter(|(_, d)| keep(*d)).fold((0, 0), |(w, l), (s, _)| if s == "won" { (w + 1, l) } else { (w, l + 1) });
        format!("- {name}: {won} won, {lost} lost")
    };
    if closed.is_empty() {
        return "No closed deals with a proposal in the last year.".into();
    }
    let mut lines = vec![line("No discount", &|d| d <= 0.0)];
    if limit.is_some() {
        lines.push(line(&format!("Up to the {cut}% limit"), &|d| d > 0.0 && d <= cut));
        lines.push(line(&format!("Past the {cut}% limit"), &|d| d > cut));
    } else {
        lines.push(line("Any discount", &|d| d > 0.0));
    }
    lines.join("\n")
}

fn tidy(r: Read, asked: f64) -> Read {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    let verdict = if ["approve", "counter", "decline"].contains(&r.verdict.as_str()) { r.verdict } else { "decline".into() };
    // A counter is only a counter when it's a smaller discount than the one asked for.
    let counter_pct = r.counter_pct.filter(|c| verdict == "counter" && *c > 0.0 && *c < asked).map(|c| (c * 100.0).round() / 100.0);
    let verdict = if verdict == "counter" && counter_pct.is_none() { "decline".into() } else { verdict };
    Read {
        verdict,
        counter_pct,
        summary: clip(&r.summary, 400),
        reasons: r.reasons.iter().map(|x| clip(x, 300)).filter(|x| !x.is_empty()).take(3).collect(),
        ask_for: clip(&r.ask_for, 300),
    }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let a: Ask = sqlx::query_as(
        "select a.discount_pct::float8 as discount_pct, a.reason, coalesce(nullif(u.name, ''), u.email::text) as asked_by, p.items, d.lead_id,
                d.org_id, s.name as stage, d.currency, (current_date - d.created_at::date)::int as created_days, d.close_date,
                o.discount_limit_pct as limit, h.health, h.likelihood, h.why as health_why
         from discount_approvals a join proposals p on p.id = a.proposal_id join deals d on d.id = p.deal_id
         join deal_stages s on s.id = d.stage_id join organizations o on o.id = d.org_id left join users u on u.id = a.asked_by
         left join lateral (select health, likelihood, why from deal_health where deal_id = d.id order by created_at desc limit 1) h on true
         where a.id = $1 and a.status = 'asked'",
    )
    .bind(input.approval_id)
    .fetch_optional(db)
    .await?
    .context("the ask was decided or replaced")?;
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(a.lead_id).fetch_one(db).await?;
    let (_, profile) = load_project(ctx, lead.project_id).await?;
    let context = briefs::gather(db, &lead, a.org_id).await?;
    let closed: Vec<(String, f64)> = sqlx::query_as(
        "select d.status, p.discount_pct::float8 from proposals p join deals d on d.id = p.deal_id
         where d.org_id = $1 and d.status in ('won', 'lost') and d.closed_at > now() - interval '365 days' and p.items <> '[]'",
    )
    .bind(a.org_id)
    .fetch_all(db)
    .await?;

    let cur = a.currency.to_uppercase();
    let money = |n: f64| format!("{} {cur}", amount((n * 100.0).round() as i64));
    let items: Vec<String> = a
        .items
        .iter()
        .map(|i| {
            let per = match i.period.as_str() {
                "month" => " a month",
                "year" => " a year",
                _ => " once",
            };
            let list = match i.list_price {
                Some(l) if l > 0.0 => format!(", list {} ({:.0}% off)", money(l), ((l - i.unit_price) / l * 100.0).max(0.0)),
                _ => ", not on the price list".into(),
            };
            format!("- {} x {} at {}{per}{list}", i.quantity, i.name, money(i.unit_price))
        })
        .collect();
    let at_list: f64 = first_year(&a.items.iter().map(|i| Item { unit_price: i.list_price.unwrap_or(i.unit_price), ..i.clone() }).collect::<Vec<_>>());
    let health = match (&a.health, a.likelihood) {
        (Some(h), Some(l)) => format!("The deal's latest health: {}, {l}% likely to close. {}", h.replace('_', " "), a.health_why.as_deref().unwrap_or("")),
        _ => "No health check on the deal yet.".into(),
    };
    let close = a.close_date.map(|d| format!(", expected to close by {d}")).unwrap_or_default();
    let limit = a.limit.map(|l| format!("{l}%")).unwrap_or_else(|| "none".into());
    let prompt = format!(
        "{who} asks to approve a {discount:.0}% discount for {company}. The team's limit without approval is {limit}.\n\
         Their reason: {reason}\n\n\
         The deal: {stage} stage, open {days} days{close}. {health}\n\
         The proposal, in {cur}:\n{items}\n\
         First year as proposed: {proposed}. At list prices: {list}.\n\n\
         What we sell: {name}. {description}\n\n\
         The buyer and what Splee knows about them:\n{context}\n\n\
         How the team's proposals did over the last year, by discount:\n{history}\n\n\
         Recommend what the approver should do.\n\
         1. verdict: approve, counter or decline.\n\
         2. counter_pct: when you counter, the discount to offer instead, smaller than {discount:.0}. Otherwise 0.\n\
         3. summary: one or two sentences, under 40 words, the approver reads first, naming the deal and the call.\n\
         4. reasons: up to 3 reasons from the facts above, each under 30 words, the most important first.\n\
         5. ask_for: in under 25 words, what to get from the buyer in return for any discount, like a longer term, a case study or signing by a date. Empty when you decline.",
        who = a.asked_by.as_deref().unwrap_or("A teammate"),
        discount = a.discount_pct,
        company = if lead.company_name.trim().is_empty() { lead.full_name.trim() } else { lead.company_name.trim() },
        reason = if a.reason.is_empty() { "(none given)" } else { &a.reason },
        stage = a.stage,
        days = a.created_days,
        items = items.join("\n"),
        proposed = money(first_year(&a.items)),
        list = money(at_list),
        name = profile.name,
        description = profile.description,
        history = history(&closed, a.limit),
    );
    ctx.log("reading the deal and the discount asked for…".to_string()).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.3, ..Call::writing(DESK, prompt, schema()) };
    let read = tidy(ctx.ask(&call).await?, a.discount_pct);
    sqlx::query("update discount_approvals set read = $2 where id = $1").bind(input.approval_id).bind(Json(&read)).execute(db).await?;
    Ok(Outcome::new(format!("Recommended to {} the discount", read.verdict)))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use sqlx::PgPool;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn a_counter_must_be_smaller_than_the_ask_and_history_buckets_by_the_limit() {
        let read = |verdict: &str, counter: Option<f64>| Read { verdict: verdict.into(), counter_pct: counter, summary: " ok ".into(), reasons: vec![], ask_for: String::new() };
        assert_eq!(tidy(read("counter", Some(15.0)), 25.0).counter_pct, Some(15.0));
        assert_eq!(tidy(read("counter", Some(30.0)), 25.0).verdict, "decline", "a bigger counter isn't a counter");
        assert_eq!(tidy(read("approve", Some(10.0)), 25.0).counter_pct, None);
        assert_eq!(tidy(read("maybe", None), 25.0).verdict, "decline");
        let closed = [("won".to_string(), 0.0), ("won".to_string(), 10.0), ("lost".to_string(), 30.0), ("won".to_string(), 30.0)];
        assert_eq!(history(&closed, Some(15)), "- No discount: 1 won, 0 lost\n- Up to the 15% limit: 1 won, 0 lost\n- Past the 15% limit: 1 won, 1 lost");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_held_proposal_is_asked_read_and_approved_and_then_the_buyer_sees_it(db: PgPool) {
        use crate::agent::practice::fixture;
        use crate::proposals::{self, Changes};
        let t = fixture::team(&db).await;
        let state = crate::autopilot::tests::state(db.clone());
        sqlx::query("update organizations set currency = 'usd', discount_limit_pct = 15 where id = $1").bind(t.org).execute(&db).await.unwrap();
        sqlx::query("update deals set currency = 'usd' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        let seats: Uuid = sqlx::query_scalar("insert into products (org_id, name, price, period) values ($1, 'Seats', 200, 'month') returning id")
            .bind(t.org)
            .fetch_one(&db)
            .await
            .unwrap();
        let items = |price: f64| Changes {
            title: Some("Fieldnote for Lumenfold".into()),
            intro: None,
            items: Some(vec![crate::deal_desk::tests::item(price, Some(seats))]),
            valid_until: None,
        };
        proposals::update(&db, t.project, t.deal, items(180.0)).await.unwrap();
        let p = proposals::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!((p.discount_pct, p.held), (10.0, false), "10% off is inside the limit");
        assert!(crate::deal_desk::ask(&state, t.org, t.deal, t.sam, "Multi-year").await.is_err(), "nothing to approve");

        proposals::update(&db, t.project, t.deal, items(150.0)).await.unwrap();
        let p = proposals::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!((p.discount_pct, p.held), (25.0, true));
        let page = proposals::public(&db, &p.token).await.unwrap().unwrap();
        assert!(page.held && page.items.is_empty(), "the buyer doesn't see the prices yet");
        let accept = || proposals::Acceptance { name: "Ines Duarte".into(), title: String::new() };
        assert!(proposals::accept(&state, &p.token, accept()).await.is_err(), "nor accept it");

        assert!(crate::deal_desk::ask(&state, t.org, t.deal, t.sam, "  ").await.is_err(), "an ask needs a reason");
        let run_id = crate::deal_desk::ask(&state, t.org, t.deal, t.sam, " They'll sign a two-year term ").await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run_id).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: state.clone(), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(run_id, |_| {
            json!({ "verdict": "counter", "counter_pct": 18, "summary": "Counter at 18%.", "reasons": ["Past-limit deals lose", ""], "ask_for": "A two-year term" })
        });
        run(&ctx).await.unwrap();
        let prompt = super::super::fake::prompts(run_id).first().cloned().unwrap();
        assert!(prompt.contains("Sam Ito asks to approve a 25% discount") && prompt.contains("Their reason: They'll sign a two-year term"), "{prompt}");
        assert!(prompt.contains("- 8 x Seats at 150 USD a month, list 200 USD (25% off)") && prompt.contains("At list prices: 19,200 USD."), "{prompt}");

        let a = crate::deal_desk::waiting(&db, t.org).await.unwrap().pop().unwrap();
        let read: Read = serde_json::from_value(a.read.clone().unwrap()).unwrap();
        assert_eq!((read.verdict.as_str(), read.counter_pct, read.reasons.len()), ("counter", Some(18.0), 1));
        crate::deal_desk::decide(&state, t.org, a.id, t.dana, true, "Get the two years in writing").await.unwrap();
        assert!(crate::deal_desk::decide(&state, t.org, a.id, t.dana, false, "").await.is_err(), "decided once");
        let p = proposals::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert!(!p.held);
        assert_eq!(p.approval.as_ref().map(|a| (a.status.as_str(), a.decided_by.as_deref())), Some(("approved", Some("Dana Reyes"))));
        assert!(!proposals::public(&db, &p.token).await.unwrap().unwrap().items.is_empty());

        proposals::update(&db, t.project, t.deal, items(120.0)).await.unwrap();
        assert!(proposals::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap().held, "a deeper discount than approved is held again");
    }
}
