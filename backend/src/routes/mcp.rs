//! Splee as a remote MCP server, so ChatGPT, Claude, Cursor and other MCP clients can prospect from chat. It serves both eras of
//! https://modelcontextprotocol.io/specification/2026-07-28 over Streamable HTTP: the stateless 2026-07-28 revision, and the
//! initialize-based 2025-11-25 and 2025-06-18 ones. Every POST gets one JSON answer, with no SSE stream or session. A request
//! signs in with an API key or an access token from Splee's OAuth (routes/oauth.rs), and each tool calls the matching public
//! API v1 handler for that organization.

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
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
use crate::auth::{ApiKeyAuth, api_key_org};
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

const INSTRUCTIONS: &str = "Splee finds companies and people for outbound email campaigns and writes the emails. A typical loop: \
list_projects, then list_campaigns or create_campaign, search_companies and add_companies, find_people, write_emails, then \
list_emails_to_approve and approve_email or skip_email. Finding people and writing emails take minutes: they return a run_id, \
so poll get_run until it's done. Approved emails go out from the user's own mailboxes on the project's schedule. Hot replies open \
deals on the pipeline, which list_deals, create_deal and move_deal follow to won or lost. assign_lead gives a lead to someone on \
the team. get_analytics says which campaigns, mailboxes, follow-ups and send times get replies.";

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
    let org_id = match authenticate(&state, &headers).await {
        Ok(org_id) => org_id,
        Err(res) => return res,
    };
    let Ok(message) = serde_json::from_slice::<Value>(&body) else {
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
        return stateless(&state, org_id, &headers, &message).await;
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
        "tools/call" => call_tool(&state, org_id, &params).await,
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
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<Uuid, Response> {
    let token = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    let found = match token {
        Some(token) => match api_key_org(&state.db, token).await {
            Ok(Some(org_id)) => Ok(Some(org_id)),
            Ok(None) => oauth::token_org(&state.db, token, &oauth::resource(&state.cfg)).await,
            Err(e) => Err(e),
        },
        None => Ok(None),
    };
    let metadata = oauth::resource_metadata_url(&state.cfg);
    let challenge = match (found, token) {
        (Ok(Some(org_id)), _) => return Ok(org_id),
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
async fn stateless(state: &AppState, org_id: Uuid, headers: &HeaderMap, message: &Value) -> Response {
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
        "tools/call" => match call_tool(state, org_id, params).await {
            Ok(result) => result,
            Err((code, text)) => return refuse(StatusCode::OK, code, &text),
        },
        _ => return refuse(StatusCode::NOT_FOUND, -32601, &format!("Unknown method {method}")),
    };
    result["resultType"] = json!("complete");
    result["_meta"] = json!({ "io.modelcontextprotocol/serverInfo": server_info() });
    Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

async fn call_tool(state: &AppState, org_id: Uuid, params: &Value) -> Result<Value, (i64, String)> {
    let name = params["name"].as_str().unwrap_or_default();
    let args = Some(&params["arguments"]).filter(|a| a.is_object()).cloned().unwrap_or_else(|| json!({}));
    let Some(outcome) = run(state, ApiKeyAuth { org_id }, name, args).await else {
        return Err((-32602, format!("Unknown tool {name}")));
    };
    // A failed call is a result the model reads and can recover from, like a wrong id, not a protocol error.
    let (text, is_error) = match outcome {
        Ok(value) => (value.to_string(), false),
        Err(e) => {
            if let AppError::Internal(err) = &e {
                tracing::error!(error = ?err, tool = name, "mcp tool failed");
            }
            (e.public_message(), true)
        }
    };
    Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": is_error }))
}

const RUN_NEXT: &str = "This runs in the background and takes a few minutes. Call get_run with run_id until status is succeeded, failed or cancelled.";
const PROJECT_FIELDS: &[&str] = &["id", "name", "domain", "status", "created_at"];
const LEAD_FIELDS: &[&str] = &[
    "id", "full_name", "title", "email", "company_name", "company_domain", "status", "segment_id", "segment_name", "last_email_status", "score",
    "score_fit", "score_intent", "score_reasons", "owner_id", "owner_name", "owner_email",
];
const COMPANY_FIELDS: &[&str] =
    &["id", "name", "domain", "segment_id", "country_code", "employees", "status", "score", "score_fit", "score_intent", "score_reasons", "hiring", "tech"];
const RUN_FIELDS: &[&str] = &["id", "kind", "status", "message", "error", "cost_usd", "created_at", "finished_at"];
const DEAL_FIELDS: &[&str] = &[
    "id", "project_name", "lead_id", "lead_name", "company_name", "stage", "status", "value", "currency", "close_date", "owner_name", "lost_reason",
    "stage_changed_at",
];
const STAGE_FIELDS: &[&str] = &["name", "kind", "probability", "deals"];

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
        "list_campaigns" => async { value(v1::list_campaigns(st(), auth, id(a, "project_id")?).await?) }.await,
        "create_campaign" => async {
            let created = value(v1::create_campaign(st(), auth, id(a, "project_id")?, Json(parse(a)?)).await?)?;
            Ok(json!({ "campaign_id": created["campaign_id"], "run_id": created["run_id"], "next": RUN_NEXT }))
        }
        .await,
        "search_companies" => async { value(v1::search_companies(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?) }.await,
        "add_companies" => async { value(v1::add_companies(st(), auth, id(a, "segment_id")?, Json(parse(a)?)).await?) }.await,
        "find_people" => async { started(value(v1::find_people(st(), auth, id(a, "segment_id")?, Some(Json(parse(a)?))).await?)) }.await,
        "write_emails" => async { started(value(v1::write_emails(st(), auth, id(a, "segment_id")?, Some(Json(parse(a)?))).await?)) }.await,
        "get_run" => async { Ok(pick(value(v1::get_run(st(), auth, id(a, "run_id")?).await?)?, RUN_FIELDS)) }.await,
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
        "get_analytics" => async { value(v1::project_analytics(st(), auth, id(a, "project_id")?, Query(parse(a)?)).await?) }.await,
        "list_deals" => async {
            let stages = value(v1::list_stages(st(), auth.clone()).await?)?;
            let page = value(v1::list_deals(st(), auth, Query(parse(a)?), Query(parse(a)?)).await?)?;
            Ok(json!({ "stages": pick(stages, STAGE_FIELDS), "items": pick(page["items"].clone(), DEAL_FIELDS), "total": page["total"] }))
        }
        .await,
        "create_deal" => async { Ok(pick(value(v1::create_deal(st(), auth, Json(parse(a)?)).await?)?, DEAL_FIELDS)) }.await,
        "move_deal" => async { Ok(pick(value(v1::move_deal(st(), auth, id(a, "deal_id")?, Json(parse(a)?)).await?)?, DEAL_FIELDS)) }.await,
        "assign_lead" => async {
            let detail = value(v1::assign_lead(st(), auth, id(a, "lead_id")?, Json(parse(a)?)).await?)?;
            Ok(json!({ "id": detail["id"], "full_name": detail["full_name"], "owner_id": detail["owner_id"], "owner_email": detail["owner_email"] }))
        }
        .await,
        _ => return None,
    })
}

fn value<T: Serialize>(Json(body): Json<T>) -> AppResult<Value> {
    serde_json::to_value(body).map_err(|e| AppError::Internal(e.into()))
}

fn parse<T: DeserializeOwned>(args: &Value) -> AppResult<T> {
    serde_json::from_value(args.clone()).map_err(|e| AppError::BadRequest(format!("Invalid arguments: {e}")))
}

fn id(args: &Value, key: &str) -> AppResult<Path<Uuid>> {
    // Tools name campaigns campaign_id, which `run` renamed to the API's segment_id.
    let shown = if key == "segment_id" { "campaign_id" } else { key };
    args[key].as_str().and_then(|s| s.trim().parse().ok()).map(Path).ok_or_else(|| AppError::BadRequest(format!("Pass {shown} as an id")))
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
    let project = id_prop("A project id from list_projects");
    let campaign = id_prop("A campaign id from list_campaigns or create_campaign");
    vec![
        tool("list_projects", "List projects", "The organization's projects. A project is one business doing outreach.", json!({}), &[], read.clone()),
        tool(
            "list_campaigns",
            "List campaigns",
            "A project's campaigns with their funnel: companies, leads, emails sent, leads contacted, replies, bounces, hot leads and \
             spend in micro-dollars. A campaign that A/B tests its email angle has ab_test: each variant's instructions and results, \
             whether a winner is clear yet in note, and the winner auto-pick kept, if any.",
            json!({ "project_id": project }),
            &["project_id"],
            read.clone(),
        ),
        tool(
            "create_campaign",
            "Create campaign",
            "Creates a campaign from a description of who to reach, like \"dental clinics in Leeds\". An agent names it and sets its \
             targeting, which takes a minute or two. Returns campaign_id and run_id.",
            json!({ "project_id": project, "prompt": { "type": "string", "description": "Who the campaign targets, in plain words" } }),
            &["project_id", "prompt"],
            adds.clone(),
        ),
        tool(
            "search_companies",
            "Search company database",
            "Searches the company database (businesses with a website from Overture Maps), the most reliable listings first. q matches \
             words at the start of words in the name or category. Each company's tech lists the tools its website and DNS show it \
             uses, once Splee has scanned it, which it does for every project's companies. total stops counting at 10,000, and capped \
             says there are more.",
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
            })),
            &[],
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
            "find_people",
            "Find people",
            "Starts an agent run that finds decision makers and their emails at the campaign's new companies, or only at company_ids. \
             Returns run_id. Poll get_run until it's done, then list_leads.",
            json!({
                "campaign_id": campaign,
                "company_ids": { "type": "array", "items": { "type": "string", "format": "uuid" }, "description": "Optional. Only these companies" },
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
             (bouncing flags over 5% bounced), sequence step, weekday and hour of send in the project's time zone, and channel. \
             Replies, bounces and meetings count for the last email the lead got before them.",
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
            "list_leads",
            "List leads",
            "A project's leads, newest first, or the likeliest buyers first with sort \"score\". Each has a 0-100 score, its fit \
             and intent parts (0-50 each) and the reasons behind it.",
            page_props(json!({
                "project_id": project,
                "campaign_id": id_prop("Optional. Only this campaign's leads"),
                "status": { "type": "string", "enum": LEAD_STATUSES, "description": "Optional. Only leads with this status" },
                "q": { "type": "string", "description": "Optional. Matches name, email, company or title" },
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
                "filter": { "type": "string", "enum": ["all", "needs_reply", "hot", "auto_replied", "other"], "description": "all by default" },
                "q": { "type": "string", "description": "Optional. Matches the lead's name, email or company" },
                "owner": owner_prop(),
            })),
            &[],
            read.clone(),
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
            read,
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
            "assign_lead",
            "Assign lead",
            "Gives a lead to someone on the team, who then gets its hot-reply alerts, or takes it back with owner null. The project's \
             assignment rule never moves a lead that has an owner.",
            json!({
                "lead_id": id_prop("A lead id from list_leads or list_replies"),
                "owner": { "type": ["string", "null"], "description": "A team member's email or user id, or null for nobody" },
            }),
            &["lead_id", "owner"],
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
                ipinfo_token: None,
            };
            let state = AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 };
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}/api/mcp", listener.local_addr().unwrap());
            let app = Router::new().nest("/api", super::router().merge(super::v1::router())).with_state(state);
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
                     "create_deal", "move_deal", "get_analytics", "assign_lead"] {
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
}
