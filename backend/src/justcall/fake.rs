//! A local stand-in for JustCall's API under /justcall, that checks the API key and secret.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

pub const API_KEY: &str = "justcall-key";
pub const API_SECRET: &str = "justcall-secret";
/// Another account's pair, which the fake takes too.
pub const OTHER_KEY: &str = "other-justcall-key";
pub const OTHER_SECRET: &str = "other-justcall-secret";

#[derive(Clone, Default)]
pub struct Fake {
    /// Each URL Splee added, as it sent it. The nth gets the url_id "u{n}".
    pub added: Arc<Mutex<Vec<Value>>>,
    /// The event type the fake refuses to add a URL to, as JustCall does for a plan without it.
    pub refuse: Arc<Mutex<Option<String>>>,
    /// The url_ids Splee removed, and the key it used for each.
    pub removed: Arc<Mutex<Vec<String>>>,
    pub removed_with: Arc<Mutex<Vec<String>>>,
}

/// Starts the fake and returns the base URL to use as Config::justcall_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/justcall/v2.1/webhooks", get(|| async { Json(json!({ "status": "success", "count": 0, "data": [] })) }).post(add))
        .route("/justcall/v2.1/webhooks/url/{id}", delete(remove))
        .route(
            "/justcall/v2.1/calls/{id}/recording/download",
            get(|Path(id): Path<i64>| async move {
                if id == 404 {
                    return (StatusCode::NOT_FOUND, Json(json!({ "status": "error", "message": "Recording not found" }))).into_response();
                }
                ([("content-type", "audio/mpeg")], format!("mp3 of {id}")).into_response()
            }),
        )
        .layer(axum::middleware::from_fn(gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

/// The signature header JustCall sends with `event`, as its docs compute it.
pub fn sign(secret: &str, event: &Value, timestamp: &str) -> String {
    let url = event["webhook_url"].as_str().unwrap_or_default();
    // As the docs' Python does it, with urllib's quote and nothing kept safe.
    let encoded: String =
        url.bytes().map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("{secret}|{encoded}|{}|{timestamp}", event["type"].as_str().unwrap_or_default()).as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

fn key(headers: &HeaderMap) -> &str {
    headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default()
}

async fn gate(req: Request, next: axum::middleware::Next) -> Response {
    let pairs = [format!("{API_KEY}:{API_SECRET}"), format!("{OTHER_KEY}:{OTHER_SECRET}")];
    if !pairs.iter().any(|p| p == key(req.headers())) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "status": "error", "message": "Unauthorized" }))).into_response();
    }
    next.run(req).await
}

async fn add(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    if fake.refuse.lock().unwrap().as_deref() == body["type"].as_str() {
        return (StatusCode::FORBIDDEN, Json(json!({ "status": "error", "message": "Not allowed" }))).into_response();
    }
    let mut added = fake.added.lock().unwrap();
    added.push(body.clone());
    let url = json!({ "webhook_url": body["webhook_url"], "url_id": format!("u{}", added.len()), "status": "Active", "api_version": "2.1" });
    Json(json!({ "type": body["type"], "url_count": 2, "webhook_urls": [{ "webhook_url": "https://zapier.example/hook", "url_id": "z1" }, url] }))
        .into_response()
}

async fn remove(State(fake): State<Fake>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    fake.removed.lock().unwrap().push(id);
    fake.removed_with.lock().unwrap().push(key(&headers).to_string());
    Json(json!({ "status": "success" })).into_response()
}
