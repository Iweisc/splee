//! Splee as a remote MCP server, so ChatGPT, Claude, Cursor and other MCP clients can prospect from chat. It serves both eras of
//! https://modelcontextprotocol.io/specification/2026-07-28 over Streamable HTTP: the stateless 2026-07-28 revision, and the
//! initialize-based 2025-11-25 and 2025-06-18 ones. Every POST gets one JSON answer, with no SSE stream or session. A request
//! signs in with an API key or an access token from Splee's OAuth (routes/oauth.rs), and each tool calls the matching public
//! API v1 handler for that organization.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use axum::body::{Bytes, to_bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use super::api_v1 as v1;
use super::oauth;
use super::util_input::LEAD_STATUSES;
use crate::auth::{ApiKeyAuth, KeyAuth};
use crate::company_filters::{Claude, Model};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

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
back, and update_project_controls sets the project's sending, limits, schedule, autopilot, review window and monthly cap, which get_monthly_cap \
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

const EMAIL_BATCH_NEXT: &str = "This runs in the background, within the organization's 200 email searches an hour. Call get_email_batch with task_id until status is completed.";
const RUN_NEXT: &str = "This runs in the background and takes a few minutes. Call get_run with run_id until status is succeeded, failed or cancelled.";
const IMPORT_NEXT: &str = "The leads go in in the background, usually within seconds. Call get_campaign_import with task_id until status is \
completed or failed, then list_campaign_import_rows says what became of each lead.";
const MY_COPY_TEXT_RULES: &str = "Changes the user's own first email or turns it on or off. Pass only what changes. Variables: \
{{first_name}}, {{company}}, {{job_title}}, {{company_city}}, {{sender_first_name}}, {{sender_name}}, {{sender_company}}, each \
with an optional fallback like {{company_city|your city}}. A lead missing one with no fallback gets the AI email. {a|b} picks one \
option per lead. Up to 5 {{ai: instruction}} lines are written for each lead. The signature goes under the text unless it ends \
with the sender's name. A text with a mistake is refused with what to fix, and nothing is saved.";
const PROJECT_FIELDS: &[&str] = &["id", "name", "domain", "status", "created_at", "review_window_hours"];
const LEAD_FIELDS: &[&str] = &[
    "id", "full_name", "title", "email", "company_name", "company_domain", "status", "segment_id", "segment_name", "last_email_status", "score",
    "score_fit", "score_intent", "score_reasons", "owner_id", "owner_name", "owner_email", "language", "effective_language",
    "effective_language_source",
];
const COMPANY_FIELDS: &[&str] =
    &["id", "name", "domain", "segment_id", "country_code", "employees", "status", "score", "score_fit", "score_intent", "score_reasons", "hiring", "tech", "news"];
/// What a campaign's language can be set to.
static LANGUAGE_CHOICES: LazyLock<Vec<&str>> =
    LazyLock::new(|| crate::language::LANGUAGES.iter().map(|l| l.code).chain([crate::language::EACH_LEAD]).collect());
const RUN_FIELDS: &[&str] = &["id", "kind", "status", "message", "error", "cost_usd", "created_at", "finished_at"];
const DEAL_FIELDS: &[&str] = &[
    "id", "project_name", "lead_id", "lead_name", "company_name", "stage", "status", "value", "currency", "close_date", "owner_name", "lost_reason",
    "stage_changed_at",
];
const STAGE_FIELDS: &[&str] = &["name", "kind", "probability", "deals"];
const MEETING_FIELDS: &[&str] = &[
    "id", "provider", "title", "started_at", "duration_secs", "attendees", "summary", "next_steps", "provider_summary", "recording_url",
    "suggested_stage", "stage_reason", "stage_status", "follow_up_status",
];
const LOCAL_FIELDS: &[&str] =
    &["place_id", "name", "category", "address", "phone", "website", "domain", "rating", "review_count", "maps_url", "known"];
const SHEET_FIELDS: &[&str] =
    &["id", "kind", "spreadsheet_id", "spreadsheet_name", "tab", "segment_id", "keep_importing", "events", "last_sync_at", "rows_added", "error"];
const SIGNAL_FIELDS: &[&str] =
    &["domain", "company_id", "company_name", "leads", "kind", "happened_on", "headline", "title", "amount", "offered", "industry", "url", "checked_at"];
const RESEARCH_QUESTION_FIELDS: &[&str] = &["id", "label", "question", "answer_type", "use_in_emails"];
const RESEARCH_ANSWER_FIELDS: &[&str] = &["status", "answer", "evidence", "source_url"];
/// Rows download_companies puts in its answer. The rest are a download_url away.
const MAX_DOWNLOAD_ROWS: i64 = 200;
/// Signals one list_company_signals call returns at most, so a big project's list doesn't fill the client's context.
const MAX_SIGNALS: usize = 100;
const JOB_CHANGE_FIELDS: &[&str] = &[
    "id", "full_name", "linkedin_url", "relationship", "old_company", "new_company", "new_domain", "new_title", "new_lead_id", "new_lead_email",
    "segment_name", "seen_at",
];
const LOOKALIKE_FIELDS: &[&str] = &["domain", "name", "country_code", "region", "category", "score", "reasons"];
const LOOKALIKE_SEED_FIELDS: &[&str] = &["domain", "name", "won", "in_database", "known"];
const LOOKALIKE_PART_FIELDS: &[&str] = &["key", "label", "on", "known", "values"];
const CALL_FIELDS: &[&str] = &[
    "id", "lead_id", "lead_name", "lead_company", "lead_phone", "status", "bucket", "due_at", "instructions", "owner_name", "local_time",
    "outside_hours", "outcome", "notes", "duration_secs", "summary", "next_step", "cancel_reason",
];

/// None for a tool that doesn't exist.
async fn run(state: &AppState, auth: ApiKeyAuth, name: &str, mut args: Value) -> Option<AppResult<Value>> {
    let st = || State(state.clone());
    // Pages a model reads in full, so they're shorter than the API's default of 50.
    if let Some(args) = args.as_object_mut() {
        args.entry("per_page").or_insert(json!(20));
        if let Some(campaign) = args.remove("campaign_id") {
            args.insert("segment_id".into(), campaign);
        }
    }
    let a = &args;
    Some(match name {
        "list_projects" => async { Ok(pick(value(v1::list_projects(st(), auth).await?)?, PROJECT_FIELDS)) }.await,
        "get_project" => async { value(v1::get_project(st(), auth, id(a, "project_id")?).await?) }.await,
        "create_project" => async {
            let created = value(v1::create_project(st(), auth, Json(parse(a)?)).await?)?;
            Ok(json!({ "project_id": created["project_id"], "run_id": created["run_id"], "next": RUN_NEXT }))
        }
        .await,
        "list_campaigns" => async { value(v1::list_campaigns(st(), auth, id(a, "project_id")?, Query(parse(a)?)).await?) }.await,
        "create_campaign" => async {
            let created = value(v1::create_campaign(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?)?;
            Ok(json!({ "campaign_id": created["campaign_id"], "run_id": created["run_id"], "next": RUN_NEXT }))
        }
        .await,
        "import_campaign" => async {
            let started = value(v1::import_campaign(st(), auth, Json(parse(a)?)).await?)?;
            Ok(json!({ "task_id": started["task_id"], "campaign_id": started["campaign_id"], "next": IMPORT_NEXT }))
        }
        .await,
        "get_campaign_import" => async { value(v1::campaign_import(st(), auth, id(a, "task_id")?).await?) }.await,
        "list_campaign_import_rows" => async { value(v1::campaign_import_rows(st(), auth, id(a, "task_id")?, Query(parse(a)?)).await?) }.await,
        "search_companies" => async { value(v1::search_companies(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "find_companies_by_domains" => async { value(v1::companies_by_domains(st(), auth, Json(parse(a)?)).await?) }.await,
        "fill_company_filters" => async {
            let model: Arc<dyn Model> = Arc::new(Claude);
            value(v1::companies_filters(st(), auth, Extension(model), Json(parse(a)?)).await?)
        }
        .await,
        "add_companies" => async { value(v1::add_companies(st(), auth, id(a, "segment_id")?, Json(parse(a)?)).await?) }.await,
        "list_company_dedup_lists" => async { value(v1::list_dedup_lists(st(), auth).await?) }.await,
        "create_company_dedup_list" => async { value(v1::add_to_dedup_list(st(), auth, Json(parse(a)?)).await?) }.await,
        "get_company_dedup_list" => async { value(v1::get_dedup_list(st(), auth, id(a, "list_id")?, Query(parse(a)?)).await?) }.await,
        "set_company_dedup_list_projects" => async { value(v1::rescope_dedup_list(st(), auth, id(a, "list_id")?, Json(parse(a)?)).await?) }.await,
        "delete_company_dedup_list" => async { value(v1::delete_dedup_list(st(), auth, id(a, "list_id")?).await?) }.await,
        "list_saved_searches" => async { value(v1::list_saved_searches(st(), auth, id(a, "project_id")?).await?) }.await,
        "save_search" => async { value(v1::save_search(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?) }.await,
        "delete_saved_search" => async { value(v1::delete_saved_search(st(), auth, id(a, "search_id")?).await?) }.await,
        "list_new_matches" => async { value(v1::new_matches(st(), auth, id(a, "search_id")?, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "find_email" => async { value(v1::find_email(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?) }.await,
        "find_phone" => async { value(v1::find_phone(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?) }.await,
        "find_emails" => async {
            let started = value(v1::find_emails(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?)?;
            Ok(json!({ "task_id": started["task_id"], "total": started["total"], "next": EMAIL_BATCH_NEXT }))
        }
        .await,
        "get_email_batch" => async { value(v1::email_batch(st(), auth, id(a, "task_id")?).await?) }.await,
        "list_sending_domains" => async { value(v1::list_sending_domains(st(), auth).await?) }.await,
        "suggest_sending_domains" => async { value(v1::suggest_sending_domains(st(), auth, Query(parse(a)?)).await?) }.await,
        "check_sending_domains" => async { value(v1::check_sending_domains(st(), auth, Query(parse(&comma_joined(a, "domains"))?)).await?) }.await,
        "download_companies" => async {
            let mut a = comma_joined(a, "domains");
            a["per_page"] = json!(a["per_page"].as_i64().unwrap_or(20).clamp(1, MAX_DOWNLOAD_ROWS));
            a["format"] = json!("csv");
            a.as_object_mut().expect("an object").remove("page");
            let res = v1::export_companies(st(), auth, Query(parse(&a)?), Query(parse(&a)?)).await?;
            downloaded(state, res, &a).await
        }
        .await,
        "find_lookalikes" => async {
            let a = comma_joined(a, "seeds");
            let limit = a["limit"].as_u64().unwrap_or(25).clamp(1, crate::lookalikes::LIMIT as u64) as usize;
            let found = value(v1::list_lookalikes(st(), auth, id(&a, "project_id")?, Query(parse(&a)?)).await?)?;
            let profile = &found["profile"];
            let results: Vec<Value> = found["results"].as_array().into_iter().flatten().take(limit).cloned().collect();
            Ok(json!({
                "seeds": pick(profile["seeds"].clone(), LOOKALIKE_SEED_FIELDS),
                "profile": pick(profile["parts"].clone(), LOOKALIKE_PART_FIELDS),
                "results": pick(Value::Array(results), LOOKALIKE_FIELDS),
                "total": found["results"].as_array().map_or(0, Vec::len),
                "database_empty": found["database_empty"],
            }))
        }
        .await,
        "add_lookalikes" => async {
            let mut a = a.clone();
            if let Some(seeds) = a["seeds"].as_str() {
                a["seeds"] = json!(seeds.split(',').collect::<Vec<_>>());
            }
            value(v1::add_lookalikes(st(), auth, id(&a, "project_id")?, Json(parse(&a)?)).await?)
        }
        .await,
        "search_local_businesses" => async { Ok(pick(value(v1::search_local_businesses(st(), auth, Query(parse(a)?)).await?)?, LOCAL_FIELDS)) }.await,
        "add_local_businesses" => async { value(v1::add_local_businesses(st(), auth, id(a, "segment_id")?, Json(parse(a)?)).await?) }.await,
        "find_people" => async { started(value(v1::find_people(st(), auth, id(a, "segment_id")?, Some(Json(parse(a)?))).await?)) }.await,
        "write_emails" => async { started(value(v1::write_emails(st(), auth, id(a, "segment_id")?, Some(Json(parse(a)?))).await?)) }.await,
        "get_run" => async { Ok(pick(value(v1::get_run(st(), auth, id(a, "run_id")?).await?)?, RUN_FIELDS)) }.await,
        "list_tasks" => async { value(v1::list_tasks(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "list_leads" => async {
            let page = value(v1::list_leads(st(), auth, id(a, "project_id")?, Query(parse(a)?), Query(parse(a)?)).await?)?;
            Ok(pick(page, LEAD_FIELDS))
        }
        .await,
        "list_companies" => async {
            let page = value(v1::list_companies(st(), auth, id(a, "project_id")?, Query(parse(a)?), Query(parse(a)?)).await?)?;
            Ok(pick(page, COMPANY_FIELDS))
        }
        .await,
        "list_emails_to_approve" => async {
            let mut a = a.clone();
            a["status"] = json!("draft");
            value(v1::list_emails(st(), auth, Query(parse(&a)?), Query(parse(&a)?)).await?)
        }
        .await,
        "approve_email" => async { value(v1::approve_email(st(), auth, id(a, "email_id")?).await?) }.await,
        "skip_email" => async { value(v1::skip_email(st(), auth, id(a, "email_id")?).await?) }.await,
        "list_replies" => async { value(v1::list_inbox(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "get_thread" => async { value(v1::lead_thread(st(), auth, id(a, "lead_id")?).await?) }.await,
        "reply_to_lead" => async { value(v1::reply_to_lead(st(), auth, id(a, "lead_id")?, Json(parse(a)?)).await?) }.await,
        "mark_replied" => async { value(v1::mark_replied(st(), auth, id(a, "lead_id")?).await?) }.await,
        "get_analytics" => async { value(v1::project_analytics(st(), auth, id(a, "project_id")?, Query(parse(a)?)).await?) }.await,
        "list_deals" => async {
            let stages = value(v1::list_stages(st(), auth.clone()).await?)?;
            let page = value(v1::list_deals(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?)?;
            Ok(json!({ "stages": pick(stages, STAGE_FIELDS), "items": pick(page["items"].clone(), DEAL_FIELDS), "total": page["total"] }))
        }
        .await,
        "create_deal" => async { Ok(pick(value(v1::create_deal(st(), auth, Json(parse(a)?)).await?)?, DEAL_FIELDS)) }.await,
        "move_deal" => async { Ok(pick(value(v1::move_deal(st(), auth, id(a, "deal_id")?, Json(parse(a)?)).await?)?, DEAL_FIELDS)) }.await,
        "set_lead_language" => async {
            let body = json!({ "language": a["language"].as_str().unwrap_or_default() });
            let detail = value(v1::update_lead(st(), auth, id(a, "lead_id")?, Json(parse(&body)?)).await?)?;
            Ok(json!({ "id": detail["id"], "full_name": detail["full_name"], "language": detail["language"],
                       "effective_language": detail["effective_language"], "effective_language_source": detail["effective_language_source"] }))
        }
        .await,
        "set_campaign_language" => async {
            value(v1::update_campaign(st(), auth, id(a, "segment_id")?, Json(json!({ "language": a["language"] }))).await?)
        }
        .await,
        "get_campaign" => async { value(v1::get_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "get_campaign_send_state" => async { value(v1::campaign_send_state(st(), auth, id(a, "segment_id")?).await?) }.await,
        "get_my_copy" => async { value(v1::campaign_my_copy(st(), auth, id(a, "segment_id")?).await?) }.await,
        "update_my_copy" => async { value(v1::update_campaign_my_copy(st(), auth, id(a, "segment_id")?, Json(my_copy_body(a))).await?) }.await,
        "set_my_copy_source" => async {
            value(v1::set_my_copy_source(st(), auth, id(a, "segment_id")?, Json(json!({ "source": a["source"] }))).await?)
        }
        .await,
        "preview_my_copy" => async { value(v1::preview_my_copy(st(), auth, id(a, "segment_id")?).await?) }.await,
        "get_project_my_copy" => async { value(v1::project_my_copy(st(), auth, id(a, "project_id")?).await?) }.await,
        "update_project_my_copy" => async {
            value(v1::update_project_my_copy(st(), auth, id(a, "project_id")?, Json(my_copy_body(a))).await?)
        }
        .await,
        "start_campaign" => async { value(v1::start_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "stop_campaign" => async { value(v1::stop_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "approve_campaign" => async { value(v1::approve_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "archive_campaign" => async { value(v1::archive_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "unarchive_campaign" => async { value(v1::unarchive_campaign(st(), auth, id(a, "segment_id")?).await?) }.await,
        "update_project_controls" => async {
            value(v1::update_project(st(), auth, id(a, "project_id")?, Json(patch_body(a, "project_id"))).await?)
        }
        .await,
        "get_monthly_cap" => async { value(v1::project_monthly_cap(st(), auth, id(a, "project_id")?).await?) }.await,
        "update_campaign" => async { value(v1::update_campaign(st(), auth, id(a, "segment_id")?, Json(patch_body(a, "segment_id"))).await?) }.await,
        "list_hot_leads" => async { value(v1::hot_leads(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "get_inbox_changes" => async { value(v1::inbox_changes(st(), auth, id(a, "project_id")?, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "list_erased_leads" => async { value(v1::erased_leads(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "assign_lead" => async {
            let detail = value(v1::assign_lead(st(), auth, id(a, "lead_id")?, Json(parse(a)?)).await?)?;
            Ok(json!({ "id": detail["id"], "full_name": detail["full_name"], "owner_id": detail["owner_id"], "owner_email": detail["owner_email"] }))
        }
        .await,
        "list_call_tasks" => async { Ok(pick(value(v1::list_calls(st(), auth, id(a, "project_id")?, Query(parse(a)?)).await?)?, CALL_FIELDS)) }.await,
        "log_call" => async { Ok(pick(value(v1::log_call(st(), auth, id(a, "lead_id")?, Json(parse(a)?)).await?)?, CALL_FIELDS)) }.await,
        "list_meetings" => async { Ok(pick(value(v1::list_meetings(st(), auth, id(a, "lead_id")?).await?)?, MEETING_FIELDS)) }.await,
        "list_sheet_connections" => async { Ok(pick(value(v1::list_sheets(st(), auth, id(a, "project_id")?).await?)?, SHEET_FIELDS)) }.await,
        "import_from_sheet" => async {
            let Path(project) = id(a, "project_id")?;
            let Path(conn) = id(a, "connection_id")?;
            let done = value(v1::import_sheet(st(), auth, Path((project, conn))).await?)?;
            let import = &done["import"];
            Ok(json!({ "imported": import["imported"].as_i64().unwrap_or(0), "skipped": import["skipped"].as_i64().unwrap_or(0), "errors": import["errors"],
                       "connection": pick(done["connection"].clone(), SHEET_FIELDS) }))
        }
        .await,
        "list_job_changes" => async {
            let overview = value(v1::list_job_changes(st(), auth, id(a, "project_id")?).await?)?;
            Ok(json!({ "connected": overview["connected"], "watching": overview["watching"], "last_checked_at": overview["last_checked_at"],
                       "items": pick(overview["changes"].clone(), JOB_CHANGE_FIELDS) }))
        }
        .await,
        "add_to_watch_list" => async { value(v1::add_to_watch_list(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?) }.await,
        "list_research" => async {
            let grid = value(v1::list_research(st(), auth, id(a, "project_id")?, Query(parse(a)?), Query(parse(a)?)).await?)?;
            // Each answer carries its question's label, so a client reads a company's answers without matching ids. Keyed by
            // question id, since two questions can share a label.
            let labels: HashMap<String, Value> =
                grid["questions"].as_array().into_iter().flatten().map(|q| (q["id"].as_str().unwrap_or_default().to_string(), q["label"].clone())).collect();
            let items: Vec<Value> = grid["items"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|c| {
                    let answers: serde_json::Map<String, Value> = c["answers"]
                        .as_object()
                        .into_iter()
                        .flatten()
                        .map(|(qid, a)| {
                            let mut answer = pick(a.clone(), RESEARCH_ANSWER_FIELDS);
                            answer["label"] = labels.get(qid).cloned().unwrap_or(Value::Null);
                            (qid.clone(), answer)
                        })
                        .collect();
                    json!({ "id": c["id"], "domain": c["domain"], "name": c["name"], "score": c["score"], "answers": answers })
                })
                .collect();
            Ok(json!({
                "questions": pick(grid["questions"].clone(), RESEARCH_QUESTION_FIELDS),
                "items": items,
                "total": grid["total"],
                "researching": grid["researching"],
            }))
        }
        .await,
        "add_research_question" => async { Ok(pick(value(v1::add_research_question(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?)?, RESEARCH_QUESTION_FIELDS)) }.await,
        "research_companies" => async { value(v1::start_research(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?) }.await,
        "list_company_signals" => async {
            let limit = a["limit"].as_u64().unwrap_or(25).clamp(1, MAX_SIGNALS as u64) as usize;
            let mut listed = value(v1::list_signals(st(), auth, id(a, "project_id")?, Query(parse(a)?)).await?)?;
            if let Some(items) = listed.as_array_mut() {
                items.truncate(limit);
            }
            Ok(pick(listed, SIGNAL_FIELDS))
        }
        .await,
        _ => return None,
    })
}

fn value<T: Serialize>(Json(body): Json<T>) -> AppResult<Value> {
    serde_json::to_value(body).map_err(|e| AppError::Internal(e.into()))
}

/// The PATCH body of the my-copy routes: the arguments it takes that were passed.
fn my_copy_body(args: &Value) -> Value {
    let body: serde_json::Map<String, Value> =
        ["mode", "subject", "body"].iter().filter_map(|k| args.get(*k).filter(|v| !v.is_null()).map(|v| (k.to_string(), v.clone()))).collect();
    Value::Object(body)
}

fn parse<T: DeserializeOwned>(args: &Value) -> AppResult<T> {
    serde_json::from_value(args.clone()).map_err(|e| AppError::BadRequest(format!("Invalid arguments: {e}")))
}

/// The arguments besides the id, for a PATCH that refuses fields it doesn't know.
fn patch_body(args: &Value, id: &str) -> Value {
    let mut body = args.clone();
    if let Some(body) = body.as_object_mut() {
        body.remove(id);
        body.remove("per_page");
    }
    body
}

/// The arguments with a list under `key` as the comma-separated value the API's query string takes.
fn comma_joined(args: &Value, key: &str) -> Value {
    let mut args = args.clone();
    if let Some(items) = args[key].as_array() {
        args[key] = json!(items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(","));
    }
    args
}

fn id(args: &Value, key: &str) -> AppResult<Path<Uuid>> {
    // Tools name campaigns campaign_id, which `run` renamed to the API's segment_id.
    let shown = if key == "segment_id" { "campaign_id" } else { key };
    args[key].as_str().and_then(|s| s.trim().parse().ok()).map(Path).ok_or_else(|| AppError::BadRequest(format!("Pass {shown} as an id")))
}

/// The first rows as CSV for the model, and the API link that downloads the whole search with an API key.
async fn downloaded(state: &AppState, res: Response, args: &Value) -> AppResult<Value> {
    // A Response isn't Sync, so what reads its headers can't live across the await below.
    let (rows, more, status) = {
        let header = |name: &str| res.headers().get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        (header("splee-rows").parse::<i64>().unwrap_or(0), header("splee-capped") == "true", res.status())
    };
    let body = to_bytes(res.into_body(), 16 << 20).await.map_err(|e| AppError::Internal(e.into()))?;
    let text = String::from_utf8_lossy(&body).into_owned();
    // The download answers its rate limit as a response rather than an error, for the API's Retry-After.
    if !status.is_success() {
        let message = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["error"].as_str().map(String::from)).unwrap_or(text);
        return Err(AppError::TooManyRequests(message));
    }
    let base = format!("{}/api/v1/companies/export", state.cfg.public_url.trim_end_matches('/'));
    let mut url = Url::parse(&base).map_err(|e| AppError::Internal(e.into()))?;
    for key in ["q", "country", "city", "category", "has_email", "has_phone", "tech", "project_id", "include_hidden", "domains"] {
        let value = match &args[key] {
            Value::String(v) if !v.trim().is_empty() => v.as_str(),
            Value::Bool(true) => "true",
            _ => continue,
        };
        url.query_pairs_mut().append_pair(key, value);
    }
    Ok(json!({
        "rows": rows,
        "more": more,
        "csv": text,
        "download_url": url.as_str(),
        "next": "GET download_url with a Splee API key as Authorization: Bearer for every match, up to 50,000 rows a page (add page=2 for the next), \
                 or add format=jsonl for JSON lines. Each download counts against the organization's 20 an hour.",
    }))
}

fn started(created: AppResult<Value>) -> AppResult<Value> {
    Ok(json!({ "run_id": created?["run_id"], "next": RUN_NEXT }))
}

/// Keeps only `fields` of an object, of each object in a list, or of each item in a page.
fn pick(value: Value, fields: &[&str]) -> Value {
    match value {
        Value::Array(items) => Value::Array(items.into_iter().map(|v| pick(v, fields)).collect()),
        Value::Object(mut map) if map.contains_key("items") => {
            map["items"] = pick(map["items"].take(), fields);
            Value::Object(map)
        }
        Value::Object(map) => Value::Object(map.into_iter().filter(|(k, _)| fields.contains(&k.as_str())).collect()),
        other => other,
    }
}

fn id_prop(description: &str) -> Value {
    json!({ "type": "string", "format": "uuid", "description": description })
}

fn page_props(props: Value) -> Value {
    let mut props = props;
    props["page"] = json!({ "type": "integer", "minimum": 1, "description": "Page number, from 1" });
    props["per_page"] = json!({ "type": "integer", "minimum": 1, "maximum": 200, "description": "Results per page, 20 by default" });
    props
}

fn score_prop() -> Value {
    json!({ "type": "integer", "minimum": 0, "maximum": 100, "description": "Optional. Only scores of at least this" })
}

fn sort_prop() -> Value {
    json!({ "type": "string", "enum": ["score", "newest"], "description": "Optional. score puts the highest first" })
}

fn tool(name: &str, title: &str, description: &str, properties: Value, required: &[&str], annotations: Value) -> Value {
    json!({
        "name": name,
        "title": title,
        "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required },
        "annotations": annotations,
    })
}

fn tools() -> Vec<Value> {
    let read = json!({ "readOnlyHint": true });
    let adds = json!({ "readOnlyHint": false, "destructiveHint": false });
    let control = json!({ "readOnlyHint": false, "destructiveHint": false, "idempotentHint": true });
    let texts = json!({ "type": "array", "items": { "type": "string" } });
    let project = id_prop("A project id from list_projects");
    let campaign = id_prop("A campaign id from list_campaigns or create_campaign");
    vec![
        tool("list_projects", "List projects", "The organization's projects. A project is one business doing outreach.", json!({}), &[], read.clone()),
        tool(
            "get_project",
            "Get project",
            "A project with what Splee learned about the business (company) and its controls, as update_project_controls returns them.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "create_project",
            "Create project",
            "Creates a project for a business from its website or a description, as the app's New project does. An agent researches \
             the business, which takes a few minutes, then create_campaign can start. Returns project_id and run_id.",
            json!({
                "domain": { "type": "string", "description": "The business's website, like acme.com" },
                "description": { "type": "string", "description": "What the business sells and to whom, when there's no website" },
            }),
            &[],
            adds.clone(),
        ),
        tool(
            "list_campaigns",
            "List campaigns",
            "A project's campaigns with their funnel: companies, leads, emails sent, leads contacted, replies, bounces, hot leads and \
             spend in micro-dollars. A campaign that A/B tests its email angle has ab_test: each variant's instructions and results, \
             whether a winner is clear yet in note, and the winner auto-pick kept, if any. Archived campaigns are left out unless \
             archived is true, and have archived_at set.",
            json!({
                "project_id": project,
                "archived": { "type": "boolean", "description": "Optional. True to list archived campaigns too" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "create_campaign",
            "Create campaign",
            "Creates a campaign from a description of who to reach, like \"dental clinics in Leeds\". An agent names it and sets its \
             targeting, which takes a minute or two. Returns campaign_id and run_id.",
            json!({
                "project_id": project,
                "prompt": { "type": "string", "description": "Who the campaign targets, in plain words" },
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "Optional. lead (the default) for each lead's own language, en or another language code" },
            }),
            &["project_id", "prompt"],
            adds.clone(),
        ),
        tool(
            "import_campaign",
            "Import campaign from a lead list",
            "Makes a new draft campaign holding exactly the leads given, for a list the user already has. Each lead needs email, \
             first_name, last_name, company_domain and job_title, and one without them is skipped. Leads already in the project, \
             repeats and do-not-contact entries are deduped. Returns task_id. Then write_emails writes its emails and \
             start_campaign sends them.",
            json!({
                "project_id": project,
                "name": { "type": "string", "description": "The campaign's name" },
                "leads": {
                    "type": "array",
                    "maxItems": crate::campaign_imports::MAX_LEADS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "email": { "type": "string" },
                            "first_name": { "type": "string" },
                            "last_name": { "type": "string" },
                            "company_domain": { "type": "string" },
                            "job_title": { "type": "string" },
                            "linkedin_url": { "type": "string", "description": "Optional" },
                            "client_ref": { "type": "string", "description": "Optional. The user's own id for the lead, handed back in its row" },
                        },
                        "required": ["email", "first_name", "last_name", "company_domain", "job_title"],
                    },
                },
                "instructions": { "type": "string", "description": "Optional. The campaign's outreach angle, which its first emails follow" },
                "language": { "type": "string", "description": "Optional. lead (the default) for each lead's own language, or a language code like de" },
            }),
            &["project_id", "name", "leads"],
            adds.clone(),
        ),
        tool(
            "get_campaign_import",
            "Get campaign import status",
            "An import_campaign task: status pending, completed or failed, and once completed, result with campaign_id and how \
             many leads were imported, skipped and deduped.",
            json!({ "task_id": id_prop("A task_id from import_campaign") }),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "list_campaign_import_rows",
            "List campaign import rows",
            "Each lead an import_campaign task was given, in order: status imported with person_id, the lead's id, or skipped or \
             deduped with a reason code like missing_job_title, already_contacted or do_not_contact.",
            page_props(json!({ "task_id": id_prop("A task_id from import_campaign") })),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "search_companies",
            "Search company database",
            "Searches the company database (businesses with a website from Overture Maps), the most reliable listings first. q matches \
             words at the start of words in the name or category. Each company's tech lists the tools its website and DNS show it \
             uses, once Splee has scanned it, which it does for every project's companies. Companies on the organization's dedup \
             lists are left out: the lists for every project, and with project_id that project's too. total stops counting at \
             10,000, and capped says there are more.",
            page_props(json!({
                "q": { "type": "string", "description": "Words in the name or category, like \"dental\"" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string", "description": "City name, like Leeds" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean", "description": "Only companies that list an email" },
                "has_phone": { "type": "boolean", "description": "Only companies that list a phone" },
                "tech": { "type": "string", "description": "Only companies that use this tool, like HubSpot, or a tool of this category: crm, \
                    marketing, email_sending, support, analytics, sales_intelligence, advertising, ecommerce, cms, payments, hosting, \
                    scheduling, email or workplace" },
                "project_id": id_prop("Optional. The project searching, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to show the companies on dedup lists too" },
            })),
            &[],
            read.clone(),
        ),
        tool(
            "find_companies_by_domains",
            "Find companies by domain",
            "Looks up up to 1,000 domains in the company database, like a CRM's accounts or a spreadsheet's column. Domains are cleaned \
             up first, so https://www.acme.com/about finds acme.com. items has one company per domain found, in the order given, and \
             not_found the rest, including those the organization's dedup lists hide unless include_hidden is true.",
            json!({
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains or website addresses, up to 1,000" },
                "project_id": id_prop("Optional. The project looking, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to find the companies on dedup lists too" },
            }),
            &["domains"],
            read.clone(),
        ),
        tool(
            "list_sending_domains",
            "List sending domains",
            "The organization's own sending domains, like getacme.com, bought on the Domains page: status, the mailbox provider, \
             whether its DNS is set, and how many mailboxes use and warm it.",
            json!({}),
            &[],
            read.clone(),
        ),
        tool(
            "suggest_sending_domains",
            "Suggest sending domains",
            "Names to send cold email from apart from the company's own domain, like getacme.com or acmehq.co, each with whether the \
             organization's Porkbun account can buy it and its price in micro-dollars, buyable first. available is null when the \
             registry didn't answer in time. Needs Porkbun connected on the Domains page, where the user buys them.",
            json!({ "domain": { "type": "string", "description": "The company's own domain, like acme.com" } }),
            &["domain"],
            read.clone(),
        ),
        tool(
            "check_sending_domains",
            "Check sending domains",
            "Whether the organization's Porkbun account can buy each of up to 20 names and at what price, buyable first. Needs \
             Porkbun connected on the Domains page, where the user buys them.",
            json!({ "domains": { "type": "array", "items": { "type": "string" }, "maxItems": 20, "description": "Names like getacme.com" } }),
            &["domains"],
            read.clone(),
        ),
        tool(
            "fill_company_filters",
            "Fill in company search filters",
            "Turns a description like \"dental clinics in Leeds that list an email and use Shopify\" into search_companies filters: \
             q, country, city, category, has_email, has_phone and tech, using only categories and countries the database has and \
             tools Splee detects. note says what it couldn't turn into a filter, like company size. Pass filters to search_companies \
             as they are. 30 an hour per organization.",
            json!({ "query": { "type": "string", "description": "What companies to find, in a sentence, up to 500 characters" } }),
            &["query"],
            read.clone(),
        ),
        tool(
            "add_companies",
            "Add companies to campaign",
            "Adds companies from search_companies to a campaign by domain, up to 500 at a time. Companies the project already has and \
             ones on the do-not-contact list are skipped. Then call find_people.",
            json!({
                "campaign_id": campaign,
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains from search_companies" },
            }),
            &["campaign_id", "domains"],
            adds.clone(),
        ),
        tool(
            "list_saved_searches",
            "List saved company searches",
            "A project's saved company database searches with their filters. Splee checks each about hourly for companies that \
             match and didn't before. new_matches counts the ones found since the search was last opened in the app, and \
             too_broad says it matched over 10,000 companies, so it isn't checked.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "save_search",
            "Save company search",
            "Saves search_companies filters under a name, so Splee checks them about hourly and reports companies that match \
             later, like ones from a country loaded into the database. What matches now isn't reported. With chat_alerts, the \
             project's chat hears about new matches. With campaign_id, up to 200 new matches a check are added to that campaign \
             the way add_companies adds them. Up to 50 a project.",
            json!({
                "project_id": project,
                "name": { "type": "string", "description": "Up to 120 characters, like \"Dentists in Leeds\"" },
                "q": { "type": "string", "description": "Words in the name or category" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean" },
                "has_phone": { "type": "boolean" },
                "tech": { "type": "string", "description": "A tool or tool category, as search_companies takes it" },
                "chat_alerts": { "type": "boolean", "description": "Post new matches to the project's chat" },
                "campaign_id": id_prop("Optional. A campaign of the project to add new matches to"),
            }),
            &["project_id", "name"],
            adds.clone(),
        ),
        tool(
            "delete_saved_search",
            "Delete saved company search",
            "Deletes a saved search. Splee stops checking it, and companies it added to a campaign stay there.",
            json!({ "search_id": id_prop("A search id from list_saved_searches") }),
            &["search_id"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true }),
        ),
        tool(
            "list_company_dedup_lists",
            "List company dedup lists",
            "The organization's dedup lists, newest first, each with its size in domains and project_ids, null when it covers \
             every project.",
            json!({}),
            &[],
            read.clone(),
        ),
        tool(
            "create_company_dedup_list",
            "Add companies to a dedup list",
            "Adds up to 10,000 domains or website addresses to the organization's dedup list of that name, making it when there's \
             none. Searches in the projects it covers leave those companies out: search_companies, download_companies, saved \
             searches and find_lookalikes. It only hides search results and never stops an email or adding a company to a campaign. \
             skipped counts invalid domains and ones already on it.",
            json!({
                "name": { "type": "string", "description": "The list's name, like \"CRM accounts\"" },
                "domains": { "type": "array", "items": { "type": "string" }, "maxItems": 10_000, "description": "Domains like acme.com" },
                "project_ids": {
                    "type": ["array", "null"],
                    "items": { "type": "string", "format": "uuid" },
                    "description": "Optional. The projects it covers, null for all. Left out, a new list covers all and an existing one keeps its own",
                },
            }),
            &["name", "domains"],
            adds.clone(),
        ),
        tool(
            "get_company_dedup_list",
            "Get a company dedup list",
            "A dedup list with its domains in alphabetical order, 100 at a time unless limit says up to 1,000.",
            json!({
                "list_id": id_prop("A list id from list_company_dedup_lists"),
                "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Optional. Domains to return, 100 by default" },
                "offset": { "type": "integer", "minimum": 0, "description": "Optional. Domains to skip" },
            }),
            &["list_id"],
            read.clone(),
        ),
        tool(
            "set_company_dedup_list_projects",
            "Set a dedup list's projects",
            "Changes the projects whose searches a dedup list hides companies from.",
            json!({
                "list_id": id_prop("A list id from list_company_dedup_lists"),
                "project_ids": {
                    "type": ["array", "null"],
                    "items": { "type": "string", "format": "uuid" },
                    "description": "Project ids, or null for all",
                },
            }),
            &["list_id", "project_ids"],
            control.clone(),
        ),
        tool(
            "delete_company_dedup_list",
            "Delete a company dedup list",
            "Deletes a dedup list and its domains. Its companies show up in searches again right away.",
            json!({ "list_id": id_prop("A list id from list_company_dedup_lists") }),
            &["list_id"],
            json!({ "readOnlyHint": false, "destructiveHint": true, "idempotentHint": true }),
        ),
        tool(
            "list_new_matches",
            "List new matches of a saved search",
            "The companies a saved search found after it was saved, newest first, each with found_at. Without since, the ones \
             found since it was last opened in the app. Reading them here doesn't change that.",
            page_props(json!({
                "search_id": id_prop("A search id from list_saved_searches"),
                "since": { "type": "string", "format": "date-time", "description": "Optional. Only companies found at or after this" },
            })),
            &["search_id"],
            read.clone(),
        ),
        tool(
            "find_email",
            "Find a person's email",
            "Finds a person's work email from their name and company domain with the project's email finders and verifier, \
             like Splee does for a lead without one, and doesn't add them as a lead. status is found, guessed (a common pattern \
             the verifier accepted), catch_all, not_found, suppressed (on the do-not-contact list), duplicate (a lead in the \
             project has it, named in note) or failed, and email_status verified, or found when no verifier confirmed it. phone \
             is filled only when the project finds phone numbers too. Each call is one of the organization's 200 email searches \
             an hour.",
            json!({
                "project_id": project,
                "first_name": { "type": "string" },
                "last_name": { "type": "string" },
                "company_domain": { "type": "string", "description": "The company's domain or website, like acme.com" },
                "linkedin_url": { "type": "string", "description": "Optional. Their LinkedIn profile, which some finders match on" },
            }),
            &["project_id", "first_name", "company_domain"],
            adds.clone(),
        ),
        tool(
            "find_phone",
            "Find a person's mobile",
            "Finds the mobile number of the person behind a LinkedIn profile with the project's email finders that return \
             mobiles (People Data Labs, RocketReach, Lusha and Prospeo, in the project's order), and doesn't add them as a \
             lead. status is found, with phone in E.164 and source the finder, not_found, suppressed (on the do-not-contact \
             list) or failed, and note says why when nothing was found. Each call is one of the organization's 200 email \
             searches an hour.",
            json!({
                "project_id": project,
                "linkedin_url": { "type": "string", "description": "Their LinkedIn profile, like https://www.linkedin.com/in/jane-doe" },
                "email": { "type": "string", "description": "Optional. An address they're known by, which helps the finders match" },
            }),
            &["project_id", "linkedin_url"],
            adds.clone(),
        ),
        tool(
            "find_emails",
            "Find emails for a list of people",
            "Queues up to 500 people for find_email's search, in the background and within the organization's 200 email \
             searches an hour. Returns task_id for get_email_batch.",
            json!({
                "project_id": project,
                "people": {
                    "type": "array",
                    "maxItems": 500,
                    "items": {
                        "type": "object",
                        "properties": {
                            "first_name": { "type": "string" },
                            "last_name": { "type": "string" },
                            "company_domain": { "type": "string" },
                            "linkedin_url": { "type": "string" },
                            "client_ref": { "type": "string", "description": "Optional. Your own id for the person, given back with their result" },
                        },
                        "required": ["first_name", "company_domain"],
                    },
                },
            }),
            &["project_id", "people"],
            adds.clone(),
        ),
        tool(
            "get_email_batch",
            "Get email batch results",
            "A find_emails batch: status pending or completed, how many of total are searched, and each person's result in the \
             order sent, pending until searched. Results are kept for 30 days.",
            json!({ "task_id": id_prop("A task_id from find_emails") }),
            &["task_id"],
            read.clone(),
        ),
        tool(
            "download_companies",
            "Download company search",
            "The companies a search_companies search finds, in its order, as CSV: name, domain, website, email, phone, address, city, \
             region, postcode, country, category, Overture's confidence, source_id and source_release. Returns the first rows (20 \
             unless per_page says up to 200), more when there are others, and download_url, the whole file from the API. Cells that \
             start like a formula start with an apostrophe. Counts against the organization's 20 downloads an hour, so search first.",
            json!({
                "q": { "type": "string", "description": "Words in the name or category, like \"dental\"" },
                "country": { "type": "string", "description": "Two-letter country code, like GB" },
                "city": { "type": "string", "description": "City name, like Leeds" },
                "category": { "type": "string", "description": "Exact category, like dental_clinic" },
                "has_email": { "type": "boolean", "description": "Only companies that list an email" },
                "has_phone": { "type": "boolean", "description": "Only companies that list a phone" },
                "tech": { "type": "string", "description": "Only companies that use this tool or a tool of this category, like search_companies" },
                "project_id": id_prop("Optional. The project searching, so its dedup lists apply too"),
                "include_hidden": { "type": "boolean", "description": "Optional. True to include the companies on dedup lists" },
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Optional. Only these of the search's companies, up to 500" },
                "per_page": { "type": "integer", "minimum": 1, "maximum": MAX_DOWNLOAD_ROWS, "description": "Rows in the answer, 20 by default" },
            }),
            &[],
            read.clone(),
        ),
        tool(
            "find_lookalikes",
            "Find companies like your customers",
            "Ranks the company database by how much each company has in common with the project's customers: by default the \
             companies of its won deals, plus or minus the ones picked on the Companies page. profile is what at least a third of \
             them share (industry, tools, description words, hiring for the roles you sell to, country and region) and \
             whether each part is on. Each result has a 0-100 score and the reasons that counted most. Seeds, companies already in \
             the project and do-not-contact domains are left out. total counts all of the up to 200 matches.",
            json!({
                "project_id": project,
                "seeds": { "type": "array", "items": { "type": "string" }, "description": "Optional. Customer domains to match instead of the project's" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Results to return, 25 by default" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "add_lookalikes",
            "Add lookalike companies to campaign",
            "Adds companies from find_lookalikes to a campaign, with their score and reasons as why they fit. Pass domains, or top \
             for the best that many. Pass the same seeds as find_lookalikes if you gave it some. Companies no longer among the \
             matches are skipped. Then call find_people.",
            json!({
                "project_id": project,
                "campaign_id": campaign,
                "domains": { "type": "array", "items": { "type": "string" }, "description": "Domains from find_lookalikes" },
                "top": { "type": "integer", "minimum": 1, "maximum": 200, "description": "Instead of domains, add the best this many" },
                "seeds": { "type": "array", "items": { "type": "string" }, "description": "Optional. The seeds find_lookalikes used" },
            }),
            &["project_id", "campaign_id"],
            adds.clone(),
        ),
        tool(
            "search_local_businesses",
            "Search local businesses",
            "Searches Google Maps through the organization's own Google Maps key, connected under Integrations, for local businesses \
             like dentists, gyms or restaurants in a city or ZIP code: up to 60 a place, with category, address, phone, website, \
             rating and review count. where can list up to 20 places split by semicolons, and each business shows once. Each place \
             costs up to 3 calls against the organization's monthly cap, and a place searched in the last 30 days is free. With \
             project_id, known says whether the project already has a business as a company or a lead.",
            json!({
                "what": { "type": "string", "description": "The kind of business, like \"dentists\"" },
                "where": { "type": "string", "description": "A city, area or ZIP code, like \"Austin, TX\", or several split by semicolons" },
                "min_rating": { "type": "number", "minimum": 0, "maximum": 5, "description": "Optional. Only businesses rated at least this" },
                "min_reviews": { "type": "integer", "minimum": 0, "description": "Optional. Only businesses with at least this many reviews" },
                "with_website": { "type": "boolean", "description": "Optional. Only businesses with a website" },
                "project_id": id_prop("Optional. Marks the businesses this project already has"),
            }),
            &["what", "where"],
            read.clone(),
        ),
        tool(
            "add_local_businesses",
            "Add local businesses to campaign",
            "Adds businesses from search_local_businesses to a campaign by place_id, up to 50 at a time. Each with a website becomes \
             a company, and each one a lead: Splee reads its website for a contact address at its own domain, a person's before a \
             shared one like info@. One without a website keeps its phone for calls and texts. Returns what each got, and the ones \
             skipped with why. Then call write_emails.",
            json!({
                "campaign_id": campaign,
                "place_ids": { "type": "array", "items": { "type": "string" }, "description": "place_id values from search_local_businesses" },
            }),
            &["campaign_id", "place_ids"],
            adds.clone(),
        ),
        tool(
            "find_people",
            "Find people",
            "Starts an agent run that finds decision makers and their emails at the campaign's new companies, or only at company_ids or \
             domains, in the campaign's roles or the titles given. Returns run_id. Poll get_run until it's done, then list_leads.",
            json!({
                "campaign_id": campaign,
                "company_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these companies" },
                "domains": {
                    "type": "array", "items": { "type": "string" },
                    "description": "Optional, in place of company_ids. Up to 100 company domains, added to the campaign when the project hasn't got them"
                },
                "titles": { "type": "array", "items": { "type": "string" }, "description": "Optional. Up to 20 job titles to look for instead of the campaign's roles" },
                "per_company": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Optional. People to find at each company, 2 by default" },
            }),
            &["campaign_id"],
            adds.clone(),
        ),
        tool(
            "write_emails",
            "Write emails",
            "Starts an agent run that writes a personal first email for every lead in the campaign that has none. The emails wait as \
             drafts for approval. Returns run_id. Poll get_run until it's done, then list_emails_to_approve.",
            json!({
                "campaign_id": campaign,
                "instructions": { "type": "string", "description": "Optional. Extra guidance for this batch, like \"mention our Leeds office\"" },
            }),
            &["campaign_id"],
            adds.clone(),
        ),
        tool(
            "get_analytics",
            "Get analytics",
            "What's working in a project over a date range, 30 days by default and at most 365: totals with reply, hot and \
             bounce rates by lead, meetings, deals won, unsubscribes and spend split into claude_cost_micros and email_cost_micros \
             (micro-dollars), revenue_cents from the payments of leads through Stripe, Paddle or Lemon Squeezy in the organization's \
             currency (payments in others are counted in ignored_payments) with revenue_per_meeting_cents and return_on_spend (null \
             unless that currency is USD, since spend is in dollars), the same totals for the period before in previous, a daily series (weekly past 90 days), and breakdowns by campaign (with its leads' revenue), mailbox \
             (bouncing flags over 5% bounced), sequence step, A/B test angle, weekday and hour of send in the project's time zone, \
             channel, and the language the emails were written in. Replies, bounces and meetings count for the last email the lead got before them. Totals, campaigns, \
             angles, mailboxes and steps have opened, open_rate, clicked and click_rate, by email out of those sent in the range \
             that tracked opens or clicks, scanners left out. A null rate means none tracked it. Open rates are estimates, since \
             Apple Mail opens every email itself and some people block the pixel.",
            json!({
                "project_id": project,
                "from": { "type": "string", "format": "date", "description": "Optional. First day, as YYYY-MM-DD in the project's time zone" },
                "to": { "type": "string", "format": "date", "description": "Optional. Last day, as YYYY-MM-DD. Today by default" },
                "campaign_id": id_prop("Optional. Only this campaign"),
                "mailbox_id": id_prop("Optional. Only emails from this mailbox"),
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "get_run",
            "Get run status",
            "An agent run's status: queued, running, succeeded, failed or cancelled, with its latest progress line in message and \
             why it failed in error.",
            json!({ "run_id": id_prop("A run_id from create_campaign, find_people or write_emails") }),
            &["run_id"],
            read.clone(),
        ),
        tool(
            "list_tasks",
            "List background tasks",
            "The organization's background work, newest first: agent runs, find_emails batches and import_campaign imports, each \
             with status pending, completed or failed and the url that follows it. For finding a run_id or task_id again.",
            page_props(json!({
                "status": { "type": "string", "enum": ["pending", "completed", "failed"], "description": "Optional" },
                "type": { "type": "string", "enum": ["run", "email_batch", "campaign_import"], "description": "Optional" },
                "kind": { "type": "string", "description": "Optional. Only runs of this kind, like find_people or write_emails" },
                "project_id": id_prop("Optional. Only this project's"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "list_leads",
            "List leads",
            "A project's leads, newest first, or the likeliest buyers first with sort \"score\". Each has a 0-100 score, its fit \
             and intent parts (0-50 each) and the reasons behind it, the language set on it, if any, and effective_language, the \
             one a campaign set to each lead's language writes to them in.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's leads"),
                "status": { "type": "string", "enum": LEAD_STATUSES, "description": "Optional. Only leads with this status" },
                "q": { "type": "string", "description": "Optional. Matches name, email, company or title" },
                "language": { "type": "string", "description": "Optional. Only leads whose effective_language is this code, like de" },
                "min_score": score_prop(),
                "sort": sort_prop(),
                "owner": owner_prop(),
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_companies",
            "List companies",
            "A project's own companies, best fit first, or the likeliest buyers first with sort \"score\". A company's score is its \
             best lead's, or its own while it has no leads. search_companies searches the company database instead.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "q": { "type": "string", "description": "Optional. Matches name or domain" },
                "min_score": score_prop(),
                "sort": sort_prop(),
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_emails_to_approve",
            "List emails to approve",
            "Draft emails waiting for approval, oldest first, with the full subject and body. Show them to the user before approving.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "campaign_id": id_prop("Optional. Only this campaign"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "approve_email",
            "Approve email",
            "Approves a draft so it's sent from the user's mailboxes on the project's schedule, once the campaign is sending. Only \
             approve emails the user has agreed to send.",
            json!({ "email_id": id_prop("An id from list_emails_to_approve") }),
            &["email_id"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "skip_email",
            "Skip email",
            "Cancels a draft so it's never sent. approve_email can still send it later.",
            json!({ "email_id": id_prop("An id from list_emails_to_approve") }),
            &["email_id"],
            adds.clone(),
        ),
        tool(
            "list_replies",
            "List replies",
            "Reply threads from the inbox, the latest first, with the lead, how the reply was classified and the last message cut to \
             300 characters.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "campaign_id": id_prop("Optional. Only this campaign's leads"),
                "filter": { "type": "string", "enum": ["all", "needs_reply", "hot", "auto_replied", "other"], "description": "all by default" },
                "q": { "type": "string", "description": "Optional. Matches the lead's name, email or company" },
                "owner": owner_prop(),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "get_thread",
            "Get thread",
            "A lead's emails both ways, oldest first, with who each one copied in and the names, types and sizes of its files. \
             reply_cc is who reply_to_lead copies in by default.",
            json!({ "lead_id": id_prop("A lead id from list_replies or list_leads") }),
            &["lead_id"],
            read.clone(),
        ),
        tool(
            "reply_to_lead",
            "Reply to lead",
            "Sends a reply to the lead's last email from the mailbox they wrote to. It copies in everyone the lead copied, unless \
             cc_emails names who to copy in instead. Only reply when the user asked to, with the text they agreed to.",
            json!({
                "lead_id": id_prop("A lead id from list_replies or list_leads"),
                "body_text": { "type": "string", "description": "The reply, as plain text" },
                "cc_emails": {
                    "type": "array",
                    "items": { "type": "string" },
                    "maxItems": 20,
                    "description": "Optional. Everyone to copy in, in place of the lead's CC list. An empty list copies in nobody",
                },
                "reply_all": { "type": "boolean", "description": "Optional. false copies in nobody when cc_emails isn't given. true by default" },
            }),
            &["lead_id", "body_text"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "mark_replied",
            "Mark replied",
            "Takes a reply the user answered outside Splee, or that needs no answer, out of Needs reply, and cancels the agent's reply \
             draft still waiting for it. The thread comes back when the lead writes again. The lead's status doesn't change.",
            json!({ "lead_id": id_prop("A lead id from list_replies or list_leads") }),
            &["lead_id"],
            json!({ "readOnlyHint": false }),
        ),
        tool(
            "list_deals",
            "List deals",
            "The pipeline's stages in order, and its deals, the latest first, with the lead, stage, value in the currency's main unit \
             and owner. A hot reply opens a deal and a booked meeting moves it to the booked stage by itself.",
            page_props(json!({
                "project_id": id_prop("Optional. Only this project"),
                "status": { "type": "string", "enum": ["open", "won", "lost"], "description": "Optional. Only deals with this status" },
                "lead_id": id_prop("Optional. Only this lead's deals"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "create_deal",
            "Create deal",
            "Opens a deal for a lead, in the first stage unless you name one. A lead can have only one open deal.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "stage": { "type": "string", "description": "Optional. A stage name from list_deals" },
                "value": { "type": "number", "minimum": 0, "description": "Optional. In the currency's main unit, like 12000" },
                "currency": { "type": "string", "description": "Optional. A three-letter code. The organization's currency by default" },
                "close_date": { "type": "string", "format": "date", "description": "Optional. When it should close, as YYYY-MM-DD" },
                "notes": { "type": "string", "description": "Optional" },
            }),
            &["lead_id"],
            adds.clone(),
        ),
        tool(
            "move_deal",
            "Move deal",
            "Moves a deal to another stage, forward or back. Moving it to a won stage closes it as won, a lost stage as lost, and an \
             open stage reopens it.",
            json!({
                "deal_id": id_prop("A deal id from list_deals"),
                "stage": { "type": "string", "description": "A stage name from list_deals" },
                "value": { "type": "number", "minimum": 0, "description": "Optional. The final value, when it's won" },
                "lost_reason": { "type": "string", "description": "Optional. Why it was lost" },
            }),
            &["deal_id", "stage"],
            adds.clone(),
        ),
        tool(
            "set_lead_language",
            "Set lead language",
            "Sets the language a lead is written to in, for campaigns that write in each lead's own language. Empty goes back to \
             picking it from their country, then their company's website, then English. Returns effective_language, the one now \
             used, and effective_language_source: set, country, site or default.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "language": { "type": "string", "description": "A code like de, fr or es, or its English name, or \"\" to clear it" },
            }),
            &["lead_id", "language"],
            adds.clone(),
        ),
        tool(
            "set_campaign_language",
            "Set campaign language",
            "Sets the language a campaign writes its emails, follow-ups, texts and LinkedIn messages in: lead (the default) to write to \
             each lead in their own language, en for English, or another language code. Emails already written keep theirs until they're rewritten. \
             Drafted replies always answer in the language the lead wrote in.",
            json!({
                "campaign_id": campaign,
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "en, another language code, or lead" },
            }),
            &["campaign_id", "language"],
            adds.clone(),
        ),
        tool(
            "get_campaign",
            "Get campaign",
            "A campaign's whole definition as its page shows it: status and status_reason, targeting, the brief its emails are \
             written from (pain_points, angle, and the A/B test's angles in ab_test), language, tracking, the project's schedule and \
             follow-ups it sends on, an imported campaign's sequence, and its funnel. read_only lists what can't be changed here and why.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "get_campaign_send_state",
            "Is a campaign sending",
            "Whether a campaign's emails go out now and, if not, the one thing holding them: state (sending, scheduled, archived, \
             not_started, paused, project_paused, no_mailbox, awaiting_approval, nothing_queued, monthly_cap_reached, \
             lead_cap_reached, daily_limit_reached, campaign_limit_reached, mailboxes_full or outside_hours), message, next_action \
             (the call that clears it, null when it only waits), today's counts with next_send_at, and the allowed_actions. Ask the \
             user before a next_action that starts sending.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "update_campaign",
            "Update campaign",
            "Changes a campaign's definition as its page does and returns it as get_campaign does. Pass only what changes. Each \
             targeting list sent replaces that list, and the others stay. Emails already written keep their brief and language \
             until they're rewritten. The schedule and follow-ups are the project's, so they're refused here.",
            json!({
                "campaign_id": campaign,
                "name": { "type": "string" },
                "description": { "type": "string", "description": "Who the campaign is for, in a sentence or two" },
                "targeting": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "industries": texts, "roles": texts, "company_sizes": texts, "countries": texts, "exclude_countries": texts,
                        "keywords": texts, "exclude_keywords": texts,
                    },
                },
                "pain_points": { "type": "string", "description": "The problems the emails speak to" },
                "angle": { "type": "string", "description": "How the emails pitch the offer" },
                "language": { "type": "string", "enum": LANGUAGE_CHOICES.as_slice(), "description": "en, another language code, or lead" },
                "track_clicks": { "type": "boolean" },
                "track_opens": { "type": "boolean" },
                "lead_pages": { "type": "boolean", "description": "A personal page for each lead, written with the first email" },
                "daily_limit": {
                    "type": ["integer", "null"], "minimum": 1, "maximum": 5000,
                    "description": "The most first emails and follow-ups it sends a day, on top of the project's limit. null takes it off"
                },
            }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "get_my_copy",
            "Get campaign first email",
            "A campaign's first email setup. mode instructions has the agent write every first email (from the campaign's angle, \
             in instructions). mode my_copy sends the user's own subject and body with each lead's details filled in, to 9 leads in \
             10, and the AI email to the other tenth so the two can be compared, with how each did in results. Follow-ups are \
             written as before. source project means the campaign sends the project's first email, campaign its own.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "update_my_copy",
            "Update campaign first email",
            MY_COPY_TEXT_RULES,
            json!({
                "campaign_id": campaign,
                "mode": { "type": "string", "enum": ["instructions", "my_copy"], "description": "Optional. my_copy sends the user's text" },
                "subject": { "type": "string", "description": "Optional. Empty sends \"<your company> x <their company>\"" },
                "body": { "type": "string", "description": "Optional. Needed to turn my_copy on" },
            }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "set_my_copy_source",
            "Set campaign first email source",
            "project makes the campaign send the project's first email, campaign gives it its own. Nothing is lost: switching back \
             brings the other text back. A campaign's first own text starts as a copy of the project's.",
            json!({ "campaign_id": campaign, "source": { "type": "string", "enum": ["project", "campaign"] } }),
            &["campaign_id", "source"],
            control.clone(),
        ),
        tool(
            "preview_my_copy",
            "Preview campaign first email",
            "The user's text as the campaign's first 3 leads get it: variables and {a|b} filled in, and each {{ai: …}} line shown \
             as its instruction, since the agent writes those with the email. A lead that gets the AI email instead says why in note.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            read.clone(),
        ),
        tool(
            "get_project_my_copy",
            "Get project first email",
            "The project's default first email, which every campaign in campaign_ids sends: mode, subject and body as in \
             get_my_copy, and the project's extra instructions for every AI email.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "update_project_my_copy",
            "Update project first email",
            MY_COPY_TEXT_RULES,
            json!({
                "project_id": project,
                "mode": { "type": "string", "enum": ["instructions", "my_copy"], "description": "Optional. my_copy sends the user's text" },
                "subject": { "type": "string", "description": "Optional. Empty sends \"<your company> x <their company>\"" },
                "body": { "type": "string", "description": "Optional. Needed to turn my_copy on" },
            }),
            &["project_id"],
            control.clone(),
        ),
        tool(
            "start_campaign",
            "Start campaign",
            "The campaign's Start button: it sends on the project's schedule, and its drafts are approved for sending. A campaign \
             that's already sending is left as it is. Sending also needs the project's sending on, see update_project_controls.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "stop_campaign",
            "Stop campaign",
            "The campaign's Pause button: its unsent first emails and follow-ups go back to drafts until it starts again. Replies \
             still come in, and answers to them stay queued. A campaign that isn't sending is left as it is.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "approve_campaign",
            "Approve campaign drafts",
            "Approves every first email and follow-up waiting as a draft in the campaign, like approve_email on each, and says how \
             many in approved. In a sending campaign they go out on the project's schedule. A paused one holds them until it starts.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "archive_campaign",
            "Archive campaign",
            "Puts away a campaign you're done with: it stops sending like stop_campaign and drops out of list_campaigns. Its leads, \
             emails and stats stay, and replies still come in. Archiving an archived campaign changes nothing. Returns the campaign.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "unarchive_campaign",
            "Unarchive campaign",
            "Brings an archived campaign back to list_campaigns, paused. It only sends once start_campaign starts it. Returns the campaign.",
            json!({ "campaign_id": campaign }),
            &["campaign_id"],
            control.clone(),
        ),
        tool(
            "update_project_controls",
            "Update project controls",
            "Changes a project's sending controls and returns them all. Pass only what changes, or nothing to read them. sending \
             turns the whole project's sending on or off, and on needs an active mailbox. A field it doesn't take is refused.",
            json!({
                "project_id": project,
                "sending": { "type": "boolean", "description": "Optional. The project's Start and Pause sending" },
                "daily_email_limit": { "type": "integer", "minimum": 1, "maximum": 5000, "description": "Optional. Emails a day" },
                "daily_text_limit": { "type": "integer", "minimum": 1, "maximum": 1000, "description": "Optional. Texts a day" },
                "timezone": { "type": "string", "description": "Optional. The send window's time zone, like Europe/London" },
                "send_days": { "type": "array", "items": { "type": "integer", "minimum": 1, "maximum": 7 }, "description": "Optional. 1 is Monday, 7 Sunday" },
                "send_start_hour": { "type": "integer", "minimum": 0, "maximum": 23, "description": "Optional" },
                "send_end_hour": { "type": "integer", "minimum": 1, "maximum": 24, "description": "Optional. After send_start_hour" },
                "autopilot": { "type": "boolean", "description": "Optional. Keeps sending campaigns stocked with written emails" },
                "autopilot_budget_micros": { "type": "integer", "minimum": 0, "description": "Optional. Claude spend a month in micro-dollars" },
                "autopilot_lead_cap": { "type": "integer", "minimum": 0, "maximum": 100000, "description": "Optional. New leads emailed a month" },
                "autopilot_auto_send": { "type": "boolean", "description": "Optional. Autopilot's emails go out without approval" },
                "review_window_hours": { "type": ["integer", "null"], "minimum": 1, "maximum": 72,
                                         "description": "Optional. Drafts nobody reviewed go out after this many hours. null keeps them waiting" },
                "monthly_cap_micros": { "type": ["integer", "null"], "minimum": 1000000, "maximum": 1_000_000_000_000_i64,
                                        "description": "Optional. Email spend a month in micro-dollars. null for no cap" },
                "sender_name": { "type": "string", "maxLength": 100,
                                 "description": "Optional. The name the agent signs emails with, not an address. Empty for the team" },
            }),
            &["project_id"],
            control,
        ),
        tool(
            "get_monthly_cap",
            "Get monthly cap",
            "The project's cap on email spend a calendar month (UTC), what its sent emails cost since the 1st, and whether first emails \
             and follow-ups are waiting for next month because it was reached. Replies always go out.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_hot_leads",
            "List hot leads",
            "Every lead across the organization whose reply was hot, newest first: name, email, title, company, domain, LinkedIn, \
             their campaign, the reply that made them hot and why_hot, and became_hot_at. Pass since as the newest became_hot_at \
             you've seen to get only new ones.",
            page_props(json!({
                "since": { "type": "string", "format": "date-time", "description": "Optional. Only leads that became hot after this" },
                "project_id": id_prop("Optional. Only this project's leads"),
            })),
            &[],
            read.clone(),
        ),
        tool(
            "get_inbox_changes",
            "Get new replies",
            "A project's replies that came in since cursor, oldest first, each once. Pass next_cursor as cursor on the next call. \
             On the first call pass since or nothing for every reply. has_more means call again now. A reply shows up about 5 \
             seconds after it arrives, and classification is null until the classifier has read it.",
            json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's replies"),
                "cursor": { "type": "string", "description": "Optional. next_cursor from the last call" },
                "since": { "type": "string", "format": "date-time", "description": "Optional. On the first call, only replies after this" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Optional. At most this many, 100 by default" },
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_erased_leads",
            "List erased leads",
            "Leads someone deleted or forgot in Splee since cursor, oldest first, each once, so the user can delete what they keep of \
             each one in their CRM or sheets. Each has only lead_id, project_id, campaign_id, forgotten and erased_at, since the \
             person's name and address are gone. Pass next_cursor as cursor on the next call. has_more means call again now.",
            json!({
                "project_id": id_prop("Optional. Only this project"),
                "cursor": { "type": "string", "description": "Optional. next_cursor from the last call" },
                "since": { "type": "string", "format": "date-time", "description": "Optional. On the first call, only leads erased after this" },
                "limit": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Optional. At most this many, 100 by default" },
            }),
            &[],
            read.clone(),
        ),
        tool(
            "assign_lead",
            "Assign lead",
            "Gives a lead to someone on the team, who then gets its hot-reply alerts, or takes it back with owner null. The project's \
             assignment rule never moves a lead that has an owner.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "owner": { "type": ["string", "null"], "description": "A team member's email or user id, or null for nobody" },
            }),
            &["lead_id", "owner"],
            adds.clone(),
        ),
        tool(
            "list_call_tasks",
            "List call tasks",
            "A project's calls to make, overdue first, then the calls made in the last 14 days. bucket is overdue, today or done. \
             local_time is the time where the lead is, and outside_hours says why not to call now: calls go out between 8:00 and 20:00 \
             their time.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "status": { "type": "string", "enum": ["open", "done"], "description": "Optional. Both by default" },
                "owner": owner_prop(),
            }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "log_call",
            "Log call",
            "Records a call made with a lead and what happened. It closes the lead's open call task, so their sequence goes on. \
             meeting_booked books the lead, not_interested stops their sequence, and wrong_number clears their phone number.",
            json!({
                "lead_id": id_prop("A lead id from list_call_tasks or list_leads"),
                "outcome": { "type": "string", "enum": ["connected", "voicemail", "no_answer", "wrong_number", "not_interested", "meeting_booked"] },
                "notes": { "type": "string", "description": "Optional. What was said" },
            }),
            &["lead_id", "outcome"],
            adds.clone(),
        ),
        tool(
            "list_meetings",
            "List meetings",
            "A lead's sales meetings from Fireflies, Fathom, Gong or Zoom, oldest first: the title, when, who from their side, the \
             agent's summary and next steps, and the deal stage it suggested. stage_status is pending until someone accepts or \
             dismisses that stage in Splee. follow_up_status is the status of the follow-up email drafted after it, draft while it \
             waits for approval.",
            json!({ "lead_id": id_prop("A lead id from list_leads or list_replies") }),
            &["lead_id"],
            read.clone(),
        ),
        tool(
            "list_sheet_connections",
            "List Google Sheets",
            "A project's Google Sheets tabs: kind import is a tab leads come from, read every 15 minutes when keep_importing is on, \
             and kind export a tab that gets a row for each hot lead, booked meeting or won deal in events. rows_added counts the \
             leads or rows Splee added so far, and error says why the last read or write failed.",
            json!({ "project_id": id_prop("A project id from list_projects") }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "import_from_sheet",
            "Import from Google Sheet",
            "Reads an import tab now and adds the rows it hasn't seen as leads in its campaign. Rows are told apart by email, or by \
             their content when they have none. Suppressed and duplicate addresses are skipped, with the reason in errors.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "connection_id": id_prop("An import tab's id from list_sheet_connections"),
            }),
            &["project_id", "connection_id"],
            adds.clone(),
        ),
        tool(
            "list_job_changes",
            "List job changes",
            "People who bought, booked a meeting or replied hot and then moved to a new company, newest first, from their LinkedIn \
             profiles. relationship is won, booked or hot. new_lead_id is the lead Splee made at the new company, whose email the \
             finders look up. watching counts the people on the watch list, and connected says whether LinkedIn is connected through \
             Unipile, which the checks need.",
            json!({ "project_id": id_prop("A project id from list_projects") }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "add_research_question",
            "Add a research question",
            "Adds a question the agent answers about every company in the project, like \"Do they sell to hospitals?\" or \"Which \
             CRM do they use?\". It shows as a column on the Companies page. answer_type yes_no gets yes or no, text a short answer. \
             With use_in_emails, the email writer can use the answers. Up to 10 a project. Then call research_companies.",
            json!({
                "project_id": project,
                "question": { "type": "string", "maxLength": 300, "description": "The question, about one company" },
                "label": { "type": "string", "maxLength": 40, "description": "Optional. A short column name. The question's first words by default" },
                "answer_type": { "type": "string", "enum": ["yes_no", "text"], "description": "yes_no by default" },
                "use_in_emails": { "type": "boolean", "description": "Let the email writer use the answers. False by default" },
            }),
            &["project_id", "question"],
            adds.clone(),
        ),
        tool(
            "research_companies",
            "Research companies",
            "Queues the agent to answer the project's research questions for its companies from their websites and the web, up to \
             200 companies a call, best score first. Each company is one Claude call, costed like any run. By default every \
             question, every company that isn't excluded, and only answers not found yet. companies says how many were queued and \
             more how many are left for another call. Answers take a few minutes, then show in list_research.",
            json!({
                "project_id": project,
                "question_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these questions" },
                "company_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these companies" },
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "redo": { "type": "boolean", "description": "Ask again where an answer was already found" },
                "dry_run": { "type": "boolean", "description": "Only count what would be queued" },
            }),
            &["project_id"],
            adds.clone(),
        ),
        tool(
            "list_research",
            "List research answers",
            "The project's research questions and each company's answers by question id, each with its question's label, best score first. An answer's status is \
             answered, unknown (nothing the agent read said), queued (being researched) or failed. Pass question_id with answer to \
             filter: yes, no, answered, unknown, or none for not researched yet.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's companies"),
                "q": { "type": "string", "description": "Optional. Search names and domains" },
                "question_id": id_prop("Optional. With answer, filter by this question"),
                "answer": { "type": "string", "enum": ["yes", "no", "answered", "unknown", "none"], "description": "Optional. The answer to filter by" },
            })),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "list_company_signals",
            "List company funding and news",
            "Funding and news at the project's companies, newest first: Form D filings from SEC EDGAR (kind funding_filing, with \
             amount sold and offered in US dollars) and posts on each company's own blog or newsroom feed about funding, a launch, an \
             acquisition, an expansion or a new leader. headline is the line the score and the email writer use, like \"Raised $12M \
             (SEC filing, Oct 3)\". Signals count for 90 days. company_id is set when the company is in the project, and leads counts \
             its leads.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "kind": { "type": "string", "enum": crate::signals::KINDS, "description": "Optional. Only this kind" },
                "days": { "type": "integer", "minimum": 1, "description": "Optional. Only signals from the last this many days" },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_SIGNALS, "description": "At most this many, 25 by default" },
            }),
            &["project_id"],
            read,
        ),
        tool(
            "add_to_watch_list",
            "Add to job change watch list",
            "Adds past customers or champions to a project's watch list, so Splee notices when they move to a new company. Each \
             person needs an email or a LinkedIn URL, and the checks need the URL. Someone already on the list isn't added twice.",
            json!({
                "project_id": id_prop("A project id from list_projects"),
                "relationship": { "type": "string", "enum": ["won", "booked", "hot"],
                                  "description": "What they did with you: won (bought), booked (a meeting) or hot (replied with interest). Defaults to won" },
                "people": { "type": "array", "items": { "type": "object", "properties": {
                    "name": { "type": "string" }, "email": { "type": "string" }, "linkedin_url": { "type": "string" },
                    "company": { "type": "string" }, "company_domain": { "type": "string" }, "title": { "type": "string" },
                } } },
            }),
            &["project_id", "people"],
            adds,
        ),
    ]
}

fn owner_prop() -> Value {
    json!({ "type": "string", "description": "Optional. Only leads of this team member, by email or user id, or \"unassigned\"" })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::Router;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::config::Config;
    use crate::crypto::sha256_hex;
    use crate::state::AppState;

    const KEY: &str = "splee_test_key_a";

    struct Server {
        url: String,
        client: reqwest::Client,
    }

    impl Server {
        async fn start(db: PgPool) -> Self {
            Self::with(db, String::new(), String::new()).await
        }

        /// With Google Places and the businesses' websites at fakes.
        async fn with(db: PgPool, places_api_url: String, local_sites_url: String) -> Self {
            let cfg = Config {
                database_url: String::new(),
                app_secret: String::new(),
                public_url: "http://localhost:3346".into(),
                unsubscribe_url: "http://localhost:3346".into(),
                bind_addr: String::new(),
                agent_model: String::new(),
                agent_concurrency: 0,
                sending_enabled: false,
                bird_bin: None,
                open_signup: true,
                public_agent: false,
                porkbun_api_url: String::new(),
                twilio_api_url: String::new(),
                bots_api_url: String::new(),
                outreach_api_url: String::new(),
                postmaster_api_url: String::new(),
                operator_emails: Vec::new(),
                google: None,
                microsoft: None,
                slack: None,
                tumblr: None,
                salesforce: None,
                zoho: None,
                meta: None,
                graph_api_url: String::new(),
                crm_api_url: String::new(),
                payments_api_url: String::new(),
                meetings_api_url: String::new(),
                webflow_api_url: String::new(),
                linkedin_ads: None,
                google_ads_developer_token: None,
                ads_api_url: String::new(),
                ipinfo_token: None,
                places_api_url,
                local_sites_url,
                sheets_api_url: String::new(),
                sec_user_agent: None,
                sec_api_url: String::new(),
                news_sites_url: String::new(),
                s3_api_url: String::new(),
            };
            let state = AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/api/mcp", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router().merge(super::v1::router(&state))).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { url, client: reqwest::Client::new() }
        }

        async fn post(&self, key: &str, body: Value) -> (u16, Value) {
            let res = self.client.post(&self.url).bearer_auth(key).header("accept", "application/json, text/event-stream").json(&body);
            let res = res.send().await.unwrap();
            let status = res.status().as_u16();
            (status, res.json().await.unwrap_or(Value::Null))
        }

        async fn call(&self, key: &str, tool: &str, arguments: Value) -> (bool, String) {
            let request = json!({ "jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": { "name": tool, "arguments": arguments } });
            let (status, body) = self.post(key, request).await;
            assert_eq!(status, 200, "{body}");
            let result = &body["result"];
            (result["isError"].as_bool().unwrap(), result["content"][0]["text"].as_str().unwrap().to_string())
        }
    }

    /// Two organizations, each with a project and campaign. Only the first has a key, KEY.
    async fn seed(db: &PgPool) -> [(Uuid, Uuid); 2] {
        let mut out = vec![];
        for name in ["Acme", "Other"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                .bind(org)
                .bind(format!("{name} project"))
                .fetch_one(db)
                .await
                .unwrap();
            let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id")
                .bind(project)
                .fetch_one(db)
                .await
                .unwrap();
            out.push((project, segment));
            if name == "Acme" {
                sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'mcp', 'splee_test_', $2)")
                    .bind(org)
                    .bind(sha256_hex(KEY))
                    .execute(db)
                    .await
                    .unwrap();
            }
        }
        [out[0], out[1]]
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn handshake_and_tool_list(db: PgPool) {
        seed(&db).await;
        let server = Server::start(db).await;

        let init = |version: &str| json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "protocolVersion": version, "capabilities": {}, "clientInfo": { "name": "test", "version": "1" } } });
        let (status, body) = server.post(KEY, init("2025-06-18")).await;
        assert_eq!(status, 200);
        assert_eq!(body["id"], 1);
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        assert!(body["result"]["capabilities"]["tools"].is_object());
        assert_eq!(server.post(KEY, init("2099-01-01")).await.1["result"]["protocolVersion"], "2025-11-25");

        assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await.0, 202);
        assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "id": "p", "method": "ping" })).await.1["result"], json!({}));
        assert_eq!(server.post(KEY, json!({ "jsonrpc": "2.0", "id": 2, "method": "nope" })).await.1["error"]["code"], -32601);

        let (_, body) = server.post(KEY, json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/list" })).await;
        let tools = body["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        for name in ["list_projects", "create_campaign", "search_companies", "add_companies", "find_people", "write_emails", "get_run",
                     "list_leads", "list_companies", "list_emails_to_approve", "approve_email", "skip_email", "list_replies", "list_deals",
                     "create_deal", "move_deal", "get_analytics", "assign_lead", "list_call_tasks", "log_call", "list_meetings", "get_thread", "reply_to_lead",
                     "mark_replied", "search_local_businesses", "add_local_businesses", "list_sheet_connections", "import_from_sheet",
                     "list_job_changes", "add_to_watch_list", "set_lead_language", "set_campaign_language", "find_lookalikes",
                     "add_lookalikes", "list_company_signals", "add_research_question", "research_companies", "list_research", "get_campaign",
                     "start_campaign", "stop_campaign", "approve_campaign", "archive_campaign", "unarchive_campaign", "update_project_controls",
                     "list_hot_leads", "get_inbox_changes", "find_companies_by_domains", "get_monthly_cap",
                     "update_campaign",
                     "list_erased_leads",
                     "list_sending_domains", "suggest_sending_domains", "check_sending_domains",
                     "get_project", "create_project",
                     "list_tasks",
                     "get_campaign_send_state",
                     "find_phone",
                     "list_saved_searches", "save_search", "delete_saved_search", "list_new_matches", "find_email", "find_emails",
                     "get_email_batch", "download_companies", "fill_company_filters", "import_campaign", "get_campaign_import",
                     "list_campaign_import_rows", "get_my_copy", "update_my_copy", "set_my_copy_source", "preview_my_copy",
                     "get_project_my_copy", "update_project_my_copy", "list_company_dedup_lists", "create_company_dedup_list",
                     "get_company_dedup_list", "set_company_dedup_list_projects", "delete_company_dedup_list"] {
            assert!(names.contains(&name), "{name} missing");
        }
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && !t["description"].as_str().unwrap().is_empty()));

        // A version Splee doesn't know gets the list of ones it does, which tells a newer client to pick one rather than fall back.
        let request = json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2099-01-01" } } });
        let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2099-01-01").json(&request).send().await.unwrap();
        assert_eq!(res.status(), 400);
        let body: Value = res.json().await.unwrap();
        assert_eq!((body["id"].clone(), body["error"]["code"].clone()), (json!(4), json!(-32022)));
        assert_eq!(body["error"]["data"], json!({ "supported": ["2026-07-28", "2025-11-25", "2025-06-18"], "requested": "2099-01-01" }));
        assert_eq!(server.client.get(&server.url).bearer_auth(KEY).send().await.unwrap().status(), 405);
        let res = server.client.post(&server.url).bearer_auth(KEY).header("origin", "https://evil.example").json(&json!({})).send().await.unwrap();
        assert_eq!(res.status(), 403);
        // The public API's per-organization rate limit covers MCP too.
        let res = server.client.post(&server.url).bearer_auth(KEY).json(&json!({ "jsonrpc": "2.0", "id": 5, "method": "ping" })).send().await.unwrap();
        assert_eq!((res.headers()["ratelimit-limit"].to_str().unwrap(), res.headers().contains_key("ratelimit-remaining")), ("10000", true));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn emails_are_found_by_name_for_the_keys_organization(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        let server = Server::start(db.clone()).await;
        let jane = json!({ "project_id": project, "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example" });

        let (is_error, text) = server.call(KEY, "find_email", jane.clone()).await;
        assert!(is_error && text.contains("email finder or verifier"), "{text}");
        for p in [project, other_project] {
            let connect = "insert into project_settings (project_id, verifier, verifier_key_enc) values ($1, 'millionverifier', '')";
            sqlx::query(connect).bind(p).execute(&db).await.unwrap();
        }
        sqlx::query("insert into email_search_attempts (org_id) select org_id from projects, generate_series(1, 200) where id = $1")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();
        let (is_error, text) = server.call(KEY, "find_email", jane.clone()).await;
        assert!(is_error && text.contains("200 email searches"), "{text}");

        let people = json!([{ "first_name": "Jane", "company_domain": "acme.example", "client_ref": "crm-1" }]);
        let (is_error, text) = server.call(KEY, "find_emails", json!({ "project_id": project, "people": people })).await;
        assert!(!is_error, "{text}");
        let started: Value = serde_json::from_str(&text).unwrap();
        assert!(started["next"].as_str().unwrap().contains("get_email_batch"), "{started}");
        let (is_error, text) = server.call(KEY, "get_email_batch", json!({ "task_id": started["task_id"] })).await;
        let batch: Value = serde_json::from_str(&text).unwrap();
        assert!(!is_error && batch["status"] == "pending" && batch["results"][0]["client_ref"] == "crm-1", "{text}");

        let (is_error, text) = server.call(KEY, "find_emails", json!({ "project_id": other_project, "people": people })).await;
        assert!(is_error && text.contains("Not found"), "{text}");
        let (is_error, _) = server.call(KEY, "get_email_batch", json!({ "task_id": Uuid::new_v4() })).await;
        assert!(is_error);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn leads_and_companies_filter_and_sort_by_score(db: PgPool) {
        let [(project, segment), (other_project, _)] = seed(&db).await;
        let reasons = json!([{ "text": "Hiring 2 SDRs", "kind": "intent" }]);
        for (p, name, score, age) in [(project, "Ana", 40, 3), (project, "Bo", 90, 2), (project, "Cy", 10, 1), (other_project, "Di", 95, 0)] {
            sqlx::query(
                "insert into leads (project_id, full_name, email, score, score_fit, score_intent, score_reasons, created_at)
                 values ($1, $2, $2 || '@x.com', $3, $3 / 2, $3 - $3 / 2, $4, now() - make_interval(days => $5))",
            )
            .bind(p)
            .bind(name)
            .bind(score as i16)
            .bind(&reasons)
            .bind(age)
            .execute(&db)
            .await
            .unwrap();
        }
        for (domain, score) in [("low.com", 5), ("high.com", 80)] {
            sqlx::query("insert into companies (project_id, segment_id, domain, score) values ($1, $2, $3, $4)")
                .bind(project)
                .bind(segment)
                .bind(domain)
                .bind(score as i16)
                .execute(&db)
                .await
                .unwrap();
        }
        let server = Server::start(db.clone()).await;
        let names = |text: &str, key: &str| -> Vec<String> {
            let page: Value = serde_json::from_str(text).unwrap();
            page["items"].as_array().unwrap().iter().map(|l| l[key].as_str().unwrap().to_string()).collect()
        };

        let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "sort": "score", "min_score": 20 })).await;
        assert!(!is_error, "{text}");
        assert_eq!(names(&text, "full_name"), ["Bo", "Ana"]);
        let first: Value = serde_json::from_str::<Value>(&text).unwrap()["items"][0].clone();
        assert_eq!((first["score"].clone(), first["score_fit"].clone(), first["score_reasons"].clone()), (json!(90), json!(45), reasons.clone()));
        let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project })).await;
        assert_eq!(names(&text, "full_name"), ["Cy", "Bo", "Ana"], "newest first by default");
        let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "min_score": 101 })).await;
        assert!(is_error && text.contains("0 to 100"), "{text}");
        let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "sort": "vibes" })).await;
        assert!(is_error && text.contains("Sort by score"), "{text}");

        let (is_error, text) = server.call(KEY, "list_companies", json!({ "project_id": project, "sort": "score" })).await;
        assert!(!is_error, "{text}");
        assert_eq!(names(&text, "domain"), ["high.com", "low.com"]);
        let (_, text) = server.call(KEY, "list_companies", json!({ "project_id": project, "min_score": 50 })).await;
        assert_eq!(names(&text, "domain"), ["high.com"]);

        // The REST API takes the same filters, and another organization's project is not found.
        let api = server.url.replace("/mcp", "/v1");
        let get = |path: String| server.client.get(format!("{api}{path}")).bearer_auth(KEY).send();
        let res = get(format!("/projects/{project}/leads?min_score=50&sort=score")).await.unwrap();
        let page: Value = res.json().await.unwrap();
        assert_eq!((page["total"].clone(), page["items"][0]["full_name"].clone()), (json!(1), json!("Bo")));
        let res = get(format!("/projects/{project}/companies?sort=score&min_score=1")).await.unwrap();
        assert_eq!(res.json::<Value>().await.unwrap()["total"], 2);
        for path in [format!("/projects/{other_project}/leads?sort=score"), format!("/projects/{other_project}/companies")] {
            assert_eq!(get(path).await.unwrap().status(), 404);
        }
        let (is_error, _) = server.call(KEY, "list_leads", json!({ "project_id": other_project, "sort": "score" })).await;
        assert!(is_error);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn lookalikes_rank_and_join_a_campaign_for_the_keys_organization(db: PgPool) {
        let [(project, segment), (other_project, other_segment)] = seed(&db).await;
        let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
        crate::lookalikes::tests::customers(&db, org, project).await;
        let server = Server::start(db.clone()).await;

        let (is_error, text) = server.call(KEY, "find_lookalikes", json!({ "project_id": project, "limit": 2 })).await;
        assert!(!is_error, "{text}");
        let found: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((found["total"].clone(), found["database_empty"].clone()), (json!(4), json!(false)));
        assert_eq!(found["results"].as_array().unwrap().len(), 2);
        assert_eq!(found["results"][0]["domain"], "ledger.example");
        assert_eq!(found["results"][0]["score"], 93);
        assert_eq!(found["results"][0]["reasons"][2], "Hiring a Head of Finance");
        assert_eq!(found["seeds"].as_array().unwrap().len(), 4);
        assert_eq!(found["profile"][0]["key"], "category");

        // Seeds passed in replace the project's, here and in the API's query string.
        let (_, text) = server.call(KEY, "find_lookalikes", json!({ "project_id": project, "seeds": ["beta.example"] })).await;
        let found: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(found["seeds"], json!([{ "domain": "beta.example", "name": "Beta", "won": true, "in_database": true, "known": true }]));
        let api = server.url.replace("/api/mcp", &format!("/api/v1/projects/{project}/lookalikes?seeds=beta.example,https://www.gamma.example/"));
        let res = server.client.get(&api).bearer_auth(KEY).send().await.unwrap();
        assert_eq!(res.status(), 200);
        let body: Value = res.json().await.unwrap();
        let seeds: Vec<&str> = body["profile"]["seeds"].as_array().unwrap().iter().map(|s| s["domain"].as_str().unwrap()).collect();
        assert_eq!(seeds, ["beta.example", "gamma.example"]);
        let bad = server.url.replace("/api/mcp", &format!("/api/v1/projects/{project}/lookalikes?seeds=not%20a%20domain"));
        assert_eq!(server.client.get(&bad).bearer_auth(KEY).send().await.unwrap().status(), 400);

        let (is_error, text) = server.call(KEY, "add_lookalikes", json!({ "project_id": project, "campaign_id": segment, "top": 1 })).await;
        assert!(!is_error, "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "added": 1, "skipped": 0 }));
        let args = json!({ "project_id": project, "campaign_id": segment, "domains": ["tally.example", "noise.example"] });
        let (_, text) = server.call(KEY, "add_lookalikes", args).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), json!({ "added": 1, "skipped": 1 }), "noise isn't a match");
        let (reason, segment_id): (String, Option<Uuid>) = sqlx::query_as("select fit_reason, segment_id from companies where domain = 'ledger.example' and project_id = $1")
            .bind(project)
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(reason.starts_with("Like your customers: Accounting firm"), "{reason}");
        assert_eq!(segment_id, Some(segment));

        // Another organization's project or campaign is out of reach.
        for (tool, args) in [
            ("find_lookalikes", json!({ "project_id": other_project })),
            ("add_lookalikes", json!({ "project_id": other_project, "campaign_id": other_segment, "top": 1 })),
            ("add_lookalikes", json!({ "project_id": project, "campaign_id": other_segment, "top": 1 })),
        ] {
            let (is_error, text) = server.call(KEY, tool, args).await;
            assert!(is_error, "{tool}: {text}");
        }
        let theirs: i64 = sqlx::query_scalar("select count(*) from companies where project_id = $1").bind(other_project).fetch_one(&db).await.unwrap();
        assert_eq!(theirs, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn research_questions_queue_and_list_answers_by_label(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        let server = Server::start(db.clone()).await;
        sqlx::query("insert into companies (project_id, domain, name) values ($1, 'acme.example', 'Acme')").bind(project).execute(&db).await.unwrap();

        let (is_error, text) = server.call(KEY, "add_research_question", json!({ "project_id": project, "question": "Do they sell to hospitals?" })).await;
        assert!(!is_error, "{text}");
        let question: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((question["label"].clone(), question["answer_type"].clone()), (json!("Do they sell to hospitals"), json!("yes_no")));
        let (is_error, text) = server.call(KEY, "research_companies", json!({ "project_id": project })).await;
        assert!(!is_error, "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["companies"], 1);

        sqlx::query("update research_answers set status = 'answered', answer = 'yes', evidence = 'Lists hospital clients.'").execute(&db).await.unwrap();
        let (is_error, text) = server.call(KEY, "list_research", json!({ "project_id": project, "question_id": question["id"], "answer": "yes" })).await;
        assert!(!is_error, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        let qid = question["id"].as_str().unwrap();
        assert_eq!(
            listed["items"][0]["answers"][qid],
            json!({ "label": "Do they sell to hospitals", "status": "answered", "answer": "yes", "evidence": "Lists hospital clients.", "source_url": "" })
        );

        let (is_error, _) = server.call(KEY, "research_companies", json!({ "project_id": other_project })).await;
        assert!(is_error, "another organization's project");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn company_signals_for_the_keys_organization(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        let server = Server::start(db.clone()).await;
        for (p, domain, kind, key, age) in [
            (project, "acme.example", "funding_filing", "sec:1:021-1", 5),
            (project, "acme.example", "launch", "feed:a", 40),
            (other_project, "theirs.example", "funding_filing", "sec:2:021-2", 1),
        ] {
            sqlx::query(
                "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, url)
                 values ($1, $2, $3, 'sec', $4, current_date - $5::int, 'Acme Robotics, Inc.', 12000000, 'https://www.sec.gov/' || $4)",
            )
            .bind(p)
            .bind(domain)
            .bind(kind)
            .bind(key)
            .bind(age)
            .execute(&db)
            .await
            .unwrap();
        }

        let (is_error, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "days": 30 })).await;
        assert!(!is_error, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(listed.as_array().unwrap().len(), 1);
        assert_eq!((listed[0]["kind"].clone(), listed[0]["amount"].clone()), (json!("funding_filing"), json!(12000000)));
        assert!(listed[0]["headline"].as_str().unwrap().starts_with("Raised $12M (SEC filing, "), "{text}");
        assert!(listed[0].get("id").is_none(), "only the picked fields");
        let (_, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "kind": "launch" })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()[0]["kind"], "launch");
        let (is_error, text) = server.call(KEY, "list_company_signals", json!({ "project_id": project, "kind": "gossip" })).await;
        assert!(is_error && text.contains("kind must be one of"), "{text}");

        let api = server.url.replace("/mcp", "/v1");
        let res = server.client.get(format!("{api}/projects/{project}/signals")).bearer_auth(KEY).send().await.unwrap();
        let all: Value = res.json().await.unwrap();
        assert_eq!(all.as_array().unwrap().len(), 2);
        let res = server.client.get(format!("{api}/projects/{other_project}/signals")).bearer_auth(KEY).send().await.unwrap();
        assert_eq!(res.status(), 404);
        let (is_error, _) = server.call(KEY, "list_company_signals", json!({ "project_id": other_project })).await;
        assert!(is_error);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn job_changes_and_the_watch_list_for_the_keys_organization(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        let server = Server::start(db.clone()).await;

        let people = json!([{ "name": "Dana Reyes", "email": "dana@acme.example", "linkedin_url": "https://www.linkedin.com/in/dana-reyes", "company": "Acme" },
                            { "name": "No Way" }]);
        let (is_error, text) = server.call(KEY, "add_to_watch_list", json!({ "project_id": project, "people": people })).await;
        assert!(!is_error, "{text}");
        let added: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((added["added"].clone(), added["errors"][0]["error"].clone()), (json!(1), json!("Needs an email or a LinkedIn link")));
        let watched: Uuid = sqlx::query_scalar("select id from watched_people where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        sqlx::query(
            "insert into job_changes (project_id, watched_id, old_company, new_company, new_domain, move_number, new_title)
             values ($1, $2, 'Acme', 'Brightline', 'brightline.example', 1, 'VP Sales')",
        )
        .bind(project)
        .bind(watched)
        .execute(&db)
        .await
        .unwrap();

        let (is_error, text) = server.call(KEY, "list_job_changes", json!({ "project_id": project })).await;
        assert!(!is_error, "{text}");
        let listed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((listed["connected"].clone(), listed["watching"].clone()), (json!(false), json!(1)));
        let change = &listed["items"][0];
        assert_eq!((change["full_name"].clone(), change["old_company"].clone(), change["new_company"].clone(), change["relationship"].clone()),
                   (json!("Dana Reyes"), json!("Acme"), json!("Brightline"), json!("won")));
        assert!(change.get("old_lead_id").is_none(), "only the picked fields");

        // The REST API takes a CSV too, and another organization's project is not found either way.
        let api = server.url.replace("/mcp", "/v1");
        let csv = "Name,Email,LinkedIn,Company\nBo Chen,bo@bright.example,,Brightline\n";
        let res = server.client.post(format!("{api}/projects/{project}/watch-list")).bearer_auth(KEY).json(&json!({ "relationship": "booked", "csv": csv }));
        let added: Value = res.send().await.unwrap().json().await.unwrap();
        assert_eq!(added["added"], 1);
        let res = server.client.get(format!("{api}/projects/{project}/job-changes")).bearer_auth(KEY).send().await.unwrap();
        let overview: Value = res.json().await.unwrap();
        assert_eq!((overview["watching"].clone(), overview["changes"][0]["new_title"].clone()), (json!(2), json!("VP Sales")));
        for res in [
            server.client.get(format!("{api}/projects/{other_project}/job-changes")).bearer_auth(KEY).send().await.unwrap(),
            server.client.post(format!("{api}/projects/{other_project}/watch-list")).bearer_auth(KEY).json(&json!({ "csv": csv })).send().await.unwrap(),
        ] {
            assert_eq!(res.status(), 404);
        }
        let (is_error, _) = server.call(KEY, "list_job_changes", json!({ "project_id": other_project })).await;
        assert!(is_error);
        let theirs: i64 = sqlx::query_scalar("select count(*) from watched_people where project_id = $1").bind(other_project).fetch_one(&db).await.unwrap();
        assert_eq!(theirs, 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn leads_are_assigned_and_filtered_by_owner_for_the_keys_organization(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        let org_of = |p: Uuid| sqlx::query_scalar::<_, Uuid>("select org_id from projects where id = $1").bind(p).fetch_one(&db);
        let (acme, other) = (org_of(project).await.unwrap(), org_of(other_project).await.unwrap());
        let mut people = vec![];
        for (org, email) in [(acme, "ana@acme.test"), (acme, "ben@acme.test"), (other, "eve@other.test")] {
            let id: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id").bind(email).fetch_one(&db).await.unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'member')").bind(org).bind(id).execute(&db).await.unwrap();
            people.push(id);
        }
        let lead = |project: Uuid, name: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name) values ($1, $2) returning id").bind(project).bind(name).fetch_one(&db)
        };
        let (cy, dee, theirs) = (lead(project, "Cy").await.unwrap(), lead(project, "Dee").await.unwrap(), lead(other_project, "Their Lead").await.unwrap());
        let server = Server::start(db.clone()).await;

        let (is_error, text) = server.call(KEY, "assign_lead", json!({ "lead_id": cy, "owner": "Ana@acme.test" })).await;
        assert!(!is_error, "{text}");
        let assigned: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((assigned["owner_id"].clone(), assigned["owner_email"].clone()), (json!(people[0]), json!("ana@acme.test")));
        let (is_error, text) = server.call(KEY, "assign_lead", json!({ "lead_id": dee, "owner": "eve@other.test" })).await;
        assert!(is_error && text.contains("Nobody on your team"), "{text}");
        let (is_error, _) = server.call(KEY, "assign_lead", json!({ "lead_id": dee, "owner": people[2] })).await;
        assert!(is_error, "Eve's id is no better than her email");
        let (is_error, _) = server.call(KEY, "assign_lead", json!({ "lead_id": theirs, "owner": "ana@acme.test" })).await;
        assert!(is_error, "the other organization's lead");

        let names = |text: &str| -> Vec<String> {
            let page: Value = serde_json::from_str(text).unwrap();
            page["items"].as_array().unwrap().iter().map(|l| l["full_name"].as_str().unwrap().to_string()).collect()
        };
        let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "ana@acme.test" })).await;
        assert_eq!(names(&text), ["Cy"]);
        assert!(text.contains("\"owner_email\":\"ana@acme.test\""), "{text}");
        let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "unassigned" })).await;
        assert_eq!(names(&text), ["Dee"]);
        let (is_error, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "owner": "mine" })).await;
        assert!(is_error && text.contains("API key"), "{text}");

        // The REST API: assign, take back with null, and filter by id.
        let api = server.url.replace("/mcp", "/v1");
        let res = server.client.post(format!("{api}/leads/{dee}/assign")).bearer_auth(KEY).json(&json!({ "owner": people[1] })).send().await.unwrap();
        assert_eq!(res.status(), 200);
        assert_eq!(res.json::<Value>().await.unwrap()["owner_email"], "ben@acme.test");
        let page: Value = server.client.get(format!("{api}/projects/{project}/leads?owner={}", people[1])).bearer_auth(KEY).send().await.unwrap().json().await.unwrap();
        assert_eq!(page["items"][0]["full_name"], "Dee");
        let res = server.client.post(format!("{api}/leads/{cy}/assign")).bearer_auth(KEY).json(&json!({ "owner": null })).send().await.unwrap();
        assert_eq!(res.json::<Value>().await.unwrap()["owner_id"], Value::Null);
        let res = server.client.post(format!("{api}/leads/{theirs}/assign")).bearer_auth(KEY).json(&json!({ "owner": null })).send().await.unwrap();
        assert_eq!(res.status(), 404);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn language_on_leads_and_campaigns_through_the_api_and_mcp(db: PgPool) {
        let [(project, segment), (other_project, other_segment)] = seed(&db).await;
        let lead = |project: Uuid, email: &'static str, country: &'static str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, full_name, email, country_code) values ($1, $2, $2, $3) returning id")
                .bind(project)
                .bind(email)
                .bind(country)
        };
        let anna = lead(project, "anna@brauhaus.example", "DE").fetch_one(&db).await.unwrap();
        let olaf = lead(other_project, "olaf@fjord.example", "NO").fetch_one(&db).await.unwrap();
        let server = Server::start(db.clone()).await;
        let api = server.url.trim_end_matches("/mcp").to_string();
        let send = |method: reqwest::Method, path: String, body: Value| {
            let request = server.client.request(method, format!("{api}{path}")).bearer_auth(KEY).json(&body);
            async move {
                let res = request.send().await.unwrap();
                (res.status().as_u16(), res.json::<Value>().await.unwrap_or(Value::Null))
            }
        };

        // API v1: a lead added with a language by name keeps its code, and the list filters by the language Splee writes them in.
        let (status, body) = send(reqwest::Method::POST, format!("/v1/projects/{project}/leads"), json!({ "leads": [{ "email": "camille@chocolat.example", "language": "French" }] })).await;
        assert_eq!((status, body["imported"].as_i64()), (200, Some(1)), "{body}");
        let (_, page) = send(reqwest::Method::GET, format!("/v1/projects/{project}/leads?language=fr"), Value::Null).await;
        let items = page["items"].as_array().unwrap();
        assert_eq!(items.len(), 1, "{page}");
        assert_eq!((items[0]["language"].as_str(), items[0]["effective_language"].as_str(), items[0]["effective_language_source"].as_str()), (Some("fr"), Some("fr"), Some("set")));
        let (_, page) = send(reqwest::Method::GET, format!("/v1/projects/{project}/leads?language=de"), Value::Null).await;
        assert_eq!(page["items"][0]["id"], json!(anna), "{page}");
        assert_eq!(page["total"], 1);

        let (status, detail) = send(reqwest::Method::PATCH, format!("/v1/leads/{anna}"), json!({ "language": "es" })).await;
        assert_eq!((status, detail["language"].as_str(), detail["effective_language"].as_str()), (200, Some("es"), Some("es")), "{detail}");
        let (status, body) = send(reqwest::Method::PATCH, format!("/v1/leads/{anna}"), json!({ "language": "Klingon" })).await;
        assert_eq!(status, 400, "{body}");
        assert_eq!(send(reqwest::Method::PATCH, format!("/v1/leads/{olaf}"), json!({ "language": "de" })).await.0, 404);

        // New campaigns write in each lead's language unless told otherwise, like projects did before campaigns had a language.
        let (_, created) = send(reqwest::Method::POST, format!("/v1/projects/{project}/campaigns"), json!({ "prompt": "Breweries in Munich" })).await;
        let (is_error, text) = server.call(KEY, "create_campaign", json!({ "project_id": project, "prompt": "Bakeries in Lyon" })).await;
        assert!(!is_error, "{text}");
        let from_mcp = serde_json::from_str::<Value>(&text).unwrap()["campaign_id"].clone();
        let new: Vec<String> = sqlx::query_scalar("select language from segments where id::text = any($1) order by name")
            .bind(vec![created["campaign_id"].as_str().unwrap(), from_mcp.as_str().unwrap()])
            .fetch_all(&db)
            .await
            .unwrap();
        assert_eq!(new, ["lead", "lead"]);

        let (status, campaign) = send(reqwest::Method::PATCH, format!("/v1/campaigns/{segment}"), json!({ "language": "fr" })).await;
        assert_eq!((status, campaign["language"].as_str()), (200, Some("fr")), "{campaign}");
        assert_eq!(send(reqwest::Method::PATCH, format!("/v1/campaigns/{segment}"), json!({ "language": "xx" })).await.0, 400);
        assert_eq!(send(reqwest::Method::PATCH, format!("/v1/campaigns/{other_segment}"), json!({ "language": "de" })).await.0, 404);
        let (_, campaigns) = send(reqwest::Method::GET, format!("/v1/projects/{project}/campaigns"), Value::Null).await;
        let ours = campaigns.as_array().unwrap().iter().find(|c| c["id"] == json!(segment)).unwrap();
        assert_eq!(ours["language"], "fr");

        // MCP: clearing Anna's language takes her back to her country's, and the other organization's campaign is out of reach.
        let (is_error, text) = server.call(KEY, "set_lead_language", json!({ "lead_id": anna, "language": "" })).await;
        let set: Value = serde_json::from_str(&text).unwrap();
        assert!(!is_error, "{text}");
        assert_eq!((set["language"].as_str(), set["effective_language"].as_str(), set["effective_language_source"].as_str()), (Some(""), Some("de"), Some("country")));
        let (is_error, text) = server.call(KEY, "set_campaign_language", json!({ "campaign_id": segment, "language": "nl" })).await;
        assert!(!is_error && serde_json::from_str::<Value>(&text).unwrap()["language"] == "nl", "{text}");
        let (is_error, _) = server.call(KEY, "set_campaign_language", json!({ "campaign_id": other_segment, "language": "nl" })).await;
        assert!(is_error);
        let (is_error, text) = server.call(KEY, "update_campaign", json!({ "campaign_id": segment, "angle": "Fewer no-shows" })).await;
        let updated: Value = serde_json::from_str(&text).unwrap();
        assert!(!is_error && updated["angle"] == "Fewer no-shows" && updated["language"] == "nl", "{text}");
        let (is_error, _) = server.call(KEY, "set_lead_language", json!({ "lead_id": olaf, "language": "de" })).await;
        assert!(is_error);
        let (_, text) = server.call(KEY, "list_leads", json!({ "project_id": project, "language": "de" })).await;
        let page: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((page["total"].as_i64(), page["items"][0]["effective_language"].as_str()), (Some(1), Some("de")), "{text}");
        let (_, text) = server.call(KEY, "list_campaigns", json!({ "project_id": project })).await;
        let campaigns: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(campaigns.as_array().unwrap().iter().find(|c| c["id"] == json!(segment)).unwrap()["language"], "nl", "{text}");
        let other: String = sqlx::query_scalar("select language from segments where id = $1").bind(other_segment).fetch_one(&db).await.unwrap();
        assert_eq!(other, "lead");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn downloading_companies_answers_the_first_rows_and_a_link_to_the_rest(db: PgPool) {
        seed(&db).await;
        for (domain, name, confidence) in [("smile.co.uk", "Smile Dental", 0.9), ("grin.co.uk", "=Grin Dental", 0.8), ("cafe.co.uk", "Leeds Cafe", 0.7)] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, city, country_code, category, categories, confidence, source_id, release)
                 values ($1, $2, 'https://' || $1, 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', $3, 'x', 'r')",
            )
            .bind(domain)
            .bind(name)
            .bind(confidence)
            .execute(&db)
            .await
            .unwrap();
        }
        let server = Server::start(db).await;

        let (is_error, text) = server.call(KEY, "download_companies", json!({ "q": "dental", "city": "Leeds", "has_email": false, "per_page": 1 })).await;
        assert!(!is_error, "{text}");
        let found: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((found["rows"].clone(), found["more"].clone()), (json!(1), json!(true)));
        assert_eq!(found["csv"].as_str().unwrap().lines().nth(1).unwrap(), "Smile Dental,smile.co.uk,https://smile.co.uk,,,,Leeds,,,GB,dental_clinic,0.90,x,r");
        assert_eq!(found["download_url"], "http://localhost:3346/api/v1/companies/export?q=dental&city=Leeds");

        let (_, text) = server.call(KEY, "download_companies", json!({ "domains": ["grin.co.uk", "cafe.co.uk"], "category": "dental_clinic" })).await;
        let found: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((found["rows"].clone(), found["more"].clone()), (json!(2), json!(false)));
        assert!(found["csv"].as_str().unwrap().contains("\n'=Grin Dental,grin.co.uk,"), "{text}");
        assert_eq!(found["download_url"], "http://localhost:3346/api/v1/companies/export?category=dental_clinic&domains=grin.co.uk%2Ccafe.co.uk");
        assert!(!super::read_tools().iter().any(|t| t["name"] == "download_companies"), "Ask Splee doesn't spend the downloads");
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn dedup_lists_hide_companies_from_search_companies(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        for domain in ["smile.co.uk", "grin.co.uk"] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, city, country_code, category, categories, source_id, release)
                 values ($1, $1, 'https://' || $1, 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', 'x', 'r')",
            )
            .bind(domain)
            .execute(&db)
            .await
            .unwrap();
        }
        let server = &Server::start(db).await;
        let total = |args: Value| async move {
            let (is_error, text) = server.call(KEY, "search_companies", args).await;
            assert!(!is_error, "{text}");
            serde_json::from_str::<Value>(&text).unwrap()["total"].clone()
        };

        let args = json!({ "name": "CRM accounts", "domains": ["https://www.Smile.co.uk/"], "project_ids": [other_project] });
        assert!(server.call(KEY, "create_company_dedup_list", args).await.0, "another org's project");
        let args = json!({ "name": "CRM accounts", "domains": ["https://www.Smile.co.uk/"], "project_ids": [project] });
        let (is_error, text) = server.call(KEY, "create_company_dedup_list", args).await;
        assert!(!is_error, "{text}");
        let list_id = serde_json::from_str::<Value>(&text).unwrap()["list"]["id"].clone();
        assert_eq!(total(json!({ "category": "dental_clinic", "project_id": project })).await, json!(1));
        assert_eq!(total(json!({ "category": "dental_clinic", "project_id": project, "include_hidden": true })).await, json!(2));
        assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(2), "a list for one project");

        let (_, text) = server.call(KEY, "set_company_dedup_list_projects", json!({ "list_id": list_id, "project_ids": null })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["project_ids"], Value::Null, "{text}");
        assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(1));
        let (_, text) = server.call(KEY, "get_company_dedup_list", json!({ "list_id": list_id })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["domains"], json!(["smile.co.uk"]), "{text}");
        let (_, text) = server.call(KEY, "list_company_dedup_lists", json!({})).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()[0]["size"], json!(1), "{text}");
        let (is_error, _) = server.call(KEY, "delete_company_dedup_list", json!({ "list_id": list_id })).await;
        assert!(!is_error);
        assert_eq!(total(json!({ "category": "dental_clinic" })).await, json!(2));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn tools_act_for_the_keys_organization_only(db: PgPool) {
        let [(project, segment), (other_project, other_segment)] = seed(&db).await;
        sqlx::query(
            "insert into directory_companies (domain, name, website, city, country_code, category, source_id, release)
             values ('smile.co.uk', 'Smile Dental', 'https://smile.co.uk', 'Leeds', 'GB', 'dental_clinic', 'x', 'r')",
        )
        .execute(&db)
        .await
        .unwrap();
        let other_email: Uuid = sqlx::query_scalar(
            "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Bo', 'bo@x.com') returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
             select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l returning id",
        )
        .bind(other_project)
        .bind(other_segment)
        .fetch_one(&db)
        .await
        .unwrap();
        let server = Server::start(db.clone()).await;

        let (is_error, text) = server.call(KEY, "list_projects", json!({})).await;
        assert!(!is_error);
        assert!(text.contains("Acme project") && !text.contains("Other project"), "{text}");

        let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "city": "leeds" })).await;
        assert!(!is_error && text.contains("smile.co.uk"), "{text}");
        // A scanned company carries its tools, and the tech filter takes a tool or a category.
        sqlx::query(
            "insert into directory_companies (domain, name, website, city, country_code, category, source_id, release)
             values ('grin.co.uk', 'Grin Dental', 'https://grin.co.uk', 'Leeds', 'GB', 'dental_clinic', 'y', 'r')",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into tech_scans (domain, techs, scanned_at) values ('smile.co.uk', '{HubSpot,Cloudflare}', now())").execute(&db).await.unwrap();
        for filter in ["crm", "hubspot"] {
            let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": filter })).await;
            let found: Value = serde_json::from_str(&text).unwrap();
            assert!(!is_error, "{text}");
            assert_eq!((found["total"].clone(), found["items"][0]["tech"].clone()), (json!(1), json!(["HubSpot", "Cloudflare"])), "{filter}");
        }
        let (_, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": "Stripe" })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);
        let (is_error, text) = server.call(KEY, "search_companies", json!({ "q": "dental", "tech": "Nope" })).await;
        assert!(is_error && text.contains("can't detect"), "{text}");
        let (is_error, text) = server.call(KEY, "add_companies", json!({ "campaign_id": segment, "domains": ["smile.co.uk"] })).await;
        assert!(!is_error, "{text}");
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["added"], 1);

        let (is_error, text) = server.call(KEY, "create_campaign", json!({ "project_id": project, "prompt": "Dentists in Leeds" })).await;
        assert!(!is_error, "{text}");
        let run_id = serde_json::from_str::<Value>(&text).unwrap()["run_id"].as_str().unwrap().to_string();
        let (_, text) = server.call(KEY, "get_run", json!({ "run_id": run_id })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["status"], "queued");

        let (is_error, text) = server.call(KEY, "list_campaigns", json!({ "project_id": project })).await;
        assert!(!is_error && text.contains("New campaign"), "{text}");

        // Ids of another organization look like ids that don't exist.
        for (tool, args) in [
            ("list_campaigns", json!({ "project_id": other_project })),
            ("add_companies", json!({ "campaign_id": other_segment, "domains": ["smile.co.uk"] })),
            ("find_people", json!({ "campaign_id": other_segment })),
            ("approve_email", json!({ "email_id": other_email })),
        ] {
            assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
        }
        let status: String = sqlx::query_scalar("select status from email_messages where id = $1").bind(other_email).fetch_one(&db).await.unwrap();
        assert_eq!(status, "draft");
        let (_, text) = server.call(KEY, "list_emails_to_approve", json!({})).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);

        let (is_error, text) = server.call(KEY, "list_campaigns", json!({})).await;
        assert_eq!((is_error, text.as_str()), (true, "Pass project_id as an id"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn deals_are_listed_created_and_moved_for_the_keys_organization(db: PgPool) {
        let [(project, segment), (other_project, _)] = seed(&db).await;
        let lead = |project: Uuid, name: &str| {
            sqlx::query_scalar::<_, Uuid>("insert into leads (project_id, segment_id, full_name, email) values ($1, $2, $3, $4) returning id")
                .bind(project)
                .bind(Some(segment).filter(|_| project != other_project))
                .bind(name.to_string())
                .bind(format!("{name}@x.example"))
                .fetch_one(&db)
        };
        let (sarah, bo) = (lead(project, "sarah").await.unwrap(), lead(other_project, "bo").await.unwrap());
        let other_org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(other_project).fetch_one(&db).await.unwrap();
        crate::deals::ensure_stages(&db, other_org).await.unwrap();
        let their_deal: Uuid = sqlx::query_scalar(
            "insert into deals (org_id, project_id, lead_id, stage_id) select $1, $2, $3, id from deal_stages where org_id = $1 and name = 'Proposal' returning id",
        )
        .bind(other_org)
        .bind(other_project)
        .bind(bo)
        .fetch_one(&db)
        .await
        .unwrap();
        let server = Server::start(db.clone()).await;

        let (is_error, text) = server.call(KEY, "create_deal", json!({ "lead_id": sarah, "value": 12000, "close_date": "2026-12-01" })).await;
        assert!(!is_error, "{text}");
        let deal: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((deal["stage"].clone(), deal["value"].clone(), deal["currency"].clone()), (json!("Interested"), json!(12000.0), json!("USD")));
        let (is_error, text) = server.call(KEY, "create_deal", json!({ "lead_id": sarah })).await;
        assert!(is_error && text.contains("already has an open deal"), "{text}");

        let (is_error, text) = server.call(KEY, "move_deal", json!({ "deal_id": deal["id"], "stage": "won", "value": 15000 })).await;
        assert!(!is_error, "{text}");
        let won: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((won["status"].clone(), won["value"].clone()), (json!("won"), json!(15000.0)));

        let (_, text) = server.call(KEY, "list_deals", json!({ "status": "won" })).await;
        let listed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((listed["total"].clone(), listed["items"][0]["lead_name"].clone()), (json!(1), json!("sarah")));
        let stages: Vec<&str> = listed["stages"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(stages, ["Interested", "Meeting booked", "Proposal", "Negotiation", "Won", "Lost"]);
        let (_, text) = server.call(KEY, "list_deals", json!({ "status": "open" })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["total"], 0);

        // The other organization's deal, lead and project look like ids that don't exist.
        for (tool, args) in [
            ("move_deal", json!({ "deal_id": their_deal, "stage": "Won" })),
            ("create_deal", json!({ "lead_id": bo })),
            ("list_deals", json!({ "project_id": other_project })),
        ] {
            assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
        }
        let (stage, status): (String, String) =
            sqlx::query_as("select s.name, d.status from deals d join deal_stages s on s.id = d.stage_id where d.id = $1").bind(their_deal).fetch_one(&db).await.unwrap();
        assert_eq!((stage.as_str(), status.as_str()), ("Proposal", "open"));

        // The same through the REST API.
        let api = server.url.replace("/api/mcp", "/api/v1");
        let res = server.client.get(format!("{api}/deals?project_id={project}")).bearer_auth(KEY).send().await.unwrap();
        let page: Value = res.json().await.unwrap();
        assert_eq!((page["total"].clone(), page["items"][0]["stage"].clone()), (json!(1), json!("Won")));
        let res = server.client.post(format!("{api}/deals/{}/move", deal["id"].as_str().unwrap())).bearer_auth(KEY);
        let moved: Value = res.json(&json!({ "stage": "Lost", "lost_reason": "No budget" })).send().await.unwrap().json().await.unwrap();
        assert_eq!((moved["status"].clone(), moved["lost_reason"].clone()), (json!("lost"), json!("No budget")));
        let res = server.client.get(format!("{api}/deal-stages")).bearer_auth(KEY).send().await.unwrap();
        assert_eq!(res.json::<Value>().await.unwrap().as_array().unwrap().len(), 6);
        let res = server.client.get(format!("{api}/deals")).bearer_auth("splee_wrong").send().await.unwrap();
        assert_eq!(res.status(), 401);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_wrong_key_is_refused(db: PgPool) {
        seed(&db).await;
        let server = Server::start(db).await;
        let (status, body) = server.post("splee_wrong", json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await;
        assert_eq!(status, 401);
        assert!(body["error"]["message"].as_str().unwrap().contains("API key"));
        let res = server.client.post(&server.url).json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).send().await.unwrap();
        assert_eq!(res.status(), 401);
        assert_eq!(res.headers()["www-authenticate"], "Bearer resource_metadata=\"http://localhost:3346/.well-known/oauth-protected-resource/api/mcp\"");
        let res = server.client.post(&server.url).bearer_auth("splee_wrong").json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" })).send().await.unwrap();
        assert!(res.headers()["www-authenticate"].to_str().unwrap().starts_with("Bearer error=\"invalid_token\""));
    }

    /// A 2026-07-28 request with the headers that mirror its body.
    async fn stateless(server: &Server, method: &str, name: Option<&str>, mut params: Value) -> (u16, Value) {
        params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} });
        let body = json!({ "jsonrpc": "2.0", "id": 9, "method": method, "params": params });
        let mut req = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").header("mcp-method", method);
        if let Some(name) = name {
            req = req.header("mcp-name", name);
        }
        let res = req.json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or(Value::Null))
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_stateless_revision_works_without_initialize(db: PgPool) {
        seed(&db).await;
        let server = Server::start(db).await;

        let (status, body) = stateless(&server, "server/discover", None, json!({})).await;
        assert_eq!(status, 200, "{body}");
        let result = &body["result"];
        assert_eq!(result["resultType"], "complete");
        assert_eq!(result["supportedVersions"][0], "2026-07-28");
        assert!(result["capabilities"]["tools"].is_object() && result["instructions"].is_string());
        assert_eq!(result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "splee");

        let (_, body) = stateless(&server, "tools/list", None, json!({})).await;
        assert!(body["result"]["tools"].as_array().unwrap().len() >= 12);
        assert_eq!((body["result"]["cacheScope"].as_str(), body["result"]["ttlMs"].is_u64()), (Some("private"), true));

        let (status, body) = stateless(&server, "tools/call", Some("list_projects"), json!({ "name": "list_projects", "arguments": {} })).await;
        assert_eq!(status, 200);
        assert_eq!(body["result"]["resultType"], "complete");
        assert!(body["result"]["content"][0]["text"].as_str().unwrap().contains("Acme project"));
        // Mcp-Name can carry a name in base64.
        let encoded = "=?base64?bGlzdF9wcm9qZWN0cw==?=";
        let (status, _) = stateless(&server, "tools/call", Some(encoded), json!({ "name": "list_projects", "arguments": {} })).await;
        assert_eq!(status, 200);

        // The headers have to match the body.
        let (status, body) = stateless(&server, "tools/call", Some("list_leads"), json!({ "name": "list_projects", "arguments": {} })).await;
        assert_eq!((status, body["error"]["code"].clone()), (400, json!(-32020)));
        let (status, body) = stateless(&server, "tools/call", None, json!({ "name": "list_projects", "arguments": {} })).await;
        assert_eq!((status, body["error"]["code"].clone()), (400, json!(-32020)));
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list",
            "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28", "io.modelcontextprotocol/clientCapabilities": {} } } });
        let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").json(&request).send().await.unwrap();
        assert_eq!(res.status(), 400);
        assert_eq!(res.json::<Value>().await.unwrap()["error"]["code"], -32020);

        // Every request has to say what the client can do, and the handshake's methods are gone.
        let request = json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2026-07-28" } } });
        let res = server.client.post(&server.url).bearer_auth(KEY).header("mcp-protocol-version", "2026-07-28").header("mcp-method", "tools/list");
        let res = res.json(&request).send().await.unwrap();
        assert_eq!((res.status().as_u16(), res.json::<Value>().await.unwrap()["error"]["code"].clone()), (400, json!(-32602)));
        let (status, body) = stateless(&server, "ping", None, json!({})).await;
        assert_eq!((status, body["error"]["code"].clone()), (404, json!(-32601)));

        // initialize still starts a legacy session, whatever header comes with it.
        let init = json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": { "protocolVersion": "2026-07-28", "capabilities": {} } });
        let (status, body) = server.post(KEY, init).await;
        assert_eq!((status, body["result"]["protocolVersion"].as_str()), (200, Some("2025-11-25")));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn local_business_tools_and_api_use_the_keys_organization_only(db: PgPool) {
        let [(project, segment), (other_project, other_segment)] = seed(&db).await;
        let (places_url, google) = crate::places::fake::start().await;
        let (sites_url, sites) = crate::site_contacts::fake::start().await;
        google.places.lock().unwrap().push(crate::places::fake::place("smile", "Smile Dental", "https://smile.example", 4.8, 90));
        sites.page("smile.example", "/", "Ask for Ana: ana.ruiz@smile.example");
        let server = Server::with(db.clone(), places_url, sites_url).await;

        let (is_error, text) = server.call(KEY, "search_local_businesses", json!({ "what": "dentists", "where": "Austin, TX" })).await;
        assert_eq!((is_error, text.as_str()), (true, "Connect a Google Maps key under Integrations first"));
        sqlx::query("insert into places_keys (org_id, key_enc) select org_id, $1 from projects where id = $2")
            .bind(crate::crypto::encrypt("", "good"))
            .bind(project)
            .execute(&db)
            .await
            .unwrap();

        let (is_error, text) = server.call(KEY, "search_local_businesses", json!({ "what": "dentists", "where": "Austin, TX", "project_id": project })).await;
        assert!(!is_error, "{text}");
        let found: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((found["items"][0]["place_id"].as_str(), found["items"][0]["known"].clone()), (Some("smile"), Value::Null));
        assert!(found["items"][0].get("lat").is_none(), "the tool keeps its answer short");
        let api = server.url.replace("/mcp", "/v1");
        let res = server.client.get(format!("{api}/local-businesses?what=dentists&where=Austin,%20TX")).bearer_auth(KEY).send().await.unwrap();
        let body: Value = res.json().await.unwrap();
        assert_eq!((body["items"][0]["lat"].as_f64(), body["calls"].as_i64()), (Some(30.2672), Some(0)), "the API answers in full, from the cache");
        assert_eq!(google.calls(), 1);

        for (tool, args) in [
            ("search_local_businesses", json!({ "what": "dentists", "where": "Austin", "project_id": other_project })),
            ("add_local_businesses", json!({ "campaign_id": other_segment, "place_ids": ["smile"] })),
        ] {
            assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
        }
        let res = server.client.post(format!("{api}/segments/{segment}/local-businesses")).bearer_auth(KEY).json(&json!({ "place_ids": ["smile"] }));
        let added: Value = res.send().await.unwrap().json().await.unwrap();
        assert_eq!((added["added"][0]["email"].as_str(), added["added"][0]["contact_name"].as_str()), (Some("ana.ruiz@smile.example"), Some("Ana Ruiz")));
        let (_, text) = server.call(KEY, "add_local_businesses", json!({ "campaign_id": segment, "place_ids": ["smile"] })).await;
        assert_eq!(serde_json::from_str::<Value>(&text).unwrap()["skipped"][0]["reason"], "Already in this project");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn campaigns_run_and_replies_follow_through_mcp(db: PgPool) {
        let [(project, segment), (other_project, other_segment)] = seed(&db).await;
        let lead: Uuid = sqlx::query_scalar(
            "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id),
             e as (insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
                   select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l)
             select id from l",
        )
        .bind(project)
        .bind(segment)
        .fetch_one(&db)
        .await
        .unwrap();
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, body, status, classification, classification_reason, created_at)
             values ($1, $2, 'inbound', 'inbound', 'Can we talk Tuesday?', 'received', 'hot', 'Wants a call', now() - interval '1 minute')",
        )
        .bind(project)
        .bind(lead)
        .execute(&db)
        .await
        .unwrap();
        let server = Server::start(db.clone()).await;
        let status = |id: Uuid| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, String>("select status from segments where id = $1").bind(id).fetch_one(&db).await.unwrap() }
        };
        let parsed = |text: &str| serde_json::from_str::<Value>(text).unwrap();

        let (is_error, text) = server.call(KEY, "get_campaign", json!({ "campaign_id": segment })).await;
        assert!(!is_error && parsed(&text)["status"] == "draft" && parsed(&text)["schedule"].is_object(), "{text}");
        let (is_error, text) = server.call(KEY, "approve_campaign", json!({ "campaign_id": segment })).await;
        assert_eq!((is_error, parsed(&text)), (false, json!({ "accepted": true, "approved": 1 })), "{text}");
        assert_eq!(server.call(KEY, "start_campaign", json!({ "campaign_id": segment })).await, (false, r#"{"accepted":true}"#.into()));
        assert_eq!(status(segment).await, "working");
        assert_eq!(server.call(KEY, "stop_campaign", json!({ "campaign_id": segment })).await, (false, r#"{"accepted":true}"#.into()));
        assert_eq!(status(segment).await, "paused");
        let audited: i64 = sqlx::query_scalar("select count(*) from audit_events where action in ('campaign.launched', 'campaign.paused') and actor_key_id is not null")
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(audited, 2, "the audit log names the key");

        // Archiving takes it off list_campaigns until it's unarchived, which leaves it paused.
        let srv = &server;
        let listed = move |archived: bool| async move {
            let (_, text) = srv.call(KEY, "list_campaigns", json!({ "project_id": project, "archived": archived })).await;
            serde_json::from_str::<Value>(&text).unwrap().as_array().unwrap().iter().map(|c| c["id"].clone()).collect::<Vec<_>>()
        };
        for _ in 0..2 {
            let (is_error, text) = server.call(KEY, "archive_campaign", json!({ "campaign_id": segment })).await;
            assert!(!is_error && parsed(&text)["archived_at"].is_string(), "{text}");
        }
        assert_eq!((listed(false).await, listed(true).await), (vec![], vec![json!(segment)]));
        let (is_error, text) = server.call(KEY, "unarchive_campaign", json!({ "campaign_id": segment })).await;
        assert!(!is_error && parsed(&text)["archived_at"].is_null() && parsed(&text)["status"] == "paused", "{text}");
        assert_eq!(listed(false).await, vec![json!(segment)]);

        let (is_error, text) = server.call(KEY, "update_project_controls", json!({ "project_id": project, "daily_email_limit": 70, "review_window_hours": 12 })).await;
        assert!(!is_error && parsed(&text)["daily_email_limit"] == 70 && parsed(&text)["review_window_hours"] == 12, "{text}");
        let (is_error, text) = server.call(KEY, "update_project_controls", json!({ "project_id": project, "budget": 5 })).await;
        assert!(is_error && text.contains("unknown field `budget`"), "{text}");

        let (is_error, text) = server.call(KEY, "list_hot_leads", json!({})).await;
        let hot = parsed(&text);
        assert!(!is_error, "{text}");
        assert_eq!((hot["total"].clone(), hot["items"][0]["lead_id"].clone(), hot["items"][0]["reply"].clone()), (json!(1), json!(lead), json!("Can we talk Tuesday?")));
        let (is_error, text) = server.call(KEY, "get_inbox_changes", json!({ "project_id": project })).await;
        let changes = parsed(&text);
        assert!(!is_error && changes["items"].as_array().unwrap().len() == 1 && changes["has_more"] == false, "{text}");
        let (_, text) = server.call(KEY, "get_inbox_changes", json!({ "project_id": project, "cursor": changes["next_cursor"] })).await;
        assert_eq!(parsed(&text)["items"], json!([]));

        // Another organization's ids look like ids that don't exist.
        for (tool, args) in [
            ("get_campaign", json!({ "campaign_id": other_segment })),
            ("start_campaign", json!({ "campaign_id": other_segment })),
            ("stop_campaign", json!({ "campaign_id": other_segment })),
            ("approve_campaign", json!({ "campaign_id": other_segment })),
            ("archive_campaign", json!({ "campaign_id": other_segment })),
            ("unarchive_campaign", json!({ "campaign_id": other_segment })),
            ("update_project_controls", json!({ "project_id": other_project, "daily_email_limit": 70 })),
            ("get_inbox_changes", json!({ "project_id": other_project })),
            ("list_hot_leads", json!({ "project_id": other_project })),
        ] {
            assert_eq!(server.call(KEY, tool, args).await, (true, "Not found".into()), "{tool}");
        }
        assert_eq!(status(other_segment).await, "draft");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn list_tasks_filters_the_keys_organizations_runs_by_kind(db: PgPool) {
        let [(project, _), (other_project, _)] = seed(&db).await;
        for (project, kind) in [(project, "find_people"), (project, "write_emails"), (other_project, "find_people")] {
            sqlx::query("insert into agent_runs (project_id, kind) values ($1, $2)").bind(project).bind(kind).execute(&db).await.unwrap();
        }
        let server = Server::start(db).await;
        let (error, text) = server.call(KEY, "list_tasks", json!({ "type": "run", "kind": "find_people", "per_page": 5 })).await;
        assert!(!error, "{text}");
        let page: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((&page["total"], &page["items"][0]["project_id"], &page["items"][0]["status"]), (&json!(1), &json!(project), &json!("pending")));
        assert!(server.call(KEY, "list_tasks", json!({ "status": "done" })).await.0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn replies_and_new_replies_filter_by_campaign_id(db: PgPool) {
        let [(project, segment), _] = seed(&db).await;
        sqlx::query(
            "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id)
             insert into email_messages (project_id, lead_id, segment_id, direction, kind, status, created_at)
             select $1, l.id, $2, 'inbound', 'inbound', 'received', now() - interval '1 minute' from l",
        )
        .bind(project)
        .bind(segment)
        .execute(&db)
        .await
        .unwrap();
        let server = Server::start(db).await;
        let other = Uuid::new_v4();
        for (tool, args, field) in [
            ("list_replies", json!({}), "total"),
            ("get_inbox_changes", json!({ "project_id": project }), "items"),
        ] {
            let count = |campaign: Uuid| {
                let mut args = args.clone();
                args["campaign_id"] = json!(campaign);
                let server = &server;
                async move {
                    let (error, text) = server.call(KEY, tool, args).await;
                    assert!(!error, "{tool}: {text}");
                    let body: Value = serde_json::from_str(&text).unwrap();
                    if field == "total" { body["total"].as_i64().unwrap() } else { body["items"].as_array().unwrap().len() as i64 }
                }
            };
            assert_eq!((count(segment).await, count(other).await), (1, 0), "{tool}");
        }
    }
}
