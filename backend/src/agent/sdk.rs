//! One Claude call: a prompt in, a typed JSON answer out. Drives the local `claude` CLI through
//! claude-agent-sdk-rs and turns its tool calls into run events.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, anyhow};
use claude_agent_sdk_rs::{ClaudeAgentOptions, ClaudeClient, ContentBlock, McpServerConfig, McpServers, Message, Tools};
use futures::StreamExt;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{Ctx, bluesky, channels, directory, discourse, hashnode, hiring, lemmy, linkedin, mastodon, nostr, reddit, stackexchange, threads, tumblr, x, youtube};
use crate::events;
use crate::state::AppState;

const WEB_TOOLS: [&str; 2] = ["WebSearch", "WebFetch"];

pub struct Call {
    pub system: String,
    pub prompt: String,
    pub schema: Value,
    pub web: bool,
    /// Offers the company database tool, leaving out the companies this project already has.
    pub directory: Option<Uuid>,
    /// Offers the tool that lists a company's open roles from its job board.
    pub hiring: bool,
    pub max_turns: u32,
    pub budget_usd: f64,
    pub timeout: Duration,
    /// In-process MCP servers by name, and the tools from them Claude may use. When set they replace the X tools a
    /// research call gets otherwise.
    pub servers: HashMap<String, McpServerConfig>,
    pub server_tools: Vec<String>,
}

impl Call {
    /// A task that searches and reads the web before answering.
    pub fn research(system: impl Into<String>, prompt: impl Into<String>, schema: Value, max_turns: u32) -> Self {
        Self {
            system: system.into(),
            prompt: prompt.into(),
            schema,
            web: true,
            directory: None,
            hiring: false,
            max_turns,
            budget_usd: 1.0,
            timeout: Duration::from_secs(6 * 60),
            servers: HashMap::new(),
            server_tools: vec![],
        }
    }

    /// A task that only reasons over the prompt: writing, classifying, answering.
    pub fn writing(system: impl Into<String>, prompt: impl Into<String>, schema: Value) -> Self {
        Self {
            system: system.into(),
            prompt: prompt.into(),
            schema,
            web: false,
            directory: None,
            hiring: false,
            max_turns: 4,
            budget_usd: 0.25,
            timeout: Duration::from_secs(2 * 60),
            servers: HashMap::new(),
            server_tools: vec![],
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
        let attempt = once(state, run, call, prompt).await?;
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

async fn once(state: &AppState, run: Option<&Ctx>, call: &Call, mut prompt: String) -> anyhow::Result<Attempt> {
    let tools: Vec<String> = if call.web { WEB_TOOLS.iter().map(|t| t.to_string()).collect() } else { vec![] };
    let mut allowed = tools.clone();
    let mut servers = call.servers.clone();
    allowed.extend(call.server_tools.iter().cloned());
    if let (true, true, Some(bird)) = (call.web, servers.is_empty(), &state.cfg.bird_bin) {
        servers.insert("x".to_string(), x::server(bird.clone(), None));
        allowed.extend([x::SEARCH.to_string(), x::READ.to_string()]);
        // In the task prompt, not the system prompt: the tasks' own step lists win over system guidance.
        prompt.push_str(x::GUIDANCE);
    }
    if let Some(project_id) = call.directory {
        servers.insert("directory".to_string(), directory::server(state.db.clone(), project_id));
        allowed.push(directory::SEARCH.to_string());
        prompt.push_str(directory::GUIDANCE);
    }
    if call.hiring {
        servers.insert("hiring".to_string(), hiring::server(state.db.clone(), Arc::new(crate::jobs::Boards::new()?)));
        allowed.push(hiring::JOB_OPENINGS.to_string());
        prompt.push_str(hiring::GUIDANCE);
    }
    let opts = ClaudeAgentOptions::builder()
        // `tools` only picks built-in tools. MCP tools such as the X ones just need to be allowed.
        .tools(Tools::List(tools))
        .allowed_tools(allowed)
        .mcp_servers(McpServers::Dict(servers))
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

    // ClaudeClient rather than query_stream: only the client answers the CLI's calls to in-process MCP tools.
    let mut guard = DisconnectOnDrop(Some(ClaudeClient::new(opts)));
    let client = guard.0.as_mut().expect("just set");
    client.connect().await.context("could not start the Claude CLI")?;
    let attempt = tokio::time::timeout(call.timeout, async {
        client.query(prompt).await.context("could not send the prompt to the Claude CLI")?;
        read_answer(state, run, client).await
    })
    .await;
    attempt.map_err(|_| anyhow!("Claude did not finish within {} minutes", call.timeout.as_secs().div_ceil(60)))?
}

/// Dropping a ClaudeClient does not stop the CLI: the SDK's reader task keeps it alive. So this disconnects a
/// connected client on every path, including a cancelled run, which drops `once` mid-await. Without it the CLI
/// kept working, and spending, until its turn ended. A cancel during `connect()` itself still isn't covered.
struct DisconnectOnDrop(Option<ClaudeClient>);

impl Drop for DisconnectOnDrop {
    fn drop(&mut self) {
        let (Some(mut client), Ok(runtime)) = (self.0.take(), tokio::runtime::Handle::try_current()) else { return };
        // A CLI still mid-turn won't exit on its own, and giving up on the wait drops the child, which the vendored
        // SDK spawns with kill_on_drop.
        runtime.spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(10), client.disconnect()).await;
        });
    }
}

async fn read_answer(state: &AppState, run: Option<&Ctx>, client: &ClaudeClient) -> anyhow::Result<Attempt> {
    let mut stream = client.receive_response();
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
        x::SEARCH => format!("searching X \"{}\"…", input["query"].as_str().unwrap_or_default()),
        x::READ => format!("reading X post {}…", short_url(input["post"].as_str().unwrap_or_default())),
        directory::SEARCH => {
            let words = ["q", "category", "city", "country"].iter().filter_map(|k| input[*k].as_str()).filter(|s| !s.trim().is_empty());
            format!("searching the company database for \"{}\"…", words.collect::<Vec<_>>().join(" "))
        }
        hiring::JOB_OPENINGS => format!("checking open roles at {}…", input["domain"].as_str().unwrap_or_default()),
        reddit::SEARCH => match input["subreddit"].as_str().filter(|s| !s.is_empty()) {
            Some(sr) => format!("searching r/{sr} \"{}\"…", input["query"].as_str().unwrap_or_default()),
            None => format!("searching Reddit \"{}\"…", input["query"].as_str().unwrap_or_default()),
        },
        reddit::READ => format!("reading {}…", short_url(input["thread"].as_str().unwrap_or_default())),
        bluesky::SEARCH => format!("searching Bluesky \"{}\"…", input["query"].as_str().unwrap_or_default()),
        mastodon::SEARCH => format!("searching Mastodon \"{}\"…", input["query"].as_str().unwrap_or_default()),
        linkedin::SEARCH => format!("searching LinkedIn \"{}\"…", input["query"].as_str().unwrap_or_default()),
        linkedin::PEOPLE => match input["company"].as_str().filter(|c| !c.trim().is_empty()) {
            Some(company) => format!("searching LinkedIn for people at {}…", company.trim()),
            None => format!("searching LinkedIn for {}…", input["titles"].as_str().or(input["keywords"].as_str()).unwrap_or("people")),
        },
        linkedin::PROFILE => format!("reading {}…", short_url(input["profile"].as_str().unwrap_or_default())),
        threads::SEARCH => format!("searching Threads \"{}\"…", input["query"].as_str().unwrap_or_default()),
        youtube::SEARCH => format!("searching YouTube \"{}\"…", input["query"].as_str().unwrap_or_default()),
        bluesky::READ | mastodon::READ | linkedin::READ | threads::READ => format!("reading {}…", short_url(input["post"].as_str().unwrap_or_default())),
        youtube::READ => format!("reading {}…", short_url(input["video"].as_str().unwrap_or_default())),
        stackexchange::SEARCH => format!("searching Stack Exchange \"{}\"…", input["query"].as_str().unwrap_or_default()),
        stackexchange::READ => format!("reading {}…", short_url(input["question"].as_str().unwrap_or_default())),
        discourse::SEARCH => match input["forum"].as_str().filter(|f| !f.trim().is_empty()) {
            Some(forum) => format!("searching {} \"{}\"…", short_url(forum), input["query"].as_str().unwrap_or_default()),
            None => format!("searching Discourse \"{}\"…", input["query"].as_str().unwrap_or_default()),
        },
        discourse::READ => format!("reading {}…", short_url(input["topic"].as_str().unwrap_or_default())),
        lemmy::SEARCH => format!("searching Lemmy \"{}\"…", input["query"].as_str().unwrap_or_default()),
        lemmy::READ => format!("reading {}…", short_url(input["link"].as_str().unwrap_or_default())),
        nostr::SEARCH => format!("searching Nostr \"{}\"…", input["query"].as_str().unwrap_or_default()),
        tumblr::SEARCH => format!("searching Tumblr #{}…", input["tag"].as_str().unwrap_or_default().trim_start_matches('#')),
        hashnode::SEARCH => format!("searching Hashnode #{}…", input["tag"].as_str().unwrap_or_default().trim_start_matches('#')),
        channels::DISCORD_SEARCH => format!("searching Discord \"{}\"…", input["keywords"].as_str().unwrap_or_default()),
        channels::TELEGRAM_SEARCH => format!("searching Telegram \"{}\"…", input["keywords"].as_str().unwrap_or_default()),
        nostr::READ | tumblr::READ | hashnode::READ | channels::DISCORD_READ | channels::TELEGRAM_READ => {
            format!("reading {}…", short_url(input["link"].as_str().unwrap_or_default()))
        }
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
