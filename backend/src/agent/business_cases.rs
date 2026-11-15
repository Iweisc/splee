//! Business cases (see business_cases.rs): the agent works out what the product is worth to the buyer from the
//! conversation, line by line, and says which numbers they gave and which it estimated.

use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::types::Json;
use uuid::Uuid;

use super::deal_rooms::load;
use super::sdk::Call;
use super::write::materials;
use super::{Ctx, Outcome};
use crate::business_cases::{Content, Line, MAX_NUMBER};

const WRITER: &str = "You write business cases for B2B buyers, for Splee, an outbound sales tool, like a careful analyst the \
buyer's finance team would trust. The buyer reads it and forwards it. Use only what you're given. Never invent quotes, \
customers or facts. Mark every number you estimate as an estimate, keep estimates modest, and leave out anything internal. \
Plain text only, no markdown.";

const MAX_LINES: usize = 5;
const PERIODS: [&str; 3] = ["week", "month", "year"];

#[derive(Deserialize)]
struct CaseInput {
    case_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    write: bool,
    why: String,
    case: Content,
}

fn clip(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

/// The lines that add up to something, each number in range, at most MAX_LINES.
fn tidy(c: Content) -> Content {
    let number = |n: f64| if n.is_finite() { n.clamp(0.0, MAX_NUMBER) } else { 0.0 };
    let lines = c
        .lines
        .into_iter()
        .filter(|l| PERIODS.contains(&l.per.as_str()) && !l.label.trim().is_empty())
        .map(|l| Line {
            label: clip(&l.label, 120),
            quantity: number(l.quantity),
            unit: clip(&l.unit, 40),
            unit_value: number(l.unit_value),
            per: l.per,
            count: number(l.count),
            count_unit: clip(&l.count_unit, 60),
            basis: if l.basis == "said" { "said".into() } else { "estimate".into() },
            source: clip(&l.source, 300),
        })
        .filter(|l| crate::business_cases::annual(&l.per, l.numbers()) > 0.0)
        .take(MAX_LINES)
        .collect();
    Content {
        title: clip(&c.title, 160),
        summary: clip(&c.summary, 600),
        problem: clip(&c.problem, 1200),
        lines,
        outcomes: c.outcomes.iter().map(|o| clip(o, 240)).filter(|o| !o.is_empty()).take(4).collect(),
        waiting: clip(&c.waiting, 400),
    }
}

fn schema() -> Value {
    let line = json!({
        "type": "object",
        "properties": {
            "label": { "type": "string" },
            "quantity": { "type": "number" },
            "unit": { "type": "string" },
            "unit_value": { "type": "number" },
            "per": { "type": "string", "enum": PERIODS },
            "count": { "type": "number" },
            "count_unit": { "type": "string" },
            "basis": { "type": "string", "enum": ["said", "estimate"] },
            "source": { "type": "string" }
        },
        "required": ["label", "quantity", "unit", "unit_value", "per", "count", "count_unit", "basis", "source"]
    });
    json!({
        "type": "object",
        "properties": {
            "write": { "type": "boolean" },
            "why": { "type": "string" },
            "case": {
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "summary": { "type": "string" },
                    "problem": { "type": "string" },
                    "lines": { "type": "array", "items": line },
                    "outcomes": { "type": "array", "items": { "type": "string" } },
                    "waiting": { "type": "string" }
                },
                "required": ["title", "summary", "problem", "lines", "outcomes", "waiting"]
            }
        },
        "required": ["write", "why", "case"]
    })
}

pub async fn write(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: CaseInput = ctx.input()?;
    let db = ctx.db();
    let project_id = ctx.project_id()?;
    let (deal_id, value_cents, currency): (Uuid, i64, String) = sqlx::query_as(
        "select c.deal_id, d.value_cents, d.currency from business_cases c join deals d on d.id = c.deal_id where c.id = $1 and c.project_id = $2",
    )
    .bind(input.case_id)
    .bind(project_id)
    .fetch_optional(db)
    .await?
    .context("the business case was deleted")?;
    let d = load(ctx, deal_id).await?;
    let materials = materials(ctx, project_id).await?;
    let company = if d.lead.company_name.trim().is_empty() { "them".to_string() } else { d.lead.company_name.trim().to_string() };
    let prompt = format!(
        "{us} has a deal in the {stage} stage with {who}, worth {value} {currency}.\n\n\
         What we sell: {name}. {description}\n\n\
         What we can point to:\n{materials}\n\n\
         The conversation and what else Splee knows:\n{context}\n\n\
         Write the business case for {company} buying {name}, which our contact there forwards to whoever signs.\n\
         1. write: false only when the conversation says too little about their problem to put a number on it.\n\
         2. why: under 25 words, what the case rests on, or what's missing.\n\
         3. case:\n\
         - title: under 12 words, naming {company}.\n\
         - summary: two sentences for whoever signs: the problem and what fixing it changes.\n\
         - problem: 40 to 80 words about {company} in the third person, since it's forwarded, in their words where you have them.\n\
         - lines: 1 to {MAX_LINES} things it's worth to them in money, from what they said, like time back, deals not lost or \
         a tool they stop paying for. Each is quantity units, each worth unit_value {currency}, every per (week, month or \
         year), for count count_units. quantity is for one of them, like 3 hours at 45 every week for each of 6 account \
         managers when they lose 18 hours a week between them, so each number is one they can check. Use a count of 1 \
         and a fitting count_unit when there's only one. basis: said when they gave the number, estimate when you did, with source \
         quoting what they said or saying how you estimated, under 30 words. Estimate cautiously, like a typical loaded \
         hourly cost for the role, and never count the same gain twice.\n\
         - outcomes: up to 3 short things that change and aren't money, from what they said.\n\
         - waiting: one sentence on what waiting costs them beyond the money, like another quarter of missed forecasts.\n\
         The page adds up the lines and the buyer can change them, so never write a total, a sum or a payback period in \
         the text.\n\
         {language} The case too.",
        us = d.us,
        stage = d.stage,
        who = d.who(),
        value = crate::deals::amount(value_cents),
        name = d.name,
        description = d.description,
        context = d.context,
        language = d.language,
    );
    ctx.log(format!("working out what {} is worth to {company}…", d.name)).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(WRITER, prompt, schema()) };
    let answer: Answer = ctx.ask(&call).await?;
    let case = tidy(answer.case);
    if !answer.write || case.lines.is_empty() || case.title.is_empty() {
        let why = if answer.why.trim().is_empty() { "the conversation says too little about their problem to put a number on it" } else { answer.why.trim() };
        sqlx::query("update business_cases set skipped = $2 where id = $1 and run_id = $3").bind(input.case_id).bind(clip(why, 300)).bind(ctx.run.id).execute(db).await?;
        return Ok(Outcome::new(format!("No business case: {why}")));
    }
    let total = case.annual(None);
    sqlx::query(
        "update business_cases set business_case = $2, numbers = null, edited_by = '', edited_at = null, skipped = '' where id = $1 and run_id = $3",
    )
    .bind(input.case_id)
    .bind(Json(&case))
    .bind(ctx.run.id)
    .execute(db)
    .await?;
    Ok(Outcome {
        message: format!("Wrote the business case for {company}: {} {currency} a year", crate::business_cases::rounded(total)),
        output: json!({ "annual": total }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::fake::run_next;
    use crate::business_cases::{self as cases, Edit, Numbers};

    fn line(label: &str, quantity: f64, unit_value: f64, per: &str, count: f64, basis: &str) -> Value {
        json!({ "label": label, "quantity": quantity, "unit": "hours", "unit_value": unit_value, "per": per, "count": count,
                "count_unit": "reps", "basis": basis, "source": "Ines: \"we lose Fridays to the CRM\"" })
    }

    #[test]
    fn a_sum_past_what_an_i64_of_cents_holds_still_reads() {
        assert_eq!(cases::rounded(84_240.4), "84,240");
        assert!(cases::rounded(1e9 * 1e9 * 1e9 * 48.0).starts_with("48,000,000,000,000"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_case_adds_up_its_lines_and_the_buyer_can_put_in_their_own_numbers(db: sqlx::PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let run = cases::request(&db, t.deal, t.project, t.org).await.unwrap();
        assert_eq!(cases::request(&db, t.deal, t.project, t.org).await.unwrap(), run, "one being written is reused");
        let case = cases::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(case.status, "writing");
        assert!(cases::public(&db, &case.token).await.unwrap().is_none(), "nothing before it's written");

        // A line with a period it can't count, one worth nothing and a sixth are dropped.
        let mut lines = vec![
            line("CRM notes", 6.0, 40.0, "week", 5.0, "said"),
            line("Lost renewals", 1.0, 5000.0, "month", 1.0, "estimate"),
            line("Daily", 1.0, 1.0, "day", 1.0, "said"),
            line("Nothing", 0.0, 40.0, "week", 5.0, "said"),
        ];
        lines.extend((0..3).map(|i| line(&format!("Extra {i}"), 1.0, 1.0, "year", 1.0, "guess")));
        let (outcome, prompt) = run_next(
            &db,
            "business_case",
            json!({ "write": true, "why": "They told us the hours.", "case": { "title": " The case for Fieldnote at Lumenfold ", "summary": "s",
                    "problem": "p", "lines": lines, "outcomes": ["Forecasts they trust", " "], "waiting": "w" } }),
        )
        .await;
        assert!(prompt.contains("with Ines Duarte at Lumenfold, worth 4,800 usd"), "{prompt}");
        // 6 h × 40 × 5 reps × 48 weeks = 57,600, and 5,000 × 12 = 60,000, and 3 extras of 1.
        assert_eq!(outcome.output["annual"], 117_603.0);
        let case = cases::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        let content = &case.business_case.as_ref().unwrap().0;
        assert_eq!((case.status.as_str(), content.title.as_str(), content.lines.len()), ("ready", "The case for Fieldnote at Lumenfold", 5));
        assert_eq!(content.lines.iter().map(|l| l.basis.as_str()).collect::<Vec<_>>(), vec!["said", "estimate", "estimate", "estimate", "estimate"]);
        assert_eq!(content.outcomes, vec!["Forecasts they trust"]);
        let investment = case.investment.unwrap();
        assert_eq!((investment.amount, investment.from), (4800.0, "deal"), "the deal's value without a proposal they can read");

        let mut numbers: Vec<Numbers> = content.lines.iter().map(|l| l.numbers()).collect();
        numbers[0].quantity = 3.0;
        let revision = cases::public(&db, &case.token).await.unwrap().unwrap().revision;
        let edit = |numbers: Vec<Numbers>| Edit { name: " Ines ".into(), numbers, revision: revision.clone() };
        assert!(cases::edit(&db, "https://app.example", &case.token, &edit(numbers[..2].to_vec())).await.is_err(), "one per line");
        let mut wild = numbers.clone();
        wild[1].unit_value = f64::INFINITY;
        assert!(cases::edit(&db, "https://app.example", &case.token, &edit(wild)).await.is_err());
        cases::edit(&db, "https://app.example", &case.token, &edit(numbers.clone())).await.unwrap();
        let page = cases::public(&db, &case.token).await.unwrap().unwrap();
        assert_eq!((page.edited_by.as_str(), page.numbers.as_ref().map(|n| n.0[0].quantity)), ("Ines", Some(3.0)));
        assert_eq!(page.business_case.annual(page.numbers.as_ref().map(|n| n.0.as_slice())), 117_603.0 - 28_800.0);

        // Writing it again starts from the agent's numbers.
        cases::request(&db, t.deal, t.project, t.org).await.unwrap();
        run_next(&db, "business_case", json!({ "write": true, "why": "", "case": { "title": "t", "summary": "", "problem": "", "lines": [line("CRM notes", 6.0, 40.0, "week", 5.0, "said")], "outcomes": [], "waiting": "" } })).await;
        let case = cases::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert!(case.numbers.is_none() && case.edited_by.is_empty());
        let stale = cases::edit(&db, "https://app.example", &case.token, &edit(numbers[..1].to_vec())).await;
        assert!(matches!(stale, Err(crate::error::AppError::Conflict(_))), "a page from before can't put its numbers on the new lines");

        cases::request(&db, t.deal, t.project, t.org).await.unwrap();
        run_next(&db, "business_case", json!({ "write": false, "why": "No numbers yet.", "case": { "title": "", "summary": "", "problem": "", "lines": [], "outcomes": [], "waiting": "" } })).await;
        let case = cases::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!((case.status.as_str(), case.skipped.as_str()), ("ready", "No numbers yet."), "the last case stays up when the agent declines");

        sqlx::query("update deals set status = 'lost' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        assert!(cases::public(&db, &case.token).await.unwrap().is_none(), "a lost deal's page is gone");
    }
}
