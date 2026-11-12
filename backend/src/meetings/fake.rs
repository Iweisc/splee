//! A local stand-in for Fireflies, Fathom, Gong and Zoom, each under its own path, that checks their credentials. Gong serves
//! two calls a page, so a test sees its cursor with a handful of calls.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

pub const FIREFLIES_KEY: &str = "ff-fake-key";
pub const FATHOM_KEY: &str = "fathom-fake-key";
pub const GONG_KEY: &str = "gong-access";
pub const GONG_SECRET: &str = "gong-secret";
pub const ZOOM_ACCOUNT: &str = "zoom-account";
pub const ZOOM_CLIENT: &str = "zoom-client";
pub const ZOOM_SECRET: &str = "zoom-client-secret";
const ZOOM_TOKEN: &str = "zoom-access-token";
pub const ZOOM_DOWNLOAD_TOKEN: &str = "zoom-download-token";

const PAGE: usize = 2;

#[derive(Clone, Default)]
pub struct Fake {
    /// Every request's path and query, in order.
    pub hits: Arc<Mutex<Vec<String>>>,
    /// Fireflies' transcripts by id, as its GraphQL API answers them.
    pub fireflies: Arc<Mutex<HashMap<String, Value>>>,
    /// Fathom's transcripts by recording id.
    pub fathom_transcripts: Arc<Mutex<HashMap<String, Value>>>,
    /// Gong's calls, as /v2/calls/extensive lists them, and each one's transcript by call id.
    pub gong_calls: Arc<Mutex<Vec<Value>>>,
    pub gong_transcripts: Arc<Mutex<HashMap<String, Value>>>,
    /// The bodies Gong's calls endpoint got, in order.
    pub gong_requests: Arc<Mutex<Vec<Value>>>,
    /// Zoom's participants by meeting UUID as it appears in the path, and the transcript file.
    pub zoom_participants: Arc<Mutex<HashMap<String, Value>>>,
    pub zoom_vtt: Arc<Mutex<String>>,
}

impl Fake {
    pub fn hits(&self, prefix: &str) -> usize {
        self.hits.lock().unwrap().iter().filter(|h| h.starts_with(prefix)).count()
    }
}

/// Starts the fake and returns the base URL to use as Config::meetings_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/fireflies/graphql", post(fireflies))
        .route("/fathom/meetings", get(|| async { Json(json!({ "items": [], "next_cursor": null })) }))
        .route(
            "/fathom/recordings/{id}/transcript",
            get(|State(f): State<Fake>, Path(id): Path<String>| async move { found(f.fathom_transcripts.lock().unwrap().get(&id).map(|t| json!({ "transcript": t }))) }),
        )
        .route("/gong/v2/workspaces", get(|| async { Json(json!({ "workspaces": [{ "id": "1", "name": "Acme" }] })) }))
        .route("/gong/v2/calls/extensive", post(gong_calls))
        .route("/gong/v2/calls/transcript", post(gong_transcript))
        .route("/zoom/oauth/token", post(zoom_token))
        .route(
            "/zoom/v2/past_meetings/{uuid}/participants",
            get(|State(f): State<Fake>, Path(uuid): Path<String>| async move { found(f.zoom_participants.lock().unwrap().get(&uuid).cloned()) }),
        )
        .route("/zoom/download/{file}", get(|State(f): State<Fake>| async move { f.zoom_vtt.lock().unwrap().clone() }))
        .layer(axum::middleware::from_fn_with_state(fake.clone(), gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

fn found(body: Option<Value>) -> Response {
    match body {
        Some(body) => Json(body).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "not found" }))).into_response(),
    }
}

fn basic(user: &str, password: &str) -> String {
    format!("Basic {}", STANDARD.encode(format!("{user}:{password}")))
}

/// Records the request and turns away missing or wrong credentials.
async fn gate(State(fake): State<Fake>, req: Request, next: axum::middleware::Next) -> Response {
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    fake.hits.lock().unwrap().push(format!("{}?{}", path, uri.query().unwrap_or_default()));
    // Owned, since a borrow of the request can't be held across the await below.
    let headers = req.headers().clone();
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let ok = match path.split('/').nth(1) {
        Some("fireflies") => header("authorization") == format!("Bearer {FIREFLIES_KEY}"),
        Some("fathom") => header("x-api-key") == FATHOM_KEY,
        Some("gong") => header("authorization") == basic(GONG_KEY, GONG_SECRET),
        _ if path == "/zoom/oauth/token" => {
            header("authorization") == basic(ZOOM_CLIENT, ZOOM_SECRET) && uri.query().is_some_and(|q| q.contains(&format!("account_id={ZOOM_ACCOUNT}")))
        }
        _ if path.starts_with("/zoom/download/") => header("authorization") == format!("Bearer {ZOOM_DOWNLOAD_TOKEN}"),
        _ => header("authorization") == format!("Bearer {ZOOM_TOKEN}"),
    };
    if !ok {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "message": "Invalid credentials" }))).into_response();
    }
    next.run(req).await
}

/// The two queries Splee sends: the user, to check the key, and a transcript by id.
async fn fireflies(State(fake): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
    let query = body["query"].as_str().unwrap_or_default();
    if query.contains("user {") {
        return Json(json!({ "data": { "user": { "email": "owner@acme.example" } } }));
    }
    let id = body["variables"]["id"].as_str().unwrap_or_default();
    match fake.fireflies.lock().unwrap().get(id) {
        Some(t) => Json(json!({ "data": { "transcript": t } })),
        None => Json(json!({ "data": { "transcript": null }, "errors": [{ "message": "Transcript not found", "extensions": { "code": "object_not_found" } }] })),
    }
}

/// Calls whose start is in the filter's window, two a page. The cursor is where the next page starts.
async fn gong_calls(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    fake.gong_requests.lock().unwrap().push(body.clone());
    let at = |v: &Value| v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok());
    let (from, to) = (at(&body["filter"]["fromDateTime"]), at(&body["filter"]["toDateTime"]));
    let calls: Vec<Value> = fake
        .gong_calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| at(&c["metaData"]["started"]).is_some_and(|s| from.is_none_or(|f| s >= f) && to.is_none_or(|t| s <= t)))
        .cloned()
        .collect();
    if calls.is_empty() {
        return (StatusCode::NOT_FOUND, Json(json!({ "errors": ["No calls found corresponding to the provided filters"] }))).into_response();
    }
    let start: usize = body["cursor"].as_str().and_then(|c| c.strip_prefix("page-")).and_then(|n| n.parse().ok()).unwrap_or(0);
    let page: Vec<Value> = calls.iter().skip(start).take(PAGE).cloned().collect();
    let next = (start + PAGE < calls.len()).then(|| format!("page-{}", start + PAGE));
    let records = match next {
        Some(cursor) => json!({ "totalRecords": calls.len(), "currentPageSize": page.len(), "cursor": cursor }),
        None => json!({ "totalRecords": calls.len(), "currentPageSize": page.len() }),
    };
    Json(json!({ "records": records, "calls": page })).into_response()
}

async fn gong_transcript(State(fake): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
    let ids: Vec<String> = body["filter"]["callIds"].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_string)).collect();
    let transcripts = fake.gong_transcripts.lock().unwrap();
    let list: Vec<Value> = ids.iter().filter_map(|id| transcripts.get(id).map(|t| json!({ "callId": id, "transcript": t }))).collect();
    Json(json!({ "callTranscripts": list }))
}

async fn zoom_token(Query(q): Query<HashMap<String, String>>) -> Response {
    if q.get("grant_type").map(String::as_str) != Some("account_credentials") {
        return (StatusCode::BAD_REQUEST, Json(json!({ "reason": "unsupported grant type" }))).into_response();
    }
    Json(json!({ "access_token": ZOOM_TOKEN, "token_type": "bearer", "expires_in": 3599 })).into_response()
}
