//! Writes a deal's proposal (crate::proposals) from the conversation: a title, an intro to the buyer, three short sections
//! and the prices as quoted. Writing it again rewrites the text and keeps prices that are already set.

use std::time::Duration;

use anyhow::Context;
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, company_signals, lead_brief, materials};
use super::{Ctx, Outcome, briefs, kind};
use crate::models::Lead;
use crate::proposals::{Item, Section, tidy_items};

const WRITER: &str = "You write short, plain B2B sales proposals, for Splee, an outbound sales tool. The buyer reads it. Use only \
what you're given. Never invent facts, numbers, prices, customers, names or quotes. Plain text only, no markdown.";
/// How long a new proposal can be accepted.
const VALID_DAYS: i64 = 30;

/// Starts writing the deal's proposal, making it first if it has none. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> crate::error::AppResult<Uuid> {
    let mut tx = db.begin().await?;
    let id = crate::proposals::ensure_open(&mut tx, project_id, deal_id).await?;
    let run = super::enqueue(&mut *tx, kind::PROPOSAL, Some(project_id), Some(org_id), json!({ "proposal_id": id })).await?;
    sqlx::query("update proposals set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(run)
}

#[derive(Deserialize)]
struct Input {
    proposal_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct DealNow {
    lead_id: Uuid,
    status: String,
    stage: String,
    value_cents: i64,
    currency: String,
    close_date: Option<chrono::NaiveDate>,
    notes: String,
}

#[derive(Deserialize)]
struct Draft {
    title: String,
    intro: String,
    sections: Vec<Section>,
    items: Vec<Item>,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "title": { "type": "string" },
            "intro": { "type": "string" },
            "sections": { "type": "array", "items": { "type": "object", "properties": { "title": { "type": "string" }, "body": { "type": "string" } }, "required": ["title", "body"] } },
            "items": { "type": "array", "items": { "type": "object", "properties": {
                "name": { "type": "string" }, "detail": { "type": "string" }, "quantity": { "type": "integer" },
                "unit_price": { "type": "number" }, "period": { "type": "string", "enum": ["one_time", "month", "year"] }
            }, "required": ["name", "detail", "quantity", "unit_price", "period"] } }
        },
        "required": ["title", "intro", "sections", "items"]
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (deal_id, kept, accepted): (Uuid, Json<Vec<Item>>, bool) =
        sqlx::query_as("select deal_id, items, accepted_at is not null from proposals where id = $1 and project_id = $2")
            .bind(input.proposal_id)
            .bind(ctx.project_id()?)
            .fetch_optional(db)
            .await?
            .context("the proposal was deleted")?;
    anyhow::ensure!(!accepted, "the proposal was accepted");
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

    let value = if deal.value_cents > 0 { format!(", worth {} {}", deal.value_cents / 100, deal.currency.to_uppercase()) } else { String::new() };
    let close = deal.close_date.map(|d| format!(" They expect to close by {d}.")).unwrap_or_default();
    let notes = if deal.notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", deal.notes.trim()) };
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    let prices = if kept.is_empty() {
        format!(
            "4. items: the prices exactly as quoted in the conversation, the deal's notes or our materials, in {currency}, each \
             with a name, a one-line detail, a quantity, a unit price as a number and a period (one_time, month or year), so \
             they add up to what was quoted. When no price was quoted, return no items.",
            currency = deal.currency.to_uppercase()
        )
    } else {
        "4. items: return none, since the team set the prices already.".to_string()
    };
    let prompt = format!(
        "{us} has an open deal with this lead in the {stage} stage{value}.{close}{notes}\n\n\
         What we sell: {name}. {description}\nOur materials:\n{materials}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}\n\n\
         Write the proposal {us} sends them.\n\
         1. title: under 10 words, naming their company, like \"Fieldnote for Acme's sales team\".\n\
         2. intro: 2 or 3 sentences to the lead by first name: what they told us they need and what this proposal covers.\n\
         3. sections: 3 sections, each a title of under 5 words and a body of 40 to 80 words. First what they get, tied to \
         what they asked about. Then how getting started works, fitted to their timeline. Then why us, with proof from our \
         materials. Leave a section's proof out rather than invent it.\n\
         {prices}\n\
         Leave out anything internal, like our notes on them. {language}",
        stage = deal.stage,
        name = profile.name,
        description = profile.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        about = serde_json::to_string_pretty(&about)?,
        language = crate::language::instruction(lang),
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("reading the deal with {first} and writing a proposal…")).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.4, ..Call::writing(WRITER, prompt, schema()) };
    let draft: Draft = ctx.ask(&call).await?;
    anyhow::ensure!(!draft.title.trim().is_empty(), "the agent wrote a proposal without a title");
    let sections: Vec<Section> = draft
        .sections
        .into_iter()
        .map(|s| Section { title: s.title.trim().chars().take(80).collect(), body: s.body.trim().chars().take(1200).collect() })
        .filter(|s| !s.title.is_empty() && !s.body.is_empty())
        .take(4)
        .collect();
    // Items the model got wrong are dropped rather than failing the run: the team sets prices in the deal's window anyway.
    let items = if kept.is_empty() { tidy_items(draft.items.into_iter().take(crate::proposals::MAX_ITEMS).collect()).unwrap_or_default() } else { kept.0 };

    sqlx::query(
        "update proposals set title = $2, intro = $3, sections = $4, items = $5, valid_until = coalesce(valid_until, $6), updated_at = now()
         where id = $1 and accepted_at is null",
    )
    .bind(input.proposal_id)
    .bind(draft.title.trim().chars().take(160).collect::<String>())
    .bind(draft.intro.trim().chars().take(1500).collect::<String>())
    .bind(Json(&sections))
    .bind(Json(&items))
    .bind(Utc::now().date_naive() + chrono::Duration::days(VALID_DAYS))
    .execute(db)
    .await?;
    Ok(Outcome::new(format!("Wrote a proposal with {} sections and {} prices", sections.len(), items.len())))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;
    use crate::proposals::{self, Acceptance, first_year};

    #[test]
    fn the_first_year_counts_monthly_items_twelve_times() {
        let item = |q: i64, p: f64, period: &str| Item { name: "x".into(), detail: String::new(), quantity: q, unit_price: p, period: period.into() };
        assert_eq!(first_year(&[item(8, 187.5, "month"), item(1, 500.0, "one_time"), item(2, 100.0, "year")]), 18_000.0 + 500.0 + 200.0);
        assert!(tidy_items(vec![item(0, 10.0, "month")]).is_err());
        assert!(tidy_items(vec![item(1, 10.0, "week")]).is_err());
        assert!(tidy_items(vec![item(1, -1.0, "month")]).is_err());
        assert_eq!(tidy_items(vec![item(1, 10.004, "month")]).unwrap()[0].unit_price, 10.0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_proposal_is_written_from_the_deal_and_accepting_it_wins_the_deal_once(db: PgPool) {
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
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and kind = 'open' order by position limit 1").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id, value_cents, notes) values ($1, $2, $3, $4, 1000000, '8 seats at $1,500 a month') returning id",
        )
        .bind(org)
        .bind(project)
        .bind(lead)
        .bind(stage)
        .fetch_one(&db)
        .await
        .unwrap();
        let state = crate::autopilot::tests::state(db.clone());

        let run_id = request(&db, deal, project, org).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where id = $1 returning *").bind(run_id).fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: state.clone(), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({
                "title": " Fieldnote for Quanta Forms ", "intro": "Hi Lukas,",
                "sections": [{ "title": "What you get", "body": "Notes in HubSpot." }, { "title": " ", "body": "dropped" }],
                "items": [{ "name": "Seats", "detail": "", "quantity": 8, "unit_price": 187.5, "period": "month" }]
            })
        });
        run(&ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(run_id).execute(&db).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("8 seats at $1,500 a month") && prompt.contains("the prices exactly as quoted"), "{prompt}");

        let p = proposals::for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        assert_eq!((p.status.as_str(), p.title.as_deref(), p.sections.len(), p.items.len()), ("ready", Some("Fieldnote for Quanta Forms"), 1, 1));
        assert!(p.valid_until.is_some());
        let page = proposals::public(&db, &p.token).await.unwrap().unwrap();
        assert_eq!((page.from_name.as_str(), page.first_name.as_str(), page.closed), ("Fieldnote", "Lukas", false));

        let accept = |name: &str| Acceptance { name: name.into(), title: "CRO".into() };
        assert!(proposals::accept(&state, &p.token, accept(" ")).await.is_err(), "a name is needed");
        proposals::accept(&state, &p.token, accept("Lukas Weber")).await.unwrap();
        let (status, value): (String, i64) = sqlx::query_as("select status, value_cents from deals where id = $1").bind(deal).fetch_one(&db).await.unwrap();
        assert_eq!((status.as_str(), value), ("won", 1_800_000), "won at the first year's total");
        assert!(proposals::accept(&state, &p.token, accept("Lukas Weber")).await.is_err(), "once");
        assert!(request(&db, deal, project, org).await.is_err(), "an accepted proposal doesn't change");
        let p = proposals::for_deal(&db, "https://app.example", deal).await.unwrap().unwrap();
        assert_eq!((p.accepted_name.as_str(), p.accepted_title.as_str()), ("Lukas Weber", "CRO"));
    }
}
