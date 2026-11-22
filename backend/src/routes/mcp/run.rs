//! Runs a tool: each calls the public API v1 handler it matches, for the organization the request signed in as.

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::to_bytes;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use reqwest::Url;
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::ApiKeyAuth;
use crate::company_filters::{Claude, Model};
use crate::error::{AppError, AppResult};
use crate::routes::api_v1 as v1;
use crate::state::AppState;

const EMAIL_BATCH_NEXT: &str = "This runs in the background, within the organization's 200 email searches an hour. Call get_email_batch with task_id until status is completed.";
const RUN_NEXT: &str = "This runs in the background and takes a few minutes. Call get_run with run_id until status is succeeded, failed or cancelled.";
const IMPORT_NEXT: &str = "The leads go in in the background, usually within seconds. Call get_campaign_import with task_id until status is \
completed or failed, then list_campaign_import_rows says what became of each lead.";
const PROJECT_FIELDS: &[&str] = &["id", "name", "domain", "status", "created_at", "review_window_hours"];
const LEAD_FIELDS: &[&str] = &[
    "id", "full_name", "title", "email", "company_name", "company_domain", "status", "segment_id", "segment_name", "last_email_status", "score",
    "score_fit", "score_intent", "score_reasons", "owner_id", "owner_name", "owner_email", "language", "effective_language",
    "effective_language_source",
];
const COMPANY_FIELDS: &[&str] =
    &["id", "name", "domain", "segment_id", "country_code", "employees", "status", "score", "score_fit", "score_intent", "score_reasons", "hiring", "tech", "news"];
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
pub(super) const MAX_DOWNLOAD_ROWS: i64 = 200;
/// Signals one list_company_signals call returns at most, so a big project's list doesn't fill the client's context.
pub(super) const MAX_SIGNALS: usize = 100;
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
pub(super) async fn run(state: &AppState, auth: ApiKeyAuth, name: &str, mut args: Value) -> Option<AppResult<Value>> {
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
