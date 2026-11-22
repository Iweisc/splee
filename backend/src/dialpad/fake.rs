//! A local stand-in for Dialpad's API under /dialpad, that checks the API key.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde_json::{Value, json};

pub const API_KEY: &str = "dialpad-key";

#[derive(Clone, Default)]
pub struct Fake {
    /// The webhooks Splee created, as it sent them. The nth gets id 100 + n.
    pub webhooks: Arc<Mutex<Vec<Value>>>,
    /// The call subscriptions, as sent. The nth gets id 200 + n.
    pub subscriptions: Arc<Mutex<Vec<Value>>>,
    /// The paths Splee deleted, like "webhooks/101".
    pub deleted: Arc<Mutex<Vec<String>>>,
    /// Dialpad's users by email, and who has no Dialpad app open to ring, by id.
    pub users: Arc<Mutex<HashMap<String, Value>>>,
    pub no_app: Arc<Mutex<HashSet<i64>>>,
    /// Each call started, as the user's id and the body.
    pub dialed: Arc<Mutex<Vec<(i64, Value)>>>,
    /// Each call's transcript lines by call id.
    pub transcripts: Arc<Mutex<HashMap<i64, Value>>>,
}

/// Starts the fake and returns the base URL to use as Config::dialpad_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/dialpad/api/v2/company", get(|| async { Json(json!({ "id": "6456332278300672", "name": "Acme" })) }))
        .route("/dialpad/api/v2/webhooks", post(create_webhook))
        .route("/dialpad/api/v2/subscriptions/call", post(subscribe))
        .route("/dialpad/api/v2/webhooks/{id}", delete(|State(f): State<Fake>, Path(id): Path<String>| async move { deleted(&f, format!("webhooks/{id}")) }))
        .route(
            "/dialpad/api/v2/subscriptions/call/{id}",
            delete(|State(f): State<Fake>, Path(id): Path<String>| async move { deleted(&f, format!("subscriptions/call/{id}")) }),
        )
        .route("/dialpad/api/v2/users", get(users))
        .route("/dialpad/api/v2/users/{id}/initiate_call", post(initiate_call))
        .route(
            "/dialpad/api/v2/transcripts/{id}",
            get(|State(f): State<Fake>, Path(id): Path<i64>| async move {
                match f.transcripts.lock().unwrap().get(&id) {
                    Some(lines) => Json(json!({ "call_id": id.to_string(), "lines": lines })).into_response(),
                    None => (StatusCode::NOT_FOUND, Json(json!({ "error": { "message": "Not found" } }))).into_response(),
                }
            }),
        )
        .route("/dialpad/r/{id}", get(|Path(id): Path<String>| async move { ([("content-type", "audio/mpeg")], format!("mp3 of {id}")) }))
        .layer(axum::middleware::from_fn(gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

/// An event as Dialpad sends it to a webhook with a secret: a JWT signed with HS256.
pub fn sign(secret: &str, event: &Value) -> String {
    crate::crypto::hs256_jwt(secret.as_bytes(), &json!({ "alg": "HS256", "typ": "JWT" }), event)
}

async fn gate(req: Request, next: axum::middleware::Next) -> Response {
    if req.headers().get("authorization").and_then(|v| v.to_str().ok()) != Some(format!("Bearer {API_KEY}").as_str()) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": { "code": 401, "message": "Invalid API key" } }))).into_response();
    }
    next.run(req).await
}

fn deleted(fake: &Fake, path: String) -> Response {
    fake.deleted.lock().unwrap().push(path);
    Json(json!({})).into_response()
}

async fn create_webhook(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    let mut webhooks = fake.webhooks.lock().unwrap();
    webhooks.push(body.clone());
    let id = 100 + webhooks.len();
    Json(json!({ "id": id.to_string(), "hook_url": body["hook_url"], "signature": { "algo": "HS256", "secret": body["secret"], "type": "jwt" } })).into_response()
}

async fn subscribe(State(fake): State<Fake>, Json(body): Json<Value>) -> Response {
    let mut subscriptions = fake.subscriptions.lock().unwrap();
    subscriptions.push(body.clone());
    let id = 200 + subscriptions.len();
    Json(json!({ "id": id.to_string(), "call_states": body["call_states"], "enabled": true, "webhook": { "id": body["endpoint_id"] } })).into_response()
}

async fn users(State(fake): State<Fake>, Query(query): Query<HashMap<String, String>>) -> Response {
    let found = query.get("email").and_then(|email| fake.users.lock().unwrap().get(email).cloned());
    Json(json!({ "items": found.into_iter().collect::<Vec<_>>() })).into_response()
}

/// Dialpad turns a call away when the user has no Dialpad app open to ring.
async fn initiate_call(State(fake): State<Fake>, Path(id): Path<i64>, Json(body): Json<Value>) -> Response {
    if fake.no_app.lock().unwrap().contains(&id) {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": { "code": 400, "message": "No autocallable device" } }))).into_response();
    }
    fake.dialed.lock().unwrap().push((id, body));
    Json(json!({ "device": { "id": "dev-1", "type": "native", "user_id": id.to_string() } })).into_response()
}
