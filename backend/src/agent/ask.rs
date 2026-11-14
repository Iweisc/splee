//! Ask Splee: answers a team's question about its pipeline, like "Who replied this week?" or "Which campaign books the most
//! meetings?". Claude reads the pipeline through the MCP route's read-only tools, run in this process as a server named
//! `splee`, so it sees exactly what one of the organization's API keys would and can't change anything.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::Context;
use claude_agent_sdk_rs::{McpServerConfig, McpToolResultContent, SdkMcpTool, ToolResult, create_sdk_mcp_server, tool};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::sdk::Call;
use super::{Ctx, Outcome};
use crate::routes::mcp;
use crate::state::AppState;

const SERVER: &str = "splee";
pub const PREFIX: &str = "mcp__splee__";
/// Earlier questions and answers it reads, so "and last month?" means something.
pub const MAX_HISTORY: usize = 6;
pub const MAX_QUESTION: usize = 2000;
/// An earlier answer is cut to this many characters.
pub const MAX_ANSWER: usize = 3000;

const SYSTEM: &str = "You answer questions from a sales team about their outbound campaigns in Splee, an outbound sales tool. \
Look things up with the splee tools before you answer, and use only what they return: never guess a number, a name or a date. \
If the tools can't answer it, say so and say what they can tell instead. You can only read. When asked to change something, say \
so and point to where in Splee the team can do it.\n\n\
Answer in the fewest words that answer it, the number or name first. Write short paragraphs and \"- \" lists, with **bold** for \
a few key words. No headings and no tables. Link every lead you name as [Full Name](lead:<lead id>) and every campaign as \
[Campaign name](campaign:<campaign id>), with ids from the tools. Address the team as \"you\".";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub question: String,
    pub answer: String,
}

#[derive(Deserialize)]
struct Input {
    question: String,
    #[serde(default)]
    history: Vec<Turn>,
}

#[derive(Deserialize)]
struct Answer {
    answer: String,
}

/// The read-only tools, acting for the organization.
fn tools(state: &AppState, org_id: Uuid) -> Vec<SdkMcpTool> {
    mcp::read_tools()
        .into_iter()
        .map(|t| {
            let name = t["name"].as_str().unwrap_or_default().to_string();
            let state = state.clone();
            let called = name.clone();
            tool!(name, t["description"].as_str().unwrap_or_default(), t["inputSchema"].clone(), move |args: Value| {
                let (state, name) = (state.clone(), called.clone());
                async move {
                    let (text, is_error) = mcp::call(&state, org_id, &name, args).await.unwrap_or_else(|| (format!("Unknown tool {name}"), true));
                    Ok(ToolResult { content: vec![McpToolResultContent::Text { text }], is_error })
                }
            })
        })
        .collect()
}

/// The progress line for one of the tools, like "looking at leads…".
pub fn progress(name: &str) -> String {
    let tool = name.trim_start_matches(PREFIX);
    let what = match tool {
        "search_companies" => "the company database",
        "find_lookalikes" => "companies like your customers",
        "get_run" => "an agent run",
        "list_emails_to_approve" => "emails waiting for approval",
        "list_company_signals" => "company news",
        other => other.trim_start_matches("list_").trim_start_matches("get_"),
    };
    format!("looking at {}…", what.replace('_', " "))
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": { "answer": { "type": "string", "description": "The answer, in the format the instructions give." } },
        "required": ["answer"],
        "additionalProperties": false
    })
}

/// The run's input: the question and the last few turns before it, with long answers cut.
pub fn input(question: &str, history: Vec<Turn>) -> Value {
    let skip = history.len().saturating_sub(MAX_HISTORY);
    let history: Vec<Turn> = history
        .into_iter()
        .skip(skip)
        .map(|t| Turn { question: t.question.chars().take(MAX_QUESTION).collect(), answer: t.answer.chars().take(MAX_ANSWER).collect() })
        .collect();
    json!({ "question": question.trim(), "history": history })
}

pub async fn run(ctx: &Ctx) -> anyhow::Result<Outcome> {
    let input: Input = ctx.input()?;
    let project_id = ctx.project_id()?;
    let org_id = ctx.run.org_id.context("this run has no organization")?;
    let project: String = sqlx::query_scalar("select name from projects where id = $1").bind(project_id).fetch_one(ctx.db()).await?;

    let earlier = input
        .history
        .iter()
        .map(|t| format!("Q: {}\nA: {}", t.question.trim(), t.answer.trim()))
        .collect::<Vec<_>>()
        .join("\n\n");
    let prompt = format!(
        "Today is {today}. The team is in the project \"{project}\", project_id {project_id}. Questions are about it unless they \
         name another project, which list_projects finds.\n\n{earlier}Question: {question}",
        today = chrono::Utc::now().format("%A, %B %-d, %Y"),
        earlier = if earlier.is_empty() { String::new() } else { format!("Earlier in this conversation:\n{earlier}\n\n") },
        question = input.question.trim(),
    );

    let tools = tools(&ctx.state, org_id);
    let allowed = tools.iter().map(|t| format!("{PREFIX}{}", t.name)).collect();
    let call = Call {
        max_turns: 16,
        budget_usd: 0.5,
        timeout: Duration::from_secs(3 * 60),
        servers: HashMap::from([(SERVER.to_string(), McpServerConfig::Sdk(create_sdk_mcp_server(SERVER, "1.0.0", tools)))]),
        server_tools: allowed,
        ..Call::writing(SYSTEM, prompt, schema())
    };
    let answer: Answer = ctx.ask(&call).await?;
    Ok(Outcome { message: "Answered".into(), output: json!({ "answer": answer.answer.trim() }) })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicI32;
    use std::sync::{Arc, Mutex};

    use sqlx::PgPool;

    use super::*;
    use crate::models::AgentRun;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn ask_reads_only_its_own_organization_and_remembers_the_conversation(db: PgPool) {
        let org = |name: &'static str| sqlx::query_scalar::<_, Uuid>("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db);
        let (ours, theirs) = (org("Ours").await.unwrap(), org("Theirs").await.unwrap());
        let project = |org: Uuid, name: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db)
        };
        let (ours_project, theirs_project) = (project(ours, "Smile Dental").await.unwrap(), project(theirs, "Rival").await.unwrap());
        let history = (0..8).map(|i| Turn { question: format!("q{i}"), answer: format!("a{i}") }).collect();
        super::super::enqueue(&db, super::super::kind::ASK, Some(ours_project), Some(ours), input(" Who replied? ", history)).await.unwrap();
        let started = sqlx::query_as::<_, AgentRun>("update agent_runs set status = 'running' returning *").fetch_one(&db).await.unwrap();
        let ctx = Ctx { state: crate::autopilot::tests::state(db.clone()), run: started, step: AtomicI32::new(0) };
        let offered = Arc::new(Mutex::new(vec![]));
        let seen = offered.clone();
        super::super::fake::answer_with(ctx.run.id, move |call| {
            *seen.lock().unwrap() = call.server_tools.clone();
            json!({ "answer": " [Ana Ruiz](lead:1) replied. " })
        });

        let outcome = run(&ctx).await.unwrap();
        assert_eq!(outcome.output["answer"], "[Ana Ruiz](lead:1) replied.");
        let prompt = &super::super::fake::prompts(ctx.run.id)[0];
        assert!(prompt.contains(&format!("\"Smile Dental\", project_id {ours_project}")), "{prompt}");
        assert!(prompt.contains("Q: q2\nA: a2") && !prompt.contains("q1\n") && prompt.ends_with("Question: Who replied?"), "the last {MAX_HISTORY} turns: {prompt}");

        let offered = offered.lock().unwrap().clone();
        assert!(offered.contains(&format!("{PREFIX}list_leads")) && offered.contains(&format!("{PREFIX}get_analytics")));
        for writes in ["create_campaign", "approve_email", "move_deal", "find_people", "search_local_businesses"] {
            assert!(!offered.contains(&format!("{PREFIX}{writes}")), "{writes} is offered");
        }

        let list_projects = tools(&ctx.state, ours).into_iter().find(|t| t.name == "list_projects").unwrap();
        let listed = list_projects.handler.handle(json!({})).await.unwrap();
        let McpToolResultContent::Text { text } = &listed.content[0] else { panic!("not text") };
        assert!(text.contains(&ours_project.to_string()) && !text.contains(&theirs_project.to_string()), "{text}");
    }

    #[test]
    fn progress_lines_read_like_a_person_looking() {
        assert_eq!(progress("mcp__splee__list_leads"), "looking at leads…");
        assert_eq!(progress("mcp__splee__get_analytics"), "looking at analytics…");
        assert_eq!(progress("mcp__splee__list_call_tasks"), "looking at call tasks…");
        assert_eq!(progress("mcp__splee__search_companies"), "looking at the company database…");
    }
}
