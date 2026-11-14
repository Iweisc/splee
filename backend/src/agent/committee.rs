//! Buying committees: who at a lead's company takes part in buying, and who's missing. The agent sorts the company's leads by
//! their part in the decision, names up to 3 roles the deal lacks, then looks for people in them like Find people does and adds
//! them to the lead's campaign.

use std::collections::HashMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::{load_project, load_segment};
use super::sdk::Call;
use super::{Ctx, Outcome, kind, prospect};
use crate::models::{Company, Lead, Ts};

pub const ROLES: [&str; 5] = ["decision_maker", "champion", "influencer", "user", "unknown"];
/// Roles it looks for at most, and so the most people one search adds.
pub const MAX_MISSING: usize = 3;
const MAPPER: &str = "You help a salesperson sell to a whole company rather than one person, for Splee, an outbound sales tool. \
You know how companies of each size buy software and services. Use only the people given, and never invent anyone. Plain text only.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Member {
    pub lead_id: Uuid,
    pub role: String,
    /// What they'd care about, in a few words.
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Missing {
    /// Job titles to look for, like "CFO or VP Finance".
    pub role: String,
    pub why: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Committee {
    pub summary: String,
    pub members: Vec<Member>,
    pub missing: Vec<Missing>,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Saved {
    pub id: Uuid,
    pub committee: Option<Value>,
    pub found: i32,
    pub created_at: Ts,
    /// `ready`, `working` while the run maps it or looks for people, or `failed`.
    pub status: String,
}

/// The lead's company, by its link or its domain. None for a lead without one.
pub async fn company_of(db: &PgPool, lead: &Lead) -> sqlx::Result<Option<Uuid>> {
    if lead.company_id.is_some() {
        return Ok(lead.company_id);
    }
    let Some(domain) = lead.company_domain.as_deref().filter(|d| !d.is_empty()) else { return Ok(None) };
    sqlx::query_scalar("select id from companies where project_id = $1 and domain = $2 order by created_at limit 1")
        .bind(lead.project_id)
        .bind(domain)
        .fetch_optional(db)
        .await
}

pub async fn latest(db: &PgPool, company_id: Uuid) -> sqlx::Result<Option<Saved>> {
    sqlx::query_as(
        "select b.id, b.committee, b.found, b.created_at,
                case when r.status in ('queued', 'running') then 'working' when r.status = 'succeeded' then 'ready'
                     when b.committee is not null then 'ready' else 'failed' end as status
         from buying_committees b left join agent_runs r on r.id = b.run_id
         where b.company_id = $1 order by b.created_at desc limit 1",
    )
    .bind(company_id)
    .fetch_optional(db)
    .await
}

/// Maps the lead's company and looks for who's missing. Imported leads may have only a domain, so their company is added
/// then, in the lead's campaign.
pub async fn request(db: &PgPool, lead: &Lead, org_id: Uuid) -> anyhow::Result<Uuid> {
    let segment_id = lead.segment_id.context("the lead isn't in a campaign")?;
    let mut tx = db.begin().await?;
    let company_id = match company_of(db, lead).await? {
        Some(id) => id,
        None => {
            let domain = lead.company_domain.as_deref().filter(|d| !d.is_empty()).context("the lead has no company")?;
            sqlx::query_scalar("insert into companies (project_id, segment_id, domain, name, status) values ($1, $2, $3, $4, 'people_found') returning id")
                .bind(lead.project_id)
                .bind(segment_id)
                .bind(domain)
                .bind(&lead.company_name)
                .fetch_one(&mut *tx)
                .await?
        }
    };
    let id: Uuid = sqlx::query_scalar("insert into buying_committees (project_id, company_id, lead_id) values ($1, $2, $3) returning id")
        .bind(lead.project_id)
        .bind(company_id)
        .bind(lead.id)
        .fetch_one(&mut *tx)
        .await?;
    let run = super::enqueue(&mut *tx, kind::BUYING_COMMITTEE, Some(lead.project_id), Some(org_id), json!({ "committee_id": id })).await?;
    sqlx::query("update buying_committees set run_id = $2 where id = $1").bind(id).bind(run).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(id)
}

#[derive(Deserialize)]
struct Input {
    committee_id: Uuid,
}

#[derive(sqlx::FromRow)]
struct Person {
    id: Uuid,
    full_name: String,
    title: String,
    status: String,
}

#[derive(Deserialize)]
struct Answer {
    summary: String,
    members: Vec<AnswerMember>,
    missing: Vec<Missing>,
}

#[derive(Deserialize)]
struct AnswerMember {
    person: String,
    role: String,
    note: String,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "summary": { "type": "string" },
            "members": { "type": "array", "items": { "type": "object", "properties": {
                "person": { "type": "string", "description": "The person's id, like p1." },
                "role": { "type": "string", "enum": ROLES },
                "note": { "type": "string" }
            }, "required": ["person", "role", "note"] } },
            "missing": { "type": "array", "items": { "type": "object", "properties": {
                "role": { "type": "string" },
                "why": { "type": "string" }
            }, "required": ["role", "why"] } }
        },
        "required": ["summary", "members", "missing"]
    })
}

/// Keeps members the model could point to and roles worth a search.
fn tidy(answer: Answer, people: &[Person]) -> Committee {
    let ids: HashMap<String, Uuid> = people.iter().enumerate().map(|(i, p)| (format!("p{}", i + 1), p.id)).collect();
    let mut members: Vec<Member> = vec![];
    for m in answer.members {
        let Some(&lead_id) = ids.get(&m.person.trim().to_lowercase()) else { continue };
        if members.iter().any(|known| known.lead_id == lead_id) {
            continue;
        }
        let role = if ROLES.contains(&m.role.as_str()) { m.role } else { "unknown".into() };
        members.push(Member { lead_id, role, note: m.note.trim().to_string() });
    }
    let missing = answer
        .missing
        .into_iter()
        .map(|m| Missing { role: m.role.trim().to_string(), why: m.why.trim().to_string() })
        .filter(|m| !m.role.is_empty())
        .take(MAX_MISSING)
        .collect();
    Committee { summary: answer.summary.trim().to_string(), members, missing }
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let (company_id, lead_id): (Uuid, Option<Uuid>) =
        sqlx::query_as("select company_id, lead_id from buying_committees where id = $1 and project_id = $2")
            .bind(input.committee_id)
            .bind(ctx.project_id()?)
            .fetch_optional(db)
            .await?
            .context("the buying committee was deleted")?;
    let company: Company = sqlx::query_as("select * from companies where id = $1").bind(company_id).fetch_one(db).await?;
    let lead_segment: Option<Uuid> = match lead_id {
        Some(id) => sqlx::query_scalar("select segment_id from leads where id = $1").bind(id).fetch_optional(db).await?.flatten(),
        None => None,
    };
    let segment = load_segment(ctx, lead_segment.or(company.segment_id).context("the company isn't in a campaign")?).await?;
    let (project, profile) = load_project(ctx, company.project_id).await?;
    let brief = profile.brief(project.domain.as_deref());

    ctx.log(format!("mapping who buys at {}…", company.name)).await?;
    let people = load_people(db, &company).await?;
    let committee = tidy(ctx.ask(&map(&brief, &company, &people)).await?, &people);
    save(db, input.committee_id, &committee, 0).await?;
    if committee.missing.is_empty() {
        return Ok(Outcome { message: "The buying committee is complete".into(), output: json!({ "found": 0 }) });
    }

    let roles = committee.missing.iter().map(|m| m.role.as_str()).collect::<Vec<_>>().join(", ");
    let found = prospect::find_people(ctx, &project, &profile, &segment, vec![company.clone()], MAX_MISSING, MAX_MISSING, Some(&roles)).await?;
    if found > 0 {
        // Maps everyone again so the people it found get their part too. The roles it looked for stay the first map's.
        ctx.log(format!("placing the {found} people it found…")).await?;
        let people = load_people(db, &company).await?;
        let again = tidy(ctx.ask(&map(&brief, &company, &people)).await?, &people);
        save(db, input.committee_id, &Committee { missing: committee.missing, ..again }, found).await?;
    }
    Ok(Outcome { message: format!("Found {found} people for the buying committee"), output: json!({ "found": found }) })
}

async fn load_people(db: &PgPool, company: &Company) -> sqlx::Result<Vec<Person>> {
    sqlx::query_as("select id, full_name, title, status from leads where project_id = $1 and (company_id = $2 or company_domain = $3) order by created_at")
        .bind(company.project_id)
        .bind(company.id)
        .bind(&company.domain)
        .fetch_all(db)
        .await
}

async fn save(db: &PgPool, id: Uuid, committee: &Committee, found: usize) -> sqlx::Result<()> {
    sqlx::query("update buying_committees set committee = $2, found = $3 where id = $1").bind(id).bind(json!(committee)).bind(found as i32).execute(db).await?;
    Ok(())
}

fn map(brief: &str, company: &Company, people: &[Person]) -> Call {
    let listed = people.iter().enumerate().map(|(i, p)| format!("[p{}] {}, {} ({})", i + 1, p.full_name, p.title, p.status)).collect::<Vec<_>>().join("\n");
    let prompt = format!(
        "We sell:\n{brief}\n\nThe prospect: {name} ({domain}), {employees} employees. {description}\n\n\
         The people there we know, each with their id, title and where they are with us:\n{listed}\n\n\
         1. summary: in one sentence, who decides on buying our offer at a company like this and who else has a say.\n\
         2. members: for each person above, their part in that decision: decision_maker (signs off), champion (wants it and \
         pushes for it, like someone who replied with interest), influencer (has a say), user (would use it day to day) or \
         unknown. A note of under 8 words on what they'd care about.\n\
         3. missing: up to {MAX_MISSING} roles that usually take part in this decision and nobody above fills, most important \
         first. Give each as job titles to search for in under 6 words, like \"CFO or VP Finance\", and why they matter here \
         in one short sentence. Empty when the people above cover it, like at a small business the owner runs.",
        name = company.name,
        domain = company.domain,
        employees = if company.employees.is_empty() { "an unknown number of" } else { &company.employees },
        description = company.description,
    );
    Call { budget_usd: 0.2, ..Call::writing(MAPPER, prompt, schema()) }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_committee_maps_the_people_it_knows_and_searches_only_for_the_missing_roles(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Ours') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name, domain) values ($1, 'Fieldnote', 'fieldnote.example') returning id")
            .bind(org)
            .fetch_one(&db)
            .await
            .unwrap();
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'SaaS') returning id").bind(project).fetch_one(&db).await.unwrap();
        let lead = |name: &'static str, title: &'static str, email: &'static str| {
            sqlx::query_as::<_, Lead>(
                "insert into leads (project_id, segment_id, full_name, title, email, company_name, company_domain, status)
                 values ($1, $2, $3, $4, $5, 'Acme', 'acme.example', 'hot') returning *",
            )
            .bind(project)
            .bind(segment)
            .bind(name)
            .bind(title)
            .bind(email)
            .fetch_one(&db)
        };
        let ana = lead("Ana Ruiz", "Head of Sales", "ana@acme.example").await.unwrap();
        let bo = lead("Bo Chen", "Sales Ops Manager", "bo@acme.example").await.unwrap();

        assert_eq!(company_of(&db, &ana).await.unwrap(), None, "an imported lead has only a domain");
        let id = request(&db, &ana, org).await.unwrap();
        let company = company_of(&db, &ana).await.unwrap().expect("asking adds the company");
        assert_eq!(latest(&db, company).await.unwrap().unwrap().status, "working");

        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |call| {
            if call.schema["properties"].get("people").is_some() {
                return json!({ "people": [] });
            }
            json!({
                "summary": " The CRO decides. ",
                "members": [
                    { "person": "P1", "role": "champion", "note": "Replied with interest" },
                    { "person": "p2", "role": "gatekeeper", "note": "Runs the tools" },
                    { "person": "p1", "role": "user", "note": "again" },
                    { "person": "p9", "role": "user", "note": "nobody" }
                ],
                "missing": [
                    { "role": "CRO or VP Sales", "why": "Signs off on sales tools." },
                    { "role": " ", "why": "empty" },
                    { "role": "CFO", "why": "Owns the budget." }, { "role": "IT", "why": "" }, { "role": "Legal", "why": "" }
                ]
            })
        });
        run(&ctx).await.unwrap();
        sqlx::query("update agent_runs set status = 'succeeded'").execute(&db).await.unwrap();

        let saved = latest(&db, company).await.unwrap().unwrap();
        assert_eq!((saved.id, saved.status.as_str(), saved.found), (id, "ready", 0));
        let committee: Committee = serde_json::from_value(saved.committee.unwrap()).unwrap();
        assert_eq!(committee.summary, "The CRO decides.");
        let roles: Vec<(Uuid, &str)> = committee.members.iter().map(|m| (m.lead_id, m.role.as_str())).collect();
        assert_eq!(roles, vec![(ana.id, "champion"), (bo.id, "unknown")], "known people once each, with a real role");
        assert_eq!(committee.missing.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(), vec!["CRO or VP Sales", "CFO", "IT"]);

        let prompts = super::super::fake::prompts(ctx.run.id);
        assert!(prompts[0].contains("[p1] Ana Ruiz, Head of Sales (hot)"), "{}", prompts[0]);
        let search = &prompts[1];
        assert!(search.contains("Only people in these roles: CRO or VP Sales, CFO, IT."), "{search}");
        assert!(search.contains("These people are leads already, so find others:\n- acme.example: Ana Ruiz, Head of Sales"), "{search}");
    }
}
