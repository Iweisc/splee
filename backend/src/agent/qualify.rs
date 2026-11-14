//! Deal qualification: the agent fills in a MEDDICC scorecard for an open deal from what the buyer said and did, with the
//! evidence for each element and the question that would fill each gap. A claim without evidence counts as unknown. Open
//! deals get a new scorecard after something happens in them, in projects that keep it on, and meeting briefs ask the
//! questions for the gaps.

use std::time::Duration;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::write::{company_signals, lead_brief};
use super::{Ctx, Outcome, briefs, committee, health, kind};
use crate::deals::LAST_TOUCH;
use crate::models::Lead;
use crate::state::AppState;

/// The MEDDICC elements, in order, with what each asks.
pub const ELEMENTS: [(&str, &str); 8] = [
    ("metrics", "Metrics: the numbers they want to move, like hours saved or revenue, and by how much"),
    ("economic_buyer", "Economic buyer: who controls the budget and signs, and whether we've talked to them"),
    ("decision_criteria", "Decision criteria: what they'll judge the options on"),
    ("decision_process", "Decision process: the steps, the people and the dates to a decision"),
    ("paper_process", "Paper process: legal, security review, procurement and how long they take"),
    ("pain", "Identified pain: the problem behind the deal and what it costs them"),
    ("champion", "Champion: someone inside who sells it for us when we're not there"),
    ("competition", "Competition: who else they're looking at, including doing nothing or building it"),
];
const STATUSES: [&str; 3] = ["confirmed", "partial", "unknown"];
const PER_PASS: i64 = 50;
const QUALIFIER: &str = "You qualify B2B sales deals with MEDDICC for Splee, an outbound sales tool, like a strict sales manager. \
Count only what the buyer said or did in the evidence given. Our own claims, guesses and what's typical for companies like \
theirs don't count. Never invent people, numbers or dates.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Item {
    pub key: String,
    /// confirmed, partial or unknown.
    pub status: String,
    /// What the buyer said or did that shows it, with when. Empty when unknown.
    pub evidence: String,
    /// The question to ask next. Empty when confirmed.
    pub ask: String,
}

/// Queues a scorecard for every open deal that had something happen since its last one, or never had one but has heard
/// from the buyer. Returns how many.
pub async fn queue_due(db: &PgPool) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let due: Vec<(Uuid, Uuid, Uuid)> = sqlx::query_as(&format!(
        "with due as (
           insert into deal_qualification (project_id, deal_id)
           select d.project_id, d.id from deals d
           join projects p on p.id = d.project_id and p.org_id is not null
           join project_settings s on s.project_id = d.project_id and s.qualify_deals
           left join lateral (select q.created_at, r.status from deal_qualification q left join agent_runs r on r.id = q.run_id
                              where q.deal_id = d.id order by q.created_at desc limit 1) q on true
           where d.status = 'open' and coalesce(q.status, '') not in ('queued', 'running')
             and (case when q.created_at is null
                       then exists (select 1 from email_messages e where e.lead_id = d.lead_id and e.direction = 'inbound')
                         or exists (select 1 from meetings m where m.lead_id = d.lead_id and m.summary <> '')
                       else {LAST_TOUCH} > q.created_at end)
           order by q.created_at nulls first limit $1
           returning id, project_id)
         select due.id, due.project_id, p.org_id from due join projects p on p.id = due.project_id"
    ))
    .bind(PER_PASS)
    .fetch_all(&mut *tx)
    .await?;
    for (id, project_id, org_id) in &due {
        start(&mut tx, *id, *project_id, *org_id).await?;
    }
    tx.commit().await?;
    Ok(due.len())
}

/// Fills in a new scorecard for the deal now. Returns the run.
pub async fn request(db: &PgPool, deal_id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let mut tx = db.begin().await?;
    let id: Uuid =
        sqlx::query_scalar("insert into deal_qualification (project_id, deal_id) values ($1, $2) returning id").bind(project_id).bind(deal_id).fetch_one(&mut *tx).await?;
    let run = start(&mut tx, id, project_id, org_id).await?;
    tx.commit().await?;
    Ok(run)
}

async fn start(tx: &mut sqlx::PgConnection, id: Uuid, project_id: Uuid, org_id: Uuid) -> sqlx::Result<Uuid> {
    let run = super::enqueue(&mut *tx, kind::QUALIFY_DEAL, Some(project_id), Some(org_id), json!({ "qualification_id": id })).await?;
    sqlx::query("update deal_qualification set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    Ok(run)
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        crate::mail::every(Duration::from_secs(3600), || async { queue_due(&state.db).await.map(|_| ()).map_err(Into::into) }, "deal qualification").await
    });
}

/// The open deal's gaps as questions for a meeting brief, or nothing when it has no scorecard.
pub async fn gaps(db: &PgPool, lead_id: Uuid) -> sqlx::Result<String> {
    let items: Option<Value> = sqlx::query_scalar(
        "select q.items from deal_qualification q join deals d on d.id = q.deal_id
         where d.lead_id = $1 and d.status = 'open' and q.items is not null order by q.created_at desc limit 1",
    )
    .bind(lead_id)
    .fetch_optional(db)
    .await?
    .flatten();
    let items: Vec<Item> = items.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let open: Vec<String> = items
        .iter()
        .filter(|i| i.status != "confirmed" && !i.ask.is_empty())
        .filter_map(|i| ELEMENTS.iter().find(|(k, _)| *k == i.key).map(|(_, what)| format!("- {} ({}): {}", what.split(':').next().unwrap_or(&i.key), i.status, i.ask)))
        .collect();
    Ok(if open.is_empty() { String::new() } else { format!("What we still don't know about the deal, with a question for each:\n{}", open.join("\n")) })
}

#[derive(Deserialize)]
struct Input {
    qualification_id: Uuid,
}

#[derive(Deserialize)]
struct Answer {
    items: Vec<AnswerItem>,
}

#[derive(Deserialize)]
struct AnswerItem {
    key: String,
    status: String,
    evidence: String,
    ask: String,
}

fn schema() -> Value {
    let keys: Vec<&str> = ELEMENTS.iter().map(|(k, _)| *k).collect();
    json!({
        "type": "object",
        "properties": { "items": { "type": "array", "items": { "type": "object", "properties": {
            "key": { "type": "string", "enum": keys }, "status": { "type": "string", "enum": STATUSES },
            "evidence": { "type": "string" }, "ask": { "type": "string" }
        }, "required": ["key", "status", "evidence", "ask"] } } },
        "required": ["items"]
    })
}

/// Who's in the buying committee the agent mapped for the lead's company, in a line, or nothing.
async fn committee_line(db: &PgPool, lead: &Lead) -> anyhow::Result<String> {
    let Some(company) = committee::company_of(db, lead).await? else { return Ok(String::new()) };
    let Some(committee) = committee::latest(db, company).await?.and_then(|s| s.committee) else { return Ok(String::new()) };
    let committee: committee::Committee = serde_json::from_value(committee)?;
    let ids: Vec<Uuid> = committee.members.iter().map(|m| m.lead_id).collect();
    let people: Vec<(Uuid, String, String)> = sqlx::query_as("select id, full_name, title from leads where id = any($1)").bind(&ids).fetch_all(db).await?;
    let list: Vec<String> = committee
        .members
        .iter()
        .filter_map(|m| people.iter().find(|(id, ..)| *id == m.lead_id).map(|(_, name, title)| format!("{name} ({title}): {}", m.role.replace('_', " "))))
        .collect();
    Ok(if list.is_empty() { String::new() } else { format!("\nThe buying committee we mapped at their company: {}.", list.join("; ")) })
}

/// What the last scorecard found the buyer said or did, so a fact confirmed from an email that's since dropped out of the
/// latest few isn't lost.
async fn earlier_evidence(db: &PgPool, deal_id: Uuid) -> sqlx::Result<String> {
    let items: Option<Value> = sqlx::query_scalar("select items from deal_qualification where deal_id = $1 and items is not null order by created_at desc limit 1")
        .bind(deal_id)
        .fetch_optional(db)
        .await?;
    let items: Vec<Item> = items.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    let lines: Vec<String> = items.iter().filter(|i| i.status != "unknown").map(|i| format!("- {} ({}): {}", i.key, i.status, i.evidence)).collect();
    Ok(if lines.is_empty() {
        String::new()
    } else {
        format!("\n\nWhat the buyer said or did that our last scorecard found, still evidence unless newer conversation contradicts it:\n{}", lines.join("\n"))
    })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (deal_id, lead_id, status, stage, notes): (Uuid, Uuid, String, String, String) = sqlx::query_as(
        "select d.id, d.lead_id, d.status, s.name, d.notes from deal_qualification q join deals d on d.id = q.deal_id
         join deal_stages s on s.id = d.stage_id where q.id = $1 and q.project_id = $2",
    )
    .bind(input.qualification_id)
    .bind(ctx.project_id()?)
    .fetch_optional(db)
    .await?
    .context("the scorecard was deleted")?;
    anyhow::ensure!(status == "open", "the deal is closed");
    let lead = sqlx::query_as::<_, Lead>("select * from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let (project, profile) = load_project(ctx, lead.project_id).await?;
    let org_id = project.org_id.context("the project has no organization")?;
    let signals = company_signals(ctx, &[&lead]).await?;
    let about = lead_brief(&lead, &signals);
    let context = briefs::gather(db, &lead, org_id).await?;
    let shared = health::shared(db, deal_id).await?;
    let committee = committee_line(db, &lead).await?;
    let earlier = earlier_evidence(db, deal_id).await?;

    let notes = if notes.trim().is_empty() { String::new() } else { format!("\nThe deal's notes: {}", notes.trim()) };
    let shared = if shared.is_empty() { String::new() } else { format!("\n{shared}") };
    let elements = ELEMENTS.iter().map(|(k, what)| format!("- {k}. {what}")).collect::<Vec<_>>().join("\n");
    let prompt = format!(
        "{us} has an open deal with this lead in the {stage} stage.{notes}{shared}{committee}\n\n\
         What we sell: {name}. {description}\n\n\
         The lead and their company:\n{about}\n\nWhat else Splee knows:\n{context}{earlier}\n\n\
         Fill in a MEDDICC scorecard for the deal, one item for each of these:\n{elements}\n\n\
         For each item:\n\
         - status: confirmed when the buyer said or showed it clearly, partial when there's a hint but not enough, unknown when \
         nothing they said or did shows it.\n\
         - evidence: under 25 words, what they said or did that shows it, naming who and when. Empty when unknown.\n\
         - ask: one short question to ask them next that would confirm it or fill the gap, in plain words. Empty when confirmed.",
        us = profile.name,
        name = profile.name,
        description = profile.description,
        about = serde_json::to_string_pretty(&about)?,
    );
    let first = if lead.first_name.is_empty() { &lead.full_name } else { &lead.first_name };
    ctx.log(format!("qualifying the deal with {first}…")).await?;
    let call = Call { timeout: Duration::from_secs(2 * 60), budget_usd: 0.25, ..Call::writing(QUALIFIER, prompt, schema()) };
    let items = tidy(ctx.ask::<Answer>(&call).await?);
    let confirmed = items.iter().filter(|i| i.status == "confirmed").count();

    sqlx::query("update deal_qualification set items = $2 where id = $1 and run_id = $3")
        .bind(input.qualification_id)
        .bind(json!(items))
        .bind(ctx.run.id)
        .execute(db)
        .await?;
    Ok(Outcome::new(format!("Qualified the deal with {first}: {confirmed} of {} confirmed", ELEMENTS.len())))
}

/// One item per element in ELEMENTS' order, the first answer for each winning. A missing element is unknown, a claim
/// without evidence is unknown, and a confirmed one asks nothing.
fn tidy(a: Answer) -> Vec<Item> {
    let clip = |s: &str, n: usize| s.trim().chars().take(n).collect::<String>();
    ELEMENTS
        .iter()
        .map(|(key, _)| {
            let got = a.items.iter().find(|i| i.key == *key && STATUSES.contains(&i.status.as_str()));
            let evidence = got.map(|i| clip(&i.evidence, 240)).unwrap_or_default();
            let status = match got {
                Some(i) if i.status != "unknown" && !evidence.is_empty() => i.status.clone(),
                _ => "unknown".to_string(),
            };
            let ask = if status == "confirmed" { String::new() } else { got.map(|i| clip(&i.ask, 200)).unwrap_or_default() };
            Item { key: key.to_string(), evidence: if status == "unknown" { String::new() } else { evidence }, status, ask }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_deal_is_qualified_from_evidence_after_it_hears_from_the_buyer_and_briefs_ask_the_gaps(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\"}') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(&db).await.unwrap();
        let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, full_name, first_name, email, company_name) values ($1, 'Noor Haddad', 'Noor', 'n@q.example', 'Quill Labs') returning id")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        crate::deals::ensure_stages(&db, org).await.unwrap();
        let stage: Uuid = sqlx::query_scalar("select id from deal_stages where org_id = $1 and name = 'Proposal'").bind(org).fetch_one(&db).await.unwrap();
        let deal: Uuid = sqlx::query_scalar("insert into deals (org_id, project_id, lead_id, stage_id, stage_changed_at) values ($1, $2, $3, $4, now() - interval '5 days') returning id")
            .bind(org)
            .bind(project)
            .bind(lead)
            .bind(stage)
            .fetch_one(&db)
            .await
            .unwrap();

        assert_eq!(queue_due(&db).await.unwrap(), 0, "nothing from the buyer yet");
        sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, body, status, created_at) values ($1, $2, 'inbound', 'inbound', 'Our CFO signs anything over $5k.', 'received', now() - interval '4 days')")
            .bind(project)
            .bind(lead)
            .execute(&db)
            .await
            .unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1);
        let pending = crate::deals::get(&db, org, deal).await.unwrap();
        assert!(pending.qualification.is_none() && pending.qualification_writing, "the first one shows as being written");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "not while one is being written");

        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'qualify_deal' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({ "items": [
                { "key": "economic_buyer", "status": "partial", "evidence": "Noor said their CFO signs over $5k.", "ask": "Can we meet your CFO?" },
                { "key": "metrics", "status": "confirmed", "evidence": " ", "ask": "How many hours?" },
                { "key": "champion", "status": "confirmed", "evidence": "Noor pushed the pilot.", "ask": "Should be dropped" },
                { "key": "pain", "status": "made_up", "evidence": "x", "ask": "y" }
            ] })
        });
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("Our CFO signs anything over $5k.") && prompt.contains("- paper_process. Paper process"), "{prompt}");
        sqlx::query("update agent_runs set status = 'succeeded' where id = $1").bind(ctx.run.id).execute(&db).await.unwrap();

        let shown = crate::deals::get(&db, org, deal).await.unwrap().qualification.unwrap();
        let items: Vec<Item> = serde_json::from_value(shown["items"].clone()).unwrap();
        assert_eq!(items.len(), 8, "every element, in order");
        assert_eq!((items[0].key.as_str(), items[0].status.as_str(), items[0].ask.as_str()), ("metrics", "unknown", "How many hours?"), "no evidence, no claim");
        assert_eq!((items[1].status.as_str(), items[1].ask.as_str()), ("partial", "Can we meet your CFO?"));
        assert_eq!((items[6].status.as_str(), items[6].ask.as_str()), ("confirmed", ""), "a confirmed element asks nothing");
        assert_eq!(items[5].status, "unknown", "an unknown status is unknown");
        assert_eq!(shown["confirmed"], 1);

        let gaps = gaps(&db, lead).await.unwrap();
        assert!(gaps.contains("- Economic buyer (partial): Can we meet your CFO?") && !gaps.contains("Champion"), "{gaps}");
        assert_eq!(queue_due(&db).await.unwrap(), 0, "nothing new happened");
        sqlx::query("update deals set stage_changed_at = now() where id = $1").bind(deal).execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 1, "it moved, so it's qualified again");
        let again = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'qualify_deal' and status = 'queued' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: again, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| json!({ "items": [] }));
        run(&ctx).await.unwrap();
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains("- champion (confirmed): Noor pushed the pilot.") && !prompt.contains("- metrics ("), "the last scorecard's evidence: {prompt}");
        sqlx::query("update agent_runs set status = 'succeeded'").execute(&db).await.unwrap();
        sqlx::query("update deals set stage_changed_at = now() + interval '1 minute' where id = $1").bind(deal).execute(&db).await.unwrap();
        sqlx::query("update project_settings set qualify_deals = false").execute(&db).await.unwrap();
        assert_eq!(queue_due(&db).await.unwrap(), 0, "switched off");
    }
}
