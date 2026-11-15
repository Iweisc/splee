//! Deal rooms (see deal_rooms.rs): the agent writes the welcome and picks the case studies that fit the buyer best, and
//! drafts an answer to each question they ask, which the team publishes.

use std::time::Duration;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{WriterSettings, materials};
use super::{Ctx, Outcome, briefs};
use crate::deal_rooms::{Content, MAX_PROOF, Pick};
use crate::models::Lead;

const WRITER: &str = "You write the page a B2B seller shares with a buyer during a deal, for Splee, an outbound sales tool, like \
a seller who listens. The buyer reads it. Use only what you're given. Never invent facts, results, numbers, names, prices or \
promises, and leave out anything internal, like notes on the buyer or why a deal stalled. Plain text only, no markdown.";

/// Case studies the agent picks from.
const CANDIDATES: i64 = 20;

#[derive(Deserialize)]
struct RoomInput {
    room_id: Uuid,
}

#[derive(Deserialize)]
struct Choice {
    number: i64,
    why: String,
}

#[derive(Deserialize)]
struct Welcome {
    headline: String,
    intro: String,
    proof: Vec<Choice>,
}

/// The deal's lead, project and what Splee knows, which the room's runs and the business case start from.
pub(super) struct Deal {
    pub(super) lead: Lead,
    pub(super) us: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) context: String,
    pub(super) language: String,
    pub(super) stage: String,
}

pub(super) async fn load(ctx: &Ctx, deal_id: Uuid) -> anyhow::Result<Deal> {
    let db = ctx.db();
    let (lead_id, status, stage): (Uuid, String, String) =
        sqlx::query_as("select d.lead_id, d.status, s.name from deals d join deal_stages s on s.id = d.stage_id where d.id = $1").bind(deal_id).fetch_one(db).await?;
    anyhow::ensure!(status != "lost", "the deal is lost");
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let settings = WriterSettings::load(ctx, lead.project_id).await?;
    let lang = crate::language::for_lead(db, lead.id).await?;
    let context = briefs::gather(db, &lead, org_id).await?;
    let us = if settings.sender_name.is_empty() { profile.name.clone() } else { format!("{} at {}", settings.sender_name, profile.name) };
    Ok(Deal { lead, us, name: profile.name, description: profile.description, context, language: crate::language::instruction(lang), stage })
}

impl Deal {
    pub(super) fn who(&self) -> String {
        [self.lead.full_name.trim(), self.lead.company_name.trim()].iter().filter(|s| !s.is_empty()).cloned().collect::<Vec<_>>().join(" at ")
    }
}

fn welcome_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "headline": { "type": "string" },
            "intro": { "type": "string" },
            "proof": { "type": "array", "items": { "type": "object", "properties": { "number": { "type": "integer" }, "why": { "type": "string" } }, "required": ["number", "why"] } }
        },
        "required": ["headline", "intro", "proof"]
    })
}

/// The picked case studies by their number in the list, each once, at most MAX_PROOF.
fn picked(choices: &[Choice], candidates: &[(Uuid, String, String)]) -> Vec<Pick> {
    let mut out: Vec<Pick> = vec![];
    for c in choices {
        let Some((id, _, _)) = usize::try_from(c.number).ok().and_then(|n| n.checked_sub(1)).and_then(|i| candidates.get(i)) else { continue };
        if out.len() < MAX_PROOF && !out.iter().any(|p| p.material_id == *id) {
            out.push(Pick { material_id: *id, why: c.why.trim().chars().take(200).collect() });
        }
    }
    out
}

pub async fn write(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: RoomInput = ctx.input()?;
    let db = ctx.db();
    let deal_id: Uuid = sqlx::query_scalar("select deal_id from deal_rooms where id = $1 and project_id = $2")
        .bind(input.room_id)
        .bind(ctx.project_id()?)
        .fetch_optional(db)
        .await?
        .context("the room was deleted")?;
    let d = load(ctx, deal_id).await?;
    let candidates: Vec<(Uuid, String, String)> = sqlx::query_as(
        "select id, title, left(content, 600) from materials where project_id = $1 and kind = 'case_study' order by created_at desc limit $2",
    )
    .bind(d.lead.project_id)
    .bind(CANDIDATES)
    .fetch_all(db)
    .await?;
    let list = if candidates.is_empty() {
        "None.".to_string()
    } else {
        candidates.iter().enumerate().map(|(i, (_, title, content))| format!("{}. {title}: {}", i + 1, content.replace('\n', " "))).collect::<Vec<_>>().join("\n")
    };
    let prompt = format!(
        "{us} has a deal in the {stage} stage with {who}.\n\nWhat we sell: {name}. {description}\n\n\
         The conversation and what else Splee knows:\n{context}\n\n\
         Our case studies:\n{list}\n\n\
         Write the deal room page we share with them.\n\
         1. headline: under 12 words, naming their company and what they're trying to get done, from what they said.\n\
         2. intro: 2 or 3 sentences from {us} to them: what we heard they need and what this page is for (the plan, the \
         proposal, stories from teams like theirs, and a place to ask anything). No greeting or sign-off.\n\
         3. proof: up to {max} case studies from the list, by number, that are closest to their situation, each with why: \
         under 20 words to them on what's alike, like \"Also a 40-clinic group moving off spreadsheets\". Empty when none fit. \
         {language}",
        us = d.us,
        stage = d.stage,
        who = d.who(),
        name = d.name,
        description = d.description,
        context = d.context,
        max = MAX_PROOF,
        language = d.language,
    );
    ctx.log(format!("writing {}'s deal room…", d.who())).await?;
    let call = Call { timeout: Duration::from_secs(3 * 60), budget_usd: 0.3, ..Call::writing(WRITER, prompt, welcome_schema()) };
    let w: Welcome = ctx.ask(&call).await?;
    let content = Content {
        headline: w.headline.trim().chars().take(160).collect(),
        intro: w.intro.trim().chars().take(800).collect(),
        proof: picked(&w.proof, &candidates),
    };
    anyhow::ensure!(!content.headline.is_empty() && !content.intro.is_empty(), "the agent wrote an empty page");
    sqlx::query("update deal_rooms set room = $2 where id = $1 and run_id = $3").bind(input.room_id).bind(Json(&content)).bind(ctx.run.id).execute(db).await?;
    Ok(Outcome::new(format!("Wrote the deal room with {} case {}", content.proof.len(), if content.proof.len() == 1 { "study" } else { "studies" })))
}

#[derive(Deserialize)]
struct AnswerInput {
    question_id: Uuid,
}

#[derive(Deserialize)]
struct Drafted {
    answer: String,
}

pub async fn answer(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: AnswerInput = ctx.input()?;
    let db = ctx.db();
    let (deal_id, room_id, name, question): (Uuid, Uuid, String, String) = sqlx::query_as(
        "select r.deal_id, r.id, q.name, q.question from deal_room_questions q join deal_rooms r on r.id = q.room_id where q.id = $1 and r.project_id = $2",
    )
    .bind(input.question_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the question was deleted")?;
    let d = load(ctx, deal_id).await?;
    let materials = materials(ctx, d.lead.project_id).await?;
    let proposal = crate::proposals::for_deal(db, "", deal_id).await?.filter(|p| p.title.is_some() && !p.held);
    let offer = match &proposal {
        Some(p) => p
            .items
            .iter()
            .map(|i| format!("{} x {} at {} {} ({})", i.quantity, i.name, i.unit_price, p.currency.to_uppercase(), i.period.replace('_', " ")))
            .collect::<Vec<_>>()
            .join("; "),
        None => "No proposal they can see yet, so don't quote prices.".to_string(),
    };
    let plan: Vec<String> = sqlx::query_scalar(
        "select s.title || case when s.due_date is not null then ' by ' || s.due_date else '' end || case when s.done_at is not null then ' (done)' else '' end
         from close_plan_steps s join close_plans p on p.id = s.plan_id where p.deal_id = $1 order by s.position",
    )
    .bind(deal_id)
    .fetch_all(db)
    .await?;
    let earlier: Vec<(String, String)> =
        sqlx::query_as("select question, answer from deal_room_questions where room_id = $1 and answer is not null and id <> $2 order by created_at")
            .bind(room_id)
            .bind(input.question_id)
            .fetch_all(db)
            .await?;
    let earlier = earlier.iter().map(|(q, a)| format!("Q: {q}\nA: {a}")).collect::<Vec<_>>().join("\n");
    let prompt = format!(
        "{us} has a deal in the {stage} stage with {who}. {name} asked this on the deal room page:\n\"{question}\"\n\n\
         What we sell: {product}. {description}\nOur materials:\n{materials}\n\n\
         The proposal they can see: {offer}\nThe close plan: {plan}\n\
         Questions answered before:\n{earlier}\n\n\
         The conversation and what else Splee knows:\n{context}\n\n\
         Draft the answer {us} publishes on the page, which the team checks first. 1 to 4 sentences, direct and specific, \
         from the materials, the proposal, the plan and what was said. When none of it answers the question, say plainly \
         that we'll find out and come back to them, and don't guess. No greeting or sign-off. {language}",
        us = d.us,
        stage = d.stage,
        who = d.who(),
        product = d.name,
        description = d.description,
        materials = if materials.is_empty() { "(none)".to_string() } else { materials },
        plan = if plan.is_empty() { "none yet".to_string() } else { plan.join("; ") },
        earlier = if earlier.is_empty() { "None.".to_string() } else { earlier },
        context = d.context,
        language = d.language,
    );
    ctx.log(format!("drafting an answer for {name}…")).await?;
    let schema = json!({ "type": "object", "properties": { "answer": { "type": "string" } }, "required": ["answer"] });
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.2, ..Call::writing(WRITER, prompt, schema) };
    let drafted: Drafted = ctx.ask(&call).await?;
    let draft: String = drafted.answer.trim().chars().take(2000).collect();
    sqlx::query("update deal_room_questions set draft = $2 where id = $1 and run_id = $3").bind(input.question_id).bind(&draft).bind(ctx.run.id).execute(db).await?;
    Ok(Outcome::new(format!("Drafted an answer for {name}")))
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;

    use super::*;
    use crate::deal_rooms;

    async fn run_next(db: &PgPool, kind: &str, answer: Value) -> String {
        super::super::fake::run_next(db, kind, answer).await.1
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_buyer_sees_the_welcome_the_picked_proof_and_only_answers_the_team_published(db: PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        let mut studies = vec![];
        for (title, content) in [("Brightline", "Cut CRM admin by 5 hours a week per rep."), ("Kestrel", "Halved report time."), ("Old", "Gone soon.")] {
            let id: Uuid = sqlx::query_scalar("insert into materials (project_id, kind, title, content) values ($1, 'case_study', $2, $3) returning id")
                .bind(t.project)
                .bind(title)
                .bind(content)
                .fetch_one(&db)
                .await
                .unwrap();
            studies.push(id);
        }
        sqlx::query("insert into materials (project_id, kind, title, content) values ($1, 'faq', 'Security', 'SOC 2 Type II since 2025.')").bind(t.project).execute(&db).await.unwrap();

        let run = deal_rooms::request(&db, t.deal, t.project, t.org).await.unwrap();
        assert_eq!(deal_rooms::request(&db, t.deal, t.project, t.org).await.unwrap(), run, "one being written is reused");
        let room = deal_rooms::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(room.status, "writing");
        assert!(deal_rooms::public(&db, "https://app.example", &room.token).await.unwrap().is_none(), "nothing before the welcome");

        // The list is newest first: Old, Kestrel, Brightline. A number past the list and a repeat are dropped.
        let prompt = run_next(
            &db,
            "deal_room",
            json!({ "headline": "Lumenfold's reps back to selling", "intro": "You told us the CRM eats Fridays.",
                    "proof": [{ "number": 3, "why": " Same CRM pain " }, { "number": 9, "why": "x" }, { "number": 3, "why": "again" }, { "number": 1, "why": "y" }] }),
        )
        .await;
        assert!(prompt.contains("with Ines Duarte at Lumenfold") && prompt.contains("3. Brightline: Cut CRM admin"), "{prompt}");
        sqlx::query("delete from materials where id = $1").bind(studies[2]).execute(&db).await.unwrap();
        let room = deal_rooms::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        assert_eq!(room.status, "ready");
        assert_eq!(room.proof.iter().map(|p| (p.title.as_str(), p.why.as_str())).collect::<Vec<_>>(), vec![("Brightline", "Same CRM pain")], "a deleted study drops out");

        let form = deal_rooms::Ask { name: "Ines".into(), question: " Are you SOC 2 certified? ".into() };
        deal_rooms::ask(&db, "https://app.example", &room.token, &form).await.unwrap();
        assert!(deal_rooms::ask(&db, "https://app.example", "nope", &form).await.is_err());
        let prompt = run_next(&db, "room_answer", json!({ "answer": " Yes, SOC 2 Type II since 2025. " })).await;
        assert!(prompt.contains("\"Are you SOC 2 certified?\"") && prompt.contains("SOC 2 Type II since 2025.") && prompt.contains("don't quote prices"), "{prompt}");
        let room = deal_rooms::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap();
        let q = &room.questions[0];
        assert_eq!((q.draft.as_str(), q.answer.as_deref(), q.drafting), ("Yes, SOC 2 Type II since 2025.", None, false));
        let page = deal_rooms::public(&db, "https://app.example", &room.token).await.unwrap().unwrap();
        assert!(page.questions.is_empty() && page.plan.is_none() && page.proposal.is_none(), "no draft, plan or proposal shown yet");
        assert_eq!((page.room.headline.as_str(), page.proof.len()), ("Lumenfold's reps back to selling", 1));

        assert_eq!(deal_rooms::deal_of_question(&db, t.org, q.id).await.unwrap(), t.deal);
        deal_rooms::answer(&db, q.id, t.dana, " Yes, Type II. ").await.unwrap();
        let page = deal_rooms::public(&db, "https://app.example", &room.token).await.unwrap().unwrap();
        assert_eq!(page.questions.iter().map(|q| q.answer.as_str()).collect::<Vec<_>>(), vec!["Yes, Type II."]);
        deal_rooms::record_view(&db, "https://app.example", &room.token).await.unwrap();
        deal_rooms::record_view(&db, "https://app.example", &room.token).await.unwrap();

        sqlx::query("update deals set status = 'lost' where id = $1").bind(t.deal).execute(&db).await.unwrap();
        assert!(deal_rooms::public(&db, "https://app.example", &room.token).await.unwrap().is_none(), "gone once the deal is lost");
        deal_rooms::record_view(&db, "https://app.example", &room.token).await.unwrap();
        assert_eq!(deal_rooms::for_deal(&db, "https://app.example", t.deal).await.unwrap().unwrap().views, 2, "a lost deal's page counts no views");
    }
}
