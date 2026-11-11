//! A local stand-in for Stripe, Paddle and Lemon Squeezy, each under its own path, that checks the key and serves two
//! payments a page whatever the page size asked for, so a test sees pagination with a handful of rows.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};

pub const STRIPE_KEY: &str = concat!("rk", "_test_", "fake0123456789");
pub const PADDLE_KEY: &str = concat!("pdl", "_sdbx_apikey_", "fake0123456789");
pub const LEMON_KEY: &str = concat!("ls", "_fake_", "0123456789");

const PAGE: usize = 2;

#[derive(Clone, Default)]
pub struct Fake {
    /// Every request's path and query, in order.
    pub hits: Arc<Mutex<Vec<String>>>,
    /// Newest first, as Stripe lists them.
    pub stripe_charges: Arc<Mutex<Vec<Value>>>,
    pub stripe_refunds: Arc<Mutex<Vec<Value>>>,
    pub paddle_transactions: Arc<Mutex<Vec<Value>>>,
    pub paddle_adjustments: Arc<Mutex<Vec<Value>>>,
    pub lemon_orders: Arc<Mutex<Vec<Value>>>,
    pub lemon_invoices: Arc<Mutex<Vec<Value>>>,
    /// Stripe's and Paddle's customers' addresses by id.
    pub customers: Arc<Mutex<HashMap<String, String>>>,
}

impl Fake {
    pub fn hits(&self, prefix: &str) -> usize {
        self.hits.lock().unwrap().iter().filter(|h| h.starts_with(prefix)).count()
    }
}

/// Starts the fake and returns the base URL to use as Config::payments_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake::default();
    let app = Router::new()
        .route("/stripe/v1/charges", get(stripe_charges))
        .route("/stripe/v1/refunds", get(stripe_refunds))
        .route("/stripe/v1/customers", get(|| async { Json(json!({"data": [], "has_more": false})) }))
        .route("/stripe/v1/customers/{id}", get(|State(f): State<Fake>, Path(id): Path<String>| async move { customer(&f, &id, |e| json!({"id": id, "email": e})) }))
        .route("/paddle/transactions", get(paddle_transactions))
        .route("/paddle/adjustments", get(paddle_adjustments))
        .route("/paddle/customers", get(|| async { Json(json!({"data": [], "meta": {"pagination": {"has_more": false}}})) }))
        .route("/paddle/customers/{id}", get(|State(f): State<Fake>, Path(id): Path<String>| async move { customer(&f, &id, |e| json!({"data": {"id": id, "email": e}})) }))
        .route("/lemonsqueezy/v1/orders", get(|State(f): State<Fake>, Query(q): Query<HashMap<String, String>>| async move { lemon_page(&f.lemon_orders, &q) }))
        .route(
            "/lemonsqueezy/v1/subscription-invoices",
            get(|State(f): State<Fake>, Query(q): Query<HashMap<String, String>>| async move { lemon_page(&f.lemon_invoices, &q) }),
        )
        .layer(axum::middleware::from_fn_with_state(fake.clone(), gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

/// Records the request and turns away a missing or wrong key.
async fn gate(State(fake): State<Fake>, req: Request, next: axum::middleware::Next) -> Response {
    let uri = req.uri().clone();
    fake.hits.lock().unwrap().push(format!("{}?{}", uri.path(), uri.query().unwrap_or_default()));
    let key = match uri.path().split('/').nth(1) {
        Some("stripe") => STRIPE_KEY,
        Some("paddle") => PADDLE_KEY,
        _ => LEMON_KEY,
    };
    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    if auth != format!("Bearer {key}") {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": {"message": "Invalid API key"}}))).into_response();
    }
    next.run(req).await
}

fn customer(fake: &Fake, id: &str, body: impl Fn(&str) -> Value) -> Response {
    match fake.customers.lock().unwrap().get(id) {
        Some(email) => Json(body(email)).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"error": "No such customer"}))).into_response(),
    }
}

/// The items after the one with id `after`, two at a time, and whether more follow.
fn after(items: &[Value], after: Option<&String>) -> (Vec<Value>, bool) {
    let start = after.and_then(|a| items.iter().position(|i| i["id"] == a.as_str()).map(|p| p + 1)).unwrap_or(0);
    (items.iter().skip(start).take(PAGE).cloned().collect(), start + PAGE < items.len())
}

async fn stripe_charges(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let since: i64 = q.get("created[gte]").and_then(|v| v.parse().ok()).unwrap_or(0);
    let all: Vec<Value> = fake.stripe_charges.lock().unwrap().iter().filter(|c| c["created"].as_i64().unwrap_or(0) >= since).cloned().collect();
    let (data, more) = after(&all, q.get("starting_after"));
    Json(json!({ "object": "list", "data": data, "has_more": more }))
}

/// A charge's refunds all in one page, since the backfill asks for up to 100 and no test makes that many.
async fn stripe_refunds(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let charge = q.get("charge").cloned().unwrap_or_default();
    let data: Vec<Value> = fake.stripe_refunds.lock().unwrap().iter().filter(|r| r["charge"] == charge.as_str()).cloned().collect();
    Json(json!({ "object": "list", "data": data, "has_more": false }))
}

async fn paddle_adjustments(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let ids: Vec<&str> = q.get("transaction_id").map(|t| t.split(',').collect()).unwrap_or_default();
    let all: Vec<Value> = fake
        .paddle_adjustments
        .lock()
        .unwrap()
        .iter()
        .filter(|a| ids.is_empty() || ids.iter().any(|id| a["transaction_id"] == *id))
        .filter(|a| q.get("action").is_none_or(|v| a["action"] == v.as_str()) && q.get("status").is_none_or(|v| a["status"] == v.as_str()))
        .cloned()
        .collect();
    let (data, more) = after(&all, q.get("after"));
    Json(json!({ "data": data, "meta": { "pagination": { "per_page": PAGE, "has_more": more } } }))
}

async fn paddle_transactions(State(fake): State<Fake>, Query(q): Query<HashMap<String, String>>) -> Json<Value> {
    let since = q.get("billed_at[GTE]").and_then(|v| DateTime::parse_from_rfc3339(v).ok()).map(|d| d.with_timezone(&Utc));
    let billed = |t: &Value| t["billed_at"].as_str().and_then(|v| DateTime::parse_from_rfc3339(v).ok()).map(|d| d.with_timezone(&Utc));
    let all: Vec<Value> = fake.paddle_transactions.lock().unwrap().iter().filter(|t| since.is_none_or(|s| billed(t).is_some_and(|b| b >= s))).cloned().collect();
    let (mut data, more) = after(&all, q.get("after"));
    if q.get("include").is_some_and(|i| i == "customer") {
        let customers = fake.customers.lock().unwrap();
        for t in &mut data {
            if let Some(email) = t["customer_id"].as_str().and_then(|id| customers.get(id)) {
                t["customer"] = json!({ "id": t["customer_id"], "email": email });
            }
        }
    }
    Json(json!({ "data": data, "meta": { "pagination": { "per_page": PAGE, "has_more": more } } }))
}

fn lemon_page(items: &Mutex<Vec<Value>>, q: &HashMap<String, String>) -> Json<Value> {
    let items = items.lock().unwrap();
    let number: usize = q.get("page[number]").and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    let data: Vec<Value> = items.iter().skip((number - 1) * PAGE).take(PAGE).cloned().collect();
    let last = items.len().div_ceil(PAGE).max(1);
    Json(json!({ "data": data, "meta": { "page": { "currentPage": number, "perPage": PAGE, "lastPage": last, "total": items.len() } } }))
}
