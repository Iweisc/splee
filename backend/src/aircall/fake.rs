//! A local stand-in for Aircall's API under /aircall/v1 that checks the API ID and token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};

pub const API_ID: &str = "aircall-id";
pub const API_TOKEN: &str = "aircall-token";

#[derive(Clone, Default)]
pub struct Fake {
    /// The webhooks Splee created, as it sent them. The nth gets id wh-n and token token-n.
    pub webhooks: Arc<Mutex<Vec<Value>>>,
    pub deleted: Arc<Mutex<Vec<String>>>,
    /// Calls by id, as GET /calls/{id} answers them, and each one's utterances.
    pub calls: Arc<Mutex<HashMap<i64, Value>>>,
    pub transcripts: Arc<Mutex<HashMap<i64, Value>>>,
}

/// Starts the fake and returns the base URL to use as Config::aircall_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/aircall/v1/ping", get(|| async { Json(json!({ "ping": "pong" })) }))
        .route("/aircall/v1/webhooks", post(create_webhook))
        .route(
            "/aircall/v1/webhooks/{id}",
            delete(|State(f): State<Fake>, Path(id): Path<String>| async move {
                f.deleted.lock().unwrap().push(id);
                Json(json!({}))
            }),
        )
        .route("/aircall/v1/calls/{id}", get(|State(f): State<Fake>, Path(id): Path<i64>| async move { found(f.calls.lock().unwrap().get(&id).map(|c| json!({ "call": c }))) }))
        .route(
            "/aircall/v1/calls/{id}/transcription",
            get(|State(f): State<Fake>, Path(id): Path<i64>| async move {
                let utterances = f.transcripts.lock().unwrap().get(&id).cloned();
                found(utterances.map(|u| json!({ "transcription": { "id": 1, "call_id": id, "type": "call", "content": { "language": "en", "utterances": u } } })))
            }),
        )
        .layer(axum::middleware::from_fn(gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

fn found(body: Option<Value>) -> Response {
    match body {
        Some(body) => Json(body).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({ "error": "Not found" }))).into_response(),
    }
}

/// Aircall turns away wrong credentials with a 403.
async fn gate(req: Request, next: axum::middleware::Next) -> Response {
    let expected = format!("Basic {}", STANDARD.encode(format!("{API_ID}:{API_TOKEN}")));
    if req.headers().get("authorization").and_then(|v| v.to_str().ok()) != Some(expected.as_str()) {
        return (StatusCode::FORBIDDEN, Json(json!({ "error": "Forbidden", "troubleshoot": "Check your API key" }))).into_response();
    }
    next.run(req).await
}

async fn create_webhook(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    let mut webhooks = fake.webhooks.lock().unwrap();
    webhooks.push(body.clone());
    let n = webhooks.len();
    let hook = json!({ "webhook_id": format!("wh-{n}"), "url": body["url"], "active": true, "events": body["events"], "token": format!("token-{n}") });
    (StatusCode::CREATED, Json(json!({ "webhook": hook }))).into_response()
}
