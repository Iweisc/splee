//! Splee as a remote MCP server, so ChatGPT, Claude, Cursor and other MCP clients can prospect from chat. It serves both eras of
//! https://modelcontextprotocol.io/specification/2026-07-28 over Streamable HTTP: the stateless 2026-07-28 revision, and the
//! initialize-based 2025-11-25 and 2025-06-18 ones. Every POST gets one JSON answer, with no SSE stream or session. A request
//! signs in with an API key or an access token from Splee's OAuth (routes/oauth.rs), and each tool calls the matching public
//! API v1 handler for that organization.
//!
//! This file is the protocol. run.rs calls the API handler behind each tool, and tools.rs describes the tools to the client.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::Url;
use serde_json::{Value, json};

use super::oauth;
use crate::auth::{ApiKeyAuth, KeyAuth};
use crate::error::AppError;
use crate::state::AppState;

mod run;
#[cfg(test)]
mod tests;
mod tools;

use run::run;
use tools::tools;

pub fn router() -> Router<AppState> {
    // No stream for the server to push on and no session to end, which the spec allows by answering 405.
    Router::new().route("/mcp", post(handle).get(not_allowed).delete(not_allowed))
}

/// The revisions with an initialize handshake, newest first.
const LEGACY: &[&str] = &["2025-11-25", "2025-06-18"];
/// The stateless revision: no initialize, and each request names its version in _meta and the MCP-Protocol-Version header.
const STATELESS: &str = "2026-07-28";
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
/// How long a stateless client may keep tools/list and server/discover. The tools only change with a new Splee version.
const CACHE_MS: u64 = 3_600_000;

const INSTRUCTIONS: &str = "Splee finds companies and people for outbound email campaigns and writes the emails. \
A typical loop: list_projects (or create_project for a new business), then list_campaigns or create_campaign, search_companies (fill_company_filters \
turns a sentence into its filters) and add_companies, find_people, write_emails, then list_emails_to_approve and approve_email or skip_email. \
get_campaign reads a campaign's whole definition and update_campaign changes it, start_campaign, stop_campaign and approve_campaign run it, \
get_campaign_send_state says whether it's sending and what holds it, archive_campaign puts away one that's done and unarchive_campaign brings it \
back, duplicate_campaign copies one as a new draft, and update_project_controls sets the project's sending, limits, schedule, autopilot, review window and monthly cap, which get_monthly_cap \
checks against the month's email spend. \
list_hot_leads and get_inbox_changes follow what comes back, and list_erased_leads the leads deleted since. \
find_lookalikes and add_lookalikes find companies like the user's customers. \
download_companies hands over a search's companies as CSV, with a link to the whole file. \
find_companies_by_domains looks up the companies behind a list of domains, like a CRM's accounts. \
suggest_sending_domains and check_sending_domains find domains to send cold email from, which the user buys on the Domains page. \
add_research_question and research_companies have the agent answer a question about every company, like \"Do they sell to hospitals?\", and \
list_research shows the answers. \
list_company_signals shows companies that just raised money or announced news. \
save_search keeps a search_companies search, and list_new_matches shows the companies that match it later. \
create_company_dedup_list hides domains, like the user's CRM accounts or a competitor's clients, from search_companies, download_companies, saved \
searches and find_lookalikes in the projects it covers. \
It never stops an email. \
find_email finds one person's email from their name and company domain with the project's email finders and verifier, and find_emails does up to 500 \
in the background for get_email_batch to follow. \
find_phone finds a person's mobile from their LinkedIn profile. \
Finding people and writing emails take minutes: they return a run_id, so poll get_run until it's done, and list_tasks finds a lost one. \
Approved emails go out from the user's own mailboxes on the project's schedule. \
Hot replies open deals on the pipeline, which list_deals, create_deal and move_deal follow to won or lost. \
assign_lead gives a lead to someone on the team. \
get_thread reads a reply thread, reply_to_lead answers it and mark_replied clears one answered elsewhere. \
get_analytics says which campaigns, mailboxes, follow-ups and send times get replies. \
For local businesses like dentists or gyms in a city, search_local_businesses and add_local_businesses use Google Maps once the user connected a \
key. \
For a lead list the user already has, import_campaign makes a campaign of exactly those leads. \
For a first email the user wrote themselves, update_my_copy (or update_project_my_copy for every campaign) sends it with each lead's details filled \
in, and preview_my_copy shows it for three leads.";

async fn not_allowed() -> StatusCode {
    StatusCode::METHOD_NOT_ALLOWED
}

async fn handle(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    // The token already stops a page from driving the server through someone's browser. This is the check the spec asks for anyway.
    if let Some(origin) = headers.get(header::ORIGIN)
        && Url::parse(&state.cfg.public_url).map(|u| u.origin().ascii_serialization()).ok().as_deref() != origin.to_str().ok()
    {
        return failure(StatusCode::FORBIDDEN, -32600, "This origin can't use the MCP server");
    }
    let auth = match authenticate(&state, &headers).await {
        Ok(auth) => auth,
        Err(res) => return res,
    };
    // The same per-organization limits as the public API, which most tools call into.
    let permit = match state.rate_limits.acquire(auth.org_id) {
        Ok(permit) => permit,
        Err(refused) => {
            let mut res = failure(StatusCode::TOO_MANY_REQUESTS, -32000, &refused.message);
            refused.stamp(res.headers_mut());
            return res;
        }
    };
    let mut res = answer(&state, &auth, &headers, &body).await;
    permit.quota.stamp(res.headers_mut());
    res
}

async fn answer(state: &AppState, auth: &ApiKeyAuth, headers: &HeaderMap, body: &Bytes) -> Response {
    let Ok(message) = serde_json::from_slice::<Value>(body) else {
        return failure(StatusCode::BAD_REQUEST, -32700, "The body isn't JSON");
    };
    if !message.is_object() {
        return failure(StatusCode::BAD_REQUEST, -32600, "Send one JSON-RPC message per request");
    }
    // A legacy client names its version only in the header, after initialize. Anything newer is served statelessly, which
    // answers a version Splee doesn't know with the list of ones it does.
    let header_version = headers.get("mcp-protocol-version").and_then(|v| v.to_str().ok());
    let asked = message["params"]["_meta"][VERSION_KEY].as_str().or(header_version);
    if message["method"] != "initialize" && asked.is_some_and(|v| !LEGACY.contains(&v)) {
        return stateless(state, auth, headers, &message).await;
    }
    // Notifications, like notifications/initialized, and responses need nothing back.
    let (Some(id), Some(method)) = (message.get("id"), message.get("method").and_then(Value::as_str)) else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
    let outcome = match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => call_tool(state, auth, &params).await,
        _ => Err((-32601, format!("Unknown method {method}"))),
    };
    Json(match outcome {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    })
    .into_response()
}

/// An API key, or an OAuth access token issued for this server. A 401 points the client at the protected resource metadata,
/// where it finds Splee's authorization server and signs the user in.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<ApiKeyAuth, Response> {
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let found = match token {
        Some(token) => match KeyAuth::find(&state.db, token).await {
            Ok(Some(KeyAuth { org_id, key_id })) => Ok(Some(ApiKeyAuth { key_id: Some(key_id), ..ApiKeyAuth::org(org_id) })),
            Ok(None) => oauth::token_org(&state.db, token, &oauth::resource(&state.cfg))
                .await
                .map(|found| found.map(|(org_id, user_id)| ApiKeyAuth { user_id: Some(user_id), ..ApiKeyAuth::org(org_id) })),
            Err(e) => Err(e),
        },
        None => Ok(None),
    };
    let metadata = oauth::resource_metadata_url(&state.cfg);
    let challenge = match (found, token) {
        (Ok(Some(auth)), _) => return Ok(auth),
        (Err(e), _) => return Err(AppError::from(e).into_response()),
        (Ok(None), None) => format!("Bearer resource_metadata=\"{metadata}\""),
        (Ok(None), Some(_)) => format!("Bearer error=\"invalid_token\", error_description=\"The token is wrong, expired or revoked\", resource_metadata=\"{metadata}\""),
    };
    let mut res = failure(StatusCode::UNAUTHORIZED, -32001, "Sign in to Splee, or send a Splee API key as Authorization: Bearer splee_…");
    res.headers_mut().insert(header::WWW_AUTHENTICATE, challenge.parse().expect("valid header"));
    Err(res)
}

fn rpc_error(status: StatusCode, id: &Value, code: i64, message: &str, data: Option<Value>) -> Response {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    (status, Json(json!({ "jsonrpc": "2.0", "id": id, "error": error }))).into_response()
}

/// A JSON-RPC error without an id, for requests refused before they're read.
fn failure(status: StatusCode, code: i64, message: &str) -> Response {
    rpc_error(status, &Value::Null, code, message, None)
}

fn server_info() -> Value {
    json!({ "name": "splee", "title": "Splee", "version": env!("CARGO_PKG_VERSION") })
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = LEGACY.iter().find(|v| **v == asked).unwrap_or(&LEGACY[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": server_info(),
        "instructions": INSTRUCTIONS,
    })
}

/// Mcp-Name carries a value that isn't plain ASCII as =?base64?…?=.
fn header_value(value: &str) -> Option<String> {
    match value.strip_prefix("=?base64?").and_then(|v| v.strip_suffix("?=")) {
        Some(encoded) => STANDARD.decode(encoded).ok().and_then(|b| String::from_utf8(b).ok()),
        None => Some(value.to_string()),
    }
}

/// The 2026-07-28 revision. The body is the source of truth, and the headers that mirror it have to agree with it.
async fn stateless(state: &AppState, auth: &ApiKeyAuth, headers: &HeaderMap, message: &Value) -> Response {
    let id = message.get("id").cloned().unwrap_or(Value::Null);
    let params = &message["params"];
    let meta = &params["_meta"];
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let refuse = |status: StatusCode, code: i64, text: &str| rpc_error(status, &id, code, text, None);
    let supported: Vec<&str> = std::iter::once(STATELESS).chain(LEGACY.iter().copied()).collect();
    let asked = meta[VERSION_KEY].as_str().or(header("mcp-protocol-version")).unwrap_or_default();
    if asked != STATELESS {
        let data = json!({ "supported": supported, "requested": asked });
        return rpc_error(StatusCode::BAD_REQUEST, &id, -32022, "Unsupported protocol version", Some(data));
    }
    // This revision has no notifications from the client, and clients never send responses.
    let Some(method) = message["method"].as_str().filter(|_| message.get("id").is_some()) else {
        return StatusCode::ACCEPTED.into_response();
    };
    if meta[VERSION_KEY].as_str().is_none() || !meta["io.modelcontextprotocol/clientCapabilities"].is_object() {
        let text = "Send io.modelcontextprotocol/protocolVersion and io.modelcontextprotocol/clientCapabilities in params._meta";
        return refuse(StatusCode::BAD_REQUEST, -32602, text);
    }
    let mismatch = if header("mcp-protocol-version") != Some(STATELESS) {
        Some("MCP-Protocol-Version")
    } else if header("mcp-method") != Some(method) {
        Some("Mcp-Method")
    } else if method == "tools/call" && header("mcp-name").and_then(header_value).as_deref() != params["name"].as_str() {
        Some("Mcp-Name")
    } else {
        None
    };
    if let Some(name) = mismatch {
        return refuse(StatusCode::BAD_REQUEST, -32020, &format!("Header mismatch: {name} is missing or doesn't match the body"));
    }
    let mut result = match method {
        "server/discover" => json!({
            "supportedVersions": supported,
            "capabilities": { "tools": {} },
            "instructions": INSTRUCTIONS,
            "ttlMs": CACHE_MS,
            "cacheScope": "private",
        }),
        "tools/list" => json!({ "tools": tools(), "ttlMs": CACHE_MS, "cacheScope": "private" }),
        "tools/call" => match call_tool(state, auth, params).await {
            Ok(result) => result,
            Err((code, text)) => return refuse(StatusCode::OK, code, &text),
        },
        _ => return refuse(StatusCode::NOT_FOUND, -32601, &format!("Unknown method {method}")),
    };
    result["resultType"] = json!("complete");
    result["_meta"] = json!({ "io.modelcontextprotocol/serverInfo": server_info() });
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

async fn call_tool(state: &AppState, auth: &ApiKeyAuth, params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = Some(&params["arguments"]).filter(|a| a.is_object()).cloned().unwrap_or_else(|| json!({}));
    let Some((text, is_error)) = call(state, auth.clone(), name, args).await else {
        return Err((-32602, format!("Unknown tool {name}")));
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

/// Runs a tool for the organization, as the text the model reads and whether it failed. None for a tool that doesn't exist.
/// Also how Ask Splee reads the pipeline (agent/ask.rs).
pub(crate) async fn call(state: &AppState, auth: ApiKeyAuth, name: &str, args: Value) -> Option<(String, bool)> {
    // A failed call is a result the model reads and can recover from, like a wrong id, not a protocol error.
    Some(match run(state, auth, name, args).await? {
        Ok(value) => (value.to_string(), false),
        Err(e) => {
            if let AppError::Internal(err) = &e {
                tracing::error!(error = ?err, tool = name, "mcp tool failed");
            }
            (e.public_message(), true)
        }
    })
}

/// The tools that only read, for Ask Splee. Local business search is left out because it spends the organization's Google
/// Maps calls, downloading companies because it spends the organization's downloads, and filling in company filters because
/// it's a Claude call of its own.
pub(crate) fn read_tools() -> Vec<Value> {
    let spends = ["search_local_businesses", "download_companies", "fill_company_filters"];
    tools().into_iter().filter(|t| t["annotations"]["readOnlyHint"] == json!(true) && !spends.iter().any(|s| t["name"] == *s)).collect()
}
