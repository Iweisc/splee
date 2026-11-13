//! Research questions: the team asks something about every company, like "Do they sell to hospitals?", and the agent
//! answers it for each company from its website and the web. Each run answers every picked question for up to `CHUNK`
//! companies, one Claude call per company, so a company's site is read once for all its questions.

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome, kind};
use crate::models::{ResearchAnswer, ResearchQuestion};

pub const MAX_QUESTIONS: i64 = 10;
/// Companies per run. Small enough that cancelling a run stops little work, big enough that 200 companies are 10 runs.
pub const CHUNK: usize = 20;
/// Companies one request can queue.
pub const MAX_COMPANIES: usize = 200;

/// The answer's status as the app shows it. A queued answer whose run stopped, cancelled or failed, without getting to
/// it reads as failed, so it can be asked again instead of spinning for good. Expects the answer as `a`.
const STATUS: &str = "case when a.status = 'queued' and not exists (select 1 from agent_runs r where r.id = a.run_id and r.status in ('queued', 'running'))
                      then 'failed' else a.status end";

const RESEARCHER: &str = "You research companies for Splee, an outbound sales tool, so a salesperson knows who to write to and what to say. \
Answer each question from the company's own website first, then other public pages about it. Answer only what a page you read \
shows. When nothing you read shows it, the status is unknown: a wrong answer costs more than a missing one.";

/// The project's answers for these domains. Every domain when `domains` is None.
pub async fn answers(db: &PgPool, project_id: Uuid, domains: Option<&[String]>) -> sqlx::Result<Vec<ResearchAnswer>> {
    sqlx::query_as::<_, ResearchAnswer>(&format!(
        "select a.question_id, a.domain::text as domain, {STATUS} as status, a.answer, a.evidence, a.source_url, a.updated_at
         from research_answers a where a.project_id = $1 and ($2::text[] is null or a.domain = any($2::citext[]))"
    ))
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await
}

/// Queues runs that answer `questions` for `domains`, `CHUNK` companies a run, and marks their answers queued so the
/// page shows them as researching. The caller checks the questions and domains belong to the project.
pub async fn enqueue(db: &PgPool, project_id: Uuid, org_id: Uuid, questions: &[Uuid], domains: &[String]) -> sqlx::Result<usize> {
    let mut tx = db.begin().await?;
    let mut runs = 0;
    for chunk in domains.chunks(CHUNK) {
        let input = json!({ "question_ids": questions, "domains": chunk });
        let run = super::enqueue(&mut *tx, kind::RESEARCH_COMPANIES, Some(project_id), Some(org_id), input).await?;
        sqlx::query(
            "insert into research_answers (question_id, project_id, domain, status, run_id)
             select q, $1, d, 'queued', $4 from unnest($2::uuid[]) q, unnest($3::text[]) d
             on conflict (question_id, domain) do update set status = 'queued', run_id = excluded.run_id, updated_at = now()",
        )
        .bind(project_id)
        .bind(questions)
        .bind(chunk)
        .bind(run)
        .execute(&mut *tx)
        .await?;
        runs += 1;
    }
    tx.commit().await?;
    Ok(runs)
}

/// The answers the writer may use, by lowercase domain: `{question, answer}` for each question set to be used in emails
/// that has a real answer.
pub async fn briefs(db: &PgPool, project_id: Uuid, domains: &[String]) -> sqlx::Result<HashMap<String, Vec<Value>>> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "select lower(a.domain::text), q.question, q.answer_type, a.answer from research_answers a
         join research_questions q on q.id = a.question_id
         where a.project_id = $1 and a.domain = any($2::citext[]) and a.status = 'answered' and q.use_in_emails
         order by q.position, q.created_at",
    )
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await?;
    let mut out: HashMap<String, Vec<Value>> = HashMap::new();
    for (domain, question, answer_type, answer) in rows {
        let answer = if answer_type == "yes_no" { if answer == "yes" { "Yes" } else { "No" }.to_string() } else { answer };
        out.entry(domain).or_default().push(json!({ "question": question, "answer": answer }));
    }
    Ok(out)
}

#[derive(Deserialize)]
struct Input {
    question_ids: Vec<Uuid>,
    domains: Vec<String>,
}

#[derive(Deserialize)]
pub(super) struct Answers {
    answers: Vec<Answer>,
}

#[derive(Deserialize)]
struct Answer {
    id: String,
    status: String,
    #[serde(default)]
    answer: String,
    #[serde(default)]
    evidence: String,
    #[serde(default)]
    source_url: String,
}

#[derive(sqlx::FromRow)]
struct Company {
    domain: String,
    name: String,
    description: String,
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let project_id = ctx.project_id()?;
    let questions = sqlx::query_as::<_, ResearchQuestion>(
        "select * from research_questions where project_id = $1 and id = any($2) order by position, created_at",
    )
    .bind(project_id)
    .bind(&input.question_ids)
    .fetch_all(ctx.db())
    .await?;
    if questions.is_empty() {
        return Ok(Outcome::new("The questions were deleted"));
    }
    let companies = sqlx::query_as::<_, Company>(
        "select domain::text as domain, name, description from companies where project_id = $1 and domain = any($2::citext[]) order by domain",
    )
    .bind(project_id)
    .bind(&input.domains)
    .fetch_all(ctx.db())
    .await?;
    let (_, profile) = load_project(ctx, project_id).await?;
    let ids: Vec<Uuid> = questions.iter().map(|q| q.id).collect();
    let (mut answered, mut researched, mut failed) = (0, 0, 0);
    for company in &companies {
        // A newer run may have taken over this company's answers, when the questions were asked again meanwhile.
        let mine: Vec<Uuid> = sqlx::query_scalar("select question_id from research_answers where question_id = any($1) and domain = $2 and run_id = $3")
            .bind(&ids)
            .bind(&company.domain)
            .bind(ctx.run.id)
            .fetch_all(ctx.db())
            .await?;
        let asked: Vec<&ResearchQuestion> = questions.iter().filter(|q| mine.contains(&q.id)).collect();
        if asked.is_empty() {
            continue;
        }
        ctx.log(format!("researching {}…", company.domain)).await?;
        match ctx.ask::<Answers>(&call(&profile.name, &profile.description, company, &asked)).await {
            Ok(found) => {
                researched += 1;
                answered += save(ctx, company, &asked, found).await?;
            }
            Err(e) => {
                failed += 1;
                tracing::warn!(run_id = %ctx.run.id, domain = %company.domain, error = %e, "could not research a company");
                sqlx::query("update research_answers set status = 'failed', evidence = '', source_url = '', updated_at = now() where question_id = any($1) and domain = $2 and run_id = $3")
                    .bind(&ids)
                    .bind(&company.domain)
                    .bind(ctx.run.id)
                    .execute(ctx.db())
                    .await?;
            }
        }
    }
    // Domains whose company was removed before the run reached them have nothing to read.
    sqlx::query("update research_answers set status = 'failed', updated_at = now() where run_id = $1 and status = 'queued'").bind(ctx.run.id).execute(ctx.db()).await?;
    Ok(Outcome {
        message: format!("Answered {answered} questions at {researched} {}", if researched == 1 { "company" } else { "companies" }),
        output: json!({ "companies": researched, "answered": answered, "failed": failed }),
    })
}

fn call(us: &str, what_we_do: &str, company: &Company, questions: &[&ResearchQuestion]) -> Call {
    let list = questions
        .iter()
        .enumerate()
        .map(|(i, q)| {
            let kind = if q.answer_type == "yes_no" { "yes or no" } else { "a short answer of at most 12 words" };
            format!("q{} ({kind}): {}", i + 1, q.question.trim())
        })
        .collect::<Vec<_>>()
        .join("\n");
    let about = if company.description.trim().is_empty() { String::new() } else { format!(" What we know: {}", company.description.trim()) };
    let us = if us.trim().is_empty() { String::new() } else { format!("The team asking sells {us}: {}\n\n", what_we_do.trim()) };
    let prompt = format!(
        "{us}Research {name} ({domain}).{about}\n\n\
         Answer these questions about {name}:\n{list}\n\n\
         Start with https://{domain} and its pages, then search the web when the site doesn't say. Use at most 8 tool calls.\n\
         Return one item per question with its id (q1, q2…), a status (answered, or unknown when nothing you read shows it), the \
         answer (\"yes\" or \"no\" for a yes or no question, else the short answer, empty when unknown), the evidence in one sentence \
         saying what the page shows, and the URL of that page as source_url.",
        name = if company.name.trim().is_empty() { company.domain.as_str() } else { company.name.trim() },
        domain = company.domain,
    );
    let schema = json!({
        "type": "object",
        "properties": {
            "answers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "status": { "type": "string", "enum": ["answered", "unknown"] },
                        "answer": { "type": "string" },
                        "evidence": { "type": "string" },
                        "source_url": { "type": "string" }
                    },
                    "required": ["id", "status", "answer", "evidence", "source_url"]
                }
            }
        },
        "required": ["answers"]
    });
    let mut call = Call::research(RESEARCHER, prompt, schema, 12);
    call.budget_usd = 0.5;
    call.timeout = std::time::Duration::from_secs(4 * 60);
    call
}

/// Saves the company's answers and returns how many were answered. A question Claude skipped, or answered outside its
/// type, is unknown.
async fn save(ctx: &Ctx, company: &Company, questions: &[&ResearchQuestion], found: Answers) -> anyhow::Result<usize> {
    let mut answered = 0;
    for (i, q) in questions.iter().enumerate() {
        let id = format!("q{}", i + 1);
        let a = found.answers.iter().find(|a| a.id.trim().eq_ignore_ascii_case(&id));
        let (status, answer) = match a {
            Some(a) if a.status == "answered" => match normalize(&q.answer_type, &a.answer) {
                Some(answer) => ("answered", answer),
                None => ("unknown", String::new()),
            },
            _ => ("unknown", String::new()),
        };
        if status == "answered" {
            answered += 1;
        }
        let evidence: String = a.map(|a| a.evidence.trim().chars().take(500).collect()).unwrap_or_default();
        // Only a web link: it shows as a link on the page.
        let source = a.map(|a| a.source_url.trim()).filter(|u| u.starts_with("https://") || u.starts_with("http://")).unwrap_or_default();
        sqlx::query(
            "update research_answers set status = $4, answer = $5, evidence = $6, source_url = $7, updated_at = now()
             where question_id = $1 and domain = $2 and run_id = $3",
        )
        .bind(q.id)
        .bind(&company.domain)
        .bind(ctx.run.id)
        .bind(status)
        .bind(&answer)
        .bind(&evidence)
        .bind(source)
        .execute(ctx.db())
        .await?;
    }
    Ok(answered)
}

/// "yes" or "no" for a yes or no question, whatever case or punctuation Claude used, and the trimmed text otherwise.
fn normalize(answer_type: &str, answer: &str) -> Option<String> {
    let answer = answer.trim();
    if answer_type == "yes_no" {
        let word = answer.trim_end_matches(['.', '!']).to_lowercase();
        return match word.as_str() {
            "yes" | "true" => Some("yes".into()),
            "no" | "false" => Some("no".into()),
            _ => None,
        };
    }
    (!answer.is_empty()).then(|| answer.chars().take(200).collect())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;

    use super::*;
    use crate::models::AgentRun;

    #[test]
    fn yes_no_answers_are_read_loosely_and_anything_else_is_unknown() {
        assert_eq!(normalize("yes_no", " Yes. ").as_deref(), Some("yes"));
        assert_eq!(normalize("yes_no", "NO").as_deref(), Some("no"));
        assert_eq!(normalize("yes_no", "Probably"), None);
        assert_eq!(normalize("text", "  HubSpot "), Some("HubSpot".into()));
        assert_eq!(normalize("text", "  "), None);
    }

    async fn setup(db: &PgPool) -> (Uuid, Uuid, Uuid, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar(
            "insert into projects (org_id, name, company) values ($1, 'Acme', '{\"name\": \"Fieldnote\", \"description\": \"Scheduling for clinics\"}') returning id",
        )
        .bind(org)
        .fetch_one(db)
        .await
        .unwrap();
        for (domain, name) in [("smile.example", "Smile Dental"), ("gone.example", "Gone")] {
            sqlx::query("insert into companies (project_id, domain, name) values ($1, $2, $3)").bind(project).bind(domain).bind(name).execute(db).await.unwrap();
        }
        let yes_no: Uuid = sqlx::query_scalar(
            "insert into research_questions (project_id, label, question, answer_type, use_in_emails, position) values ($1, 'Hospitals', 'Do they sell to hospitals?', 'yes_no', true, 0) returning id",
        )
        .bind(project)
        .fetch_one(db)
        .await
        .unwrap();
        let text: Uuid = sqlx::query_scalar(
            "insert into research_questions (project_id, label, question, answer_type, position) values ($1, 'CRM', 'Which CRM do they use?', 'text', 1) returning id",
        )
        .bind(project)
        .fetch_one(db)
        .await
        .unwrap();
        (org, project, yes_no, text)
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_run_answers_every_question_per_company_and_the_writer_gets_the_ones_for_emails(db: PgPool) {
        let (org, project, yes_no, text) = setup(&db).await;
        let domains = vec!["smile.example".to_string(), "missing.example".to_string()];
        assert_eq!(enqueue(&db, project, org, &[yes_no, text], &domains).await.unwrap(), 1);
        let queued = answers(&db, project, None).await.unwrap();
        assert_eq!(queued.len(), 4);
        assert!(queued.iter().all(|a| a.status == "queued"));

        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' where kind = 'research_companies' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        super::super::fake::answer_with(ctx.run.id, |_| {
            json!({ "answers": [
                { "id": "q1", "status": "answered", "answer": "Yes.", "evidence": "Their site lists three hospital clients.", "source_url": "https://smile.example/clients" },
                { "id": "Q2", "status": "answered", "answer": "", "evidence": "", "source_url": "not a url" }
            ]})
        });
        let outcome = run(&ctx).await.unwrap();
        assert_eq!(outcome.output, json!({ "companies": 1, "answered": 1, "failed": 0 }));
        let prompts = super::super::fake::prompts(ctx.run.id);
        assert_eq!(prompts.len(), 1, "one call per company, not per question");
        assert!(prompts[0].contains("q1 (yes or no): Do they sell to hospitals?") && prompts[0].contains("q2 (a short answer") && prompts[0].contains("sells Fieldnote"), "{}", prompts[0]);

        let got = answers(&db, project, Some(&["SMILE.example".to_string()])).await.unwrap();
        let hospitals = got.iter().find(|a| a.question_id == yes_no).unwrap();
        assert_eq!((hospitals.status.as_str(), hospitals.answer.as_str(), hospitals.source_url.as_str()), ("answered", "yes", "https://smile.example/clients"));
        let crm = got.iter().find(|a| a.question_id == text).unwrap();
        assert_eq!((crm.status.as_str(), crm.source_url.as_str()), ("unknown", ""), "an empty answer is unknown and a bad link is dropped");
        let missing = answers(&db, project, Some(&["missing.example".to_string()])).await.unwrap();
        assert!(missing.iter().all(|a| a.status == "failed"), "a domain with no company can't stay queued");

        let briefs = briefs(&db, project, &["smile.example".to_string()]).await.unwrap();
        assert_eq!(briefs["smile.example"], vec![json!({ "question": "Do they sell to hospitals?", "answer": "Yes" })], "only questions used in emails, with real answers");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn answers_left_queued_by_a_cancelled_run_read_as_failed(db: PgPool) {
        let (org, project, yes_no, _) = setup(&db).await;
        enqueue(&db, project, org, &[yes_no], &["smile.example".to_string()]).await.unwrap();
        assert_eq!(answers(&db, project, None).await.unwrap()[0].status, "queued");
        sqlx::query("update agent_runs set status = 'cancelled'").execute(&db).await.unwrap();
        assert_eq!(answers(&db, project, None).await.unwrap()[0].status, "failed");
    }
}
