//! One Claude call: a prompt in, a typed JSON answer out. Drives the local `claude` CLI through
//! claude-agent-sdk-rs and turns its tool calls into run events.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use claude_agent_sdk_rs::{ClaudeAgentOptions, ContentBlock, Message, Tools, query_stream};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::Ctx;
use crate::events;
use crate::state::AppState;

const WEB_TOOLS: [&str; 2] = ["WebSearch", "WebFetch"];

pub struct Call {
    pub system: String,
    pub prompt: String,
    pub schema: Value,
    pub web: bool,
    pub max_turns: u32,
    pub budget_usd: f64,
    pub timeout: Duration,
}

impl Call {
    /// A task that searches and reads the web before answering.
    pub fn research(system: impl Into<String>, prompt: impl Into<String>, schema: Value, max_turns: u32) -> Self {
        Self {
            system: system.into(),
            prompt: prompt.into(),
            schema,
            web: true,
            max_turns,
            budget_usd: 1.0,
            timeout: Duration::from_secs(6 * 60),
        }
    }

    /// A task that only reasons over the prompt: writing, classifying, answering.
    pub fn writing(system: impl Into<String>, prompt: impl Into<String>, schema: Value) -> Self {
        Self {
            system: system.into(),
            prompt: prompt.into(),
            schema,
            web: false,
            max_turns: 4,
            budget_usd: 0.25,
            timeout: Duration::from_secs(2 * 60),
        }
    }
}

struct Attempt {
    output: Option<Value>,
    cost: f64,
    error: Option<String>,
}

/// Runs the call, retrying once when Claude ends without a structured answer that parses as `T`.
/// With a run context, tool calls become events and the cost is added to the run.
pub async fn ask<T: DeserializeOwned>(state: &AppState, run: Option<&Ctx>, call: &Call) -> anyhow::Result<T> {
    let mut problem = String::new();
    for try_no in 0..2 {
        let prompt = if try_no == 0 {
            call.prompt.clone()
        } else {
            format!(
                "{}\n\nYour previous attempt ended without a valid answer ({problem}). Use at most 3 more tool calls, then return the answer.",
                call.prompt
            )
        };
        let result = tokio::time::timeout(call.timeout, once(state, run, call, prompt)).await;
        let Ok(result) = result else {
            anyhow::bail!("Claude did not finish within {} minutes", call.timeout.as_secs().div_ceil(60));
        };
        let attempt = result?;
        if let Some(ctx) = run {
            ctx.add_cost(attempt.cost).await;
        }
        match (attempt.output, attempt.error) {
            (Some(value), _) => match serde_json::from_value::<T>(value) {
                Ok(parsed) => return Ok(parsed),
                Err(e) => problem = format!("the answer did not match the schema: {e}"),
            },
            (None, Some(error)) => problem = error,
            (None, None) => problem = "no structured answer".into(),
        }
        tracing::warn!(try_no, %problem, "claude returned no usable answer");
    }
    Err(anyhow!("Claude did not return a usable answer ({problem})"))
}

async fn once(state: &AppState, run: Option<&Ctx>, call: &Call, prompt: String) -> anyhow::Result<Attempt> {
    let tools: Vec<String> = if call.web { WEB_TOOLS.iter().map(|t| t.to_string()).collect() } else { vec![] };
    let opts = ClaudeAgentOptions::builder()
        .tools(Tools::List(tools.clone()))
        .allowed_tools(tools)
        // Never load the host user's ~/.claude settings, CLAUDE.md or plugins.
        .setting_sources(vec![])
        .cwd(std::env::temp_dir())
        .model(state.cfg.agent_model.clone())
        .system_prompt(call.system.clone())
        .max_turns(call.max_turns)
        .max_budget_usd(call.budget_usd)
        .output_format(json!({"type": "json_schema", "schema": call.schema}))
        // stderr is piped by the SDK. Reading it keeps a chatty CLI from blocking on a full pipe.
        .stderr_callback(Arc::new(|line: String| tracing::debug!(target: "claude_cli", "{}", line.trim_end())))
        .skip_version_check(true)
        .build();

    let mut stream = query_stream(prompt, Some(opts)).await.context("could not start the Claude CLI")?;
    while let Some(message) = stream.next().await {
        match message.context("Claude CLI stream failed")? {
            Message::Assistant(msg) => {
                let Some(ctx) = run else { continue };
                for block in msg.message.content {
                    match block {
                        ContentBlock::ToolUse(tool) if tool.name != "StructuredOutput" => report_tool(state, ctx, &tool.name, tool.input).await,
                        ContentBlock::Text(text) => report_text(state, ctx, &text.text).await,
                        _ => {}
                    }
                }
            }
            Message::Result(result) => {
                let error = result.is_error.then(|| result.result.clone().filter(|r| !r.is_empty()).unwrap_or(result.subtype.clone()));
                return Ok(Attempt { output: result.structured_output, cost: result.total_cost_usd.unwrap_or(0.0), error });
            }
            _ => {}
        }
    }
    Err(anyhow!("Claude CLI exited without a result"))
}

async fn report_tool(state: &AppState, ctx: &Ctx, name: &str, input: Value) {
    let line = match name {
        "WebSearch" => format!("searching \"{}\"…", input["query"].as_str().unwrap_or_default()),
        "WebFetch" => format!("fetching {}…", short_url(input["url"].as_str().unwrap_or_default())),
        other => format!("{other}…"),
    };
    let step = ctx.step();
    // `tool` carries the raw call for UIs that want it, `log` is the human line.
    let _ = events::emit(state, ctx.run.id, step, "tool", "", json!({"name": name, "input": input})).await;
    let _ = events::emit(state, ctx.run.id, step, "log", &line, json!({})).await;
}

/// Short single-line remarks ("Now checking the team page") read like progress lines. Longer text is
/// Claude thinking out loud and stays out of the log.
async fn report_text(state: &AppState, ctx: &Ctx, text: &str) {
    let text = text.trim();
    if text.is_empty() || text.len() > 120 || text.contains('\n') {
        return;
    }
    let _ = events::emit(state, ctx.run.id, ctx.step(), "log", text, json!({})).await;
}

fn short_url(url: &str) -> String {
    let rest = url.split("://").nth(1).unwrap_or(url);
    let rest = rest.strip_prefix("www.").unwrap_or(rest).trim_end_matches('/');
    if rest.chars().count() > 60 { format!("{}…", rest.chars().take(59).collect::<String>()) } else { rest.to_string() }
}
