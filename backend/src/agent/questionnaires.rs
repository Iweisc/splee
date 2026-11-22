//! Questionnaires (see questionnaires.rs): the library's approved answer fills a question it has seen, and the agent
//! answers the rest in batches from the project's Materials, the library and the answers published in deal rooms,
//! saying which it couldn't source rather than guessing.

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::types::Json;
use uuid::Uuid;

use super::explore::load_project;
use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::questionnaires::{MAX_ANSWER, key};

const WRITER: &str = "You answer security questionnaires and RFPs for a B2B company, for Splee, an outbound sales tool, like its \
careful security and solutions lead. Use only what you're given. Never claim a certification, control, feature, number or \
policy the sources don't state. When they don't cover a question, say the team will confirm. Plain text only, no markdown.";

const PER_BATCH: usize = 25;
/// Characters of Materials and of the library the agent reads.
const MATERIALS_CHARS: usize = 40_000;
const LIBRARY_CHARS: usize = 25_000;

#[derive(Deserialize)]
struct Input {
    questionnaire_id: Uuid,
    #[serde(default)]
    retry: bool,
}

#[derive(Deserialize)]
struct Answered {
    number: usize,
    answer: String,
    confidence: String,
    #[serde(default)]
    sources: Vec<String>,
}

#[derive(Deserialize)]
struct Batch {
    answers: Vec<Answered>,
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "answers": { "type": "array", "items": { "type": "object", "properties": {
                "number": { "type": "integer" },
                "answer": { "type": "string" },
                "confidence": { "type": "string", "enum": ["sourced", "partial", "unknown"] },
                "sources": { "type": "array", "items": { "type": "string" } }
            }, "required": ["number", "answer", "confidence", "sources"] } }
        },
        "required": ["answers"]
    })
}

/// The entries one after another, up to `budget` characters, and always the first.
fn within(entries: impl IntoIterator<Item = String>, budget: usize) -> String {
    let mut out = String::new();
    for e in entries {
        if !out.is_empty() && out.len() + e.len() > budget {
            break;
        }
        out.push_str(&e);
        out.push('\n');
    }
    out
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let db = ctx.db();
    let project_id = ctx.project_id()?;
    let id: Option<Uuid> = sqlx::query_scalar("select id from questionnaires where id = $1 and project_id = $2").bind(input.questionnaire_id).bind(project_id).fetch_optional(db).await?;
    let id = id.ok_or_else(|| anyhow::anyhow!("the questionnaire was deleted"))?;

    // Each with the answer it has now, which a write only replaces while it's still there, so the team's edits during a run stay.
    let open: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "select id, question, section, answer from questionnaire_items
         where questionnaire_id = $1 and approved_at is null and (answer = '' or ($2 and confidence = 'unknown')) order by position",
    )
    .bind(id)
    .bind(input.retry)
    .fetch_all(db)
    .await?;
    let library: Vec<(String, String, String)> =
        sqlx::query_as("select key, question, answer from answer_library where project_id = $1 order by updated_at desc").bind(project_id).fetch_all(db).await?;
    let known: HashMap<&str, &str> = library.iter().map(|(k, _, a)| (k.as_str(), a.as_str())).collect();

    let mut from_library = 0;
    let mut rest = vec![];
    for (item, question, section, before) in open {
        match known.get(key(&question).as_str()) {
            Some(answer) => from_library += usize::from(write(db, item, &before, answer, "library", &["Answer library"]).await?),
            None => rest.push((item, question, section, before)),
        }
    }

    let materials: Vec<(String, String, String)> =
        sqlx::query_as("select kind, title, content from materials where project_id = $1 order by created_at").bind(project_id).fetch_all(db).await?;
    let titles: Vec<&str> = materials.iter().map(|(_, t, _)| t.as_str()).collect();
    let sources = within(materials.iter().map(|(kind, title, content)| format!("[{title}] ({kind})\n{}", content.chars().take(6000).collect::<String>())), MATERIALS_CHARS);
    let published: Vec<(String, String)> = sqlx::query_as(
        "select q.question, q.answer from deal_room_questions q join deal_rooms r on r.id = q.room_id
         where r.project_id = $1 and q.answer is not null order by q.answered_at desc limit 40",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let approved = within(
        library.iter().map(|(_, q, a)| (q, a)).chain(published.iter().map(|(q, a)| (q, a))).map(|(q, a)| format!("Q: {q}\nA: {}", a.chars().take(600).collect::<String>())),
        LIBRARY_CHARS,
    );
    let (_, profile) = load_project(ctx, project_id).await?;

    let (mut answered, mut unknown) = (0, 0);
    for (n, batch) in rest.chunks(PER_BATCH).enumerate() {
        ctx.log(format!("answering questions {} to {} of {}…", n * PER_BATCH + 1, n * PER_BATCH + batch.len(), rest.len())).await?;
        let questions = batch
            .iter()
            .enumerate()
            .map(|(i, (_, q, section, _))| format!("{}. {}{q}", i + 1, if section.is_empty() { String::new() } else { format!("[{section}] ") }))
            .collect::<Vec<_>>()
            .join("\n");
        let prompt = format!(
            "We sell {name}. {description}\n\n\
             Our materials, each under its [title]:\n{sources}\n\n\
             Answers we approved before:\n{approved}\n\n\
             A buyer's questionnaire asks:\n{questions}\n\n\
             Answer each question by its number, as we'd write it in their file: direct, first person plural, usually 1 to 3 \
             sentences, starting with yes or no when it asks one.\n\
             - confidence: sourced when the materials or approved answers state it, partial when they cover part of it, \
             unknown when they don't. For unknown, write \"We'll confirm this.\" and anything the sources do say on it.\n\
             The buyer reads every answer, so never mention our materials, sources or what they leave out.\n\
             - sources: the [titles] of the materials it rests on, or \"Approved answers\".",
            name = profile.name,
            description = profile.description,
            approved = if approved.is_empty() { "None yet.\n".to_string() } else { approved.clone() },
        );
        let call = Call { timeout: Duration::from_secs(4 * 60), budget_usd: 0.5, ..Call::writing(WRITER, prompt, schema()) };
        let reply: Batch = ctx.ask(&call).await?;
        for a in reply.answers {
            let Some((item, _, _, before)) = a.number.checked_sub(1).and_then(|i| batch.get(i)) else { continue };
            let text: String = a.answer.trim().chars().take(MAX_ANSWER).collect();
            if text.is_empty() {
                continue;
            }
            let confidence = match a.confidence.as_str() {
                "sourced" | "partial" => a.confidence.as_str(),
                _ => "unknown",
            };
            let cited: Vec<&str> = a.sources.iter().map(|s| s.trim().trim_matches(['[', ']'])).filter(|s| titles.contains(s) || *s == "Approved answers").collect();
            // An answer that cites nothing it was given isn't sourced, whatever it says.
            let confidence = if cited.is_empty() && confidence != "unknown" { "partial" } else { confidence };
            if write(db, *item, before, &text, confidence, &cited).await? {
                answered += 1;
                unknown += usize::from(confidence == "unknown");
            }
        }
    }
    Ok(Outcome {
        message: format!("Answered {} questions, {from_library} from the library. {unknown} need the team", from_library + answered),
        output: json!({ "library": from_library, "answered": answered, "unknown": unknown }),
    })
}

/// Fills an item's answer unless someone approved it or changed it from `before` since the run read it. Whether it did.
async fn write(db: &sqlx::PgPool, item: Uuid, before: &str, answer: &str, confidence: &str, sources: &[&str]) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "update questionnaire_items set answer = $3, confidence = $4, sources = $5 where id = $1 and approved_at is null and answer = $2",
    )
    .bind(item)
    .bind(before)
    .bind(answer)
    .bind(confidence)
    .bind(Json(sources))
    .execute(db)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::fake::run_next;
    use crate::questionnaires::{self as q, Table};

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_library_answers_what_it_has_seen_and_the_agent_sources_the_rest(db: sqlx::PgPool) {
        let t = crate::agent::practice::fixture::team(&db).await;
        sqlx::query("insert into materials (project_id, kind, title, content) values ($1, 'faq', 'Security', 'SOC 2 Type II since 2025. Data stays in Frankfurt.')")
            .bind(t.project)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into answer_library (project_id, key, question, answer) values ($1, 'do you support sso', 'Do you support SSO?', 'Yes, with Okta and Azure AD.')")
            .bind(t.project)
            .execute(&db)
            .await
            .unwrap();
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let table = Table {
            headers: s(&["ID", "Domain", "Question", "Vendor response"]),
            rows: vec![
                s(&["1", "Access", "Do you support SSO", ""]),
                s(&["Part 2: data and people", "", "", ""]),
                s(&["2", "Compliance", "Are you SOC 2 certified?", ""]),
                s(&["3", "Data", "Where is customer data stored?", ""]),
                s(&["4", "HR", "Do staff get background checks?", ""]),
                s(&["", "", "", ""]),
            ],
        };
        let mut tx = db.begin().await.unwrap();
        let id = q::create(&mut tx, t.project, t.sam, "Lumenfold security review", table).await.unwrap();
        q::answer(&mut tx, id, t.project, t.org, false).await.unwrap();
        tx.commit().await.unwrap();

        let (outcome, prompt) = run_next(
            &db,
            "questionnaire",
            json!({ "answers": [
                { "number": 1, "answer": "Yes, SOC 2 Type II since 2025.", "confidence": "sourced", "sources": ["[Security]"] },
                { "number": 2, "answer": "In Frankfurt.", "confidence": "sourced", "sources": ["A blog post"] },
                { "number": 3, "answer": "We'll confirm this.", "confidence": "unknown", "sources": [] },
                { "number": 9, "answer": "x", "confidence": "sourced", "sources": [] }
            ] }),
        )
        .await;
        assert!(prompt.contains("1. [Compliance] Are you SOC 2 certified?") && !prompt.contains("Do you support SSO\n2."), "the library's question isn't asked: {prompt}");
        assert!(prompt.contains("[Security] (faq)") && prompt.contains("A: Yes, with Okta and Azure AD."), "{prompt}");
        assert_eq!(outcome.output, json!({ "library": 1, "answered": 3, "unknown": 1 }));
        let d = q::detail(&db, id).await.unwrap();
        let got: Vec<_> = d.items.iter().map(|i| (i.section.as_str(), i.confidence.as_deref(), i.sources.0.clone())).collect();
        assert_eq!(
            got,
            vec![
                ("Access", Some("library"), vec!["Answer library".to_string()]),
                ("Compliance", Some("sourced"), vec!["Security".to_string()]),
                ("Data", Some("partial"), vec![]),
                ("HR", Some("unknown"), vec![])
            ],
            "a source it wasn't given doesn't count, and the blank row isn't a question"
        );
        assert_eq!((d.summary.questions, d.summary.answered, d.summary.unknown), (4, 4, 1));

        assert_eq!(q::approve_sourced(&db, id, t.project, t.sam).await.unwrap(), 2);
        assert!(q::save(&db, d.items[3].id, t.sam, " ", true).await.is_err(), "an empty answer can't be approved");
        q::save(&db, d.items[3].id, t.sam, "Yes, every hire.", true).await.unwrap();
        let library: Vec<String> = sqlx::query_scalar("select key from answer_library order by key").fetch_all(&db).await.unwrap();
        assert_eq!(library, vec!["are you soc 2 certified", "do staff get background checks", "do you support sso"]);

        let (_, headers, rows) = q::export(&db, id).await.unwrap();
        assert_eq!(headers, vec!["ID", "Domain", "Question", "Vendor response"], "the file's own answer column is filled");
        assert_eq!(rows.len(), 6, "every row comes back, the heading and the blank one too");
        assert_eq!(rows[1], vec!["Part 2: data and people", "", "", ""]);
        assert_eq!(rows[4], vec!["4", "HR", "Do staff get background checks?", "Yes, every hire."]);
        let numbers: Vec<_> = d.items.iter().map(|i| (i.position, i.number)).collect();
        assert_eq!(numbers, vec![(0, 1), (2, 2), (3, 3), (4, 4)], "questions are numbered past the heading");

        // Trying again only asks what it couldn't source and nobody approved.
        let mut tx = db.begin().await.unwrap();
        q::answer(&mut tx, id, t.project, t.org, true).await.unwrap();
        tx.commit().await.unwrap();
        let (outcome, _) = run_next(&db, "questionnaire", json!({ "answers": [] })).await;
        assert_eq!(outcome.output["library"], 0);
    }
}
