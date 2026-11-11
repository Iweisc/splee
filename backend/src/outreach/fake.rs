//! A local stand-in for Instantly, Smartlead and lemlist, each under its own path, that checks the key the way the tool
//! takes it. It serves small pages, so a test sees pagination with a handful of rows.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use serde_json::{Value, json};

pub const INSTANTLY_KEY: &str = concat!("inst", "_fake_", "0123456789");
pub const SMARTLEAD_KEY: &str = concat!("sl", "_fake_", "0123456789");
pub const LEMLIST_KEY: &str = concat!("lem", "_fake_", "0123456789");

pub const DENTISTS: &str = "0199a1c2-0000-7000-8000-000000000001";
pub const AGENCIES: &str = "0199a1c2-0000-7000-8000-000000000002";

#[derive(Clone, Default)]
pub struct Fake {
    /// Every request's path and query, in order.
    pub hits: Arc<Mutex<Vec<String>>>,
    /// The next this many requests get a 429 with Retry-After: 1.
    pub throttle: Arc<AtomicUsize>,
    /// Instantly's leads by campaign id, which a test can change between runs.
    pub instantly_leads: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Fake {
    pub fn hits(&self, prefix: &str) -> usize {
        self.hits.lock().unwrap().iter().filter(|h| h.starts_with(prefix)).count()
    }
}

/// Starts the fake and returns the base URL to use as Config::outreach_api_url.
pub async fn start() -> (String, Fake) {
    let fake = Fake { instantly_leads: Arc::new(Mutex::new(instantly_leads())), ..Fake::default() };
    let app = Router::new()
        .route("/instantly/campaigns", get(instantly_campaigns))
        .route("/instantly/campaigns/analytics", get(|| async { Json(json!([{"campaign_id": DENTISTS, "leads_count": 7}, {"campaign_id": AGENCIES, "leads_count": 0}])) }))
        .route("/instantly/campaigns/{id}", get(instantly_campaign))
        .route("/instantly/leads/list", post(instantly_lead_page))
        .route("/instantly/block-lists-entries", get(|| async { Json(json!({"items": [{"bl_value": "blocked.io", "is_domain": true}, {"bl_value": "Nope@Example.com", "is_domain": false}, {"bl_value": "https://linkedin.com/in/x", "is_domain": false}]})) }))
        .route("/smartlead/campaigns/", get(|| async { Json(json!([{"id": 101, "name": "SaaS founders", "status": "ACTIVE"}])) }))
        .route("/smartlead/campaigns/{id}/sequences", get(|| async { Json(smartlead_sequences()) }))
        .route("/smartlead/campaigns/{id}/leads", get(smartlead_lead_page))
        .route("/smartlead/leads/get-domain-block-list", get(|| async { Json(json!([{"id": 1, "email_or_domain": "rival.com"}])) }))
        .route("/lemlist/campaigns", get(lemlist_campaigns))
        .route("/lemlist/campaigns/reports", get(lemlist_reports))
        .route("/lemlist/campaigns/{id}/sequences", get(|| async { Json(lemlist_sequences()) }))
        .route("/lemlist/v2/campaigns/{id}/export/leads", get(|| async { Json(lemlist_leads()) }))
        .route("/lemlist/v2/unsubscribes/variables", get(|| async { Json(json!([{"_id": "uns_1", "value": "gone@lemlist-lead.com", "source": "lead"}])) }))
        .layer(axum::middleware::from_fn_with_state(fake.clone(), gate))
        .with_state(fake.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, fake)
}

/// Records the request, answers 429 while throttled, and turns away a missing or wrong key.
async fn gate(State(fake): State<Fake>, req: Request, next: axum::middleware::Next) -> Response {
    let uri = req.uri().clone();
    let path = uri.path().to_string();
    fake.hits.lock().unwrap().push(format!("{path}?{}", uri.query().unwrap_or_default()));
    if fake.throttle.load(Ordering::SeqCst) > 0 {
        fake.throttle.fetch_sub(1, Ordering::SeqCst);
        return (StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], "slow down").into_response();
    }
    let headers: &HeaderMap = req.headers();
    let auth = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default();
    let parsed = reqwest::Url::parse(&format!("http://fake{uri}")).unwrap();
    let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    let lemlist = format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!(":{LEMLIST_KEY}")));
    let ok = match path.split('/').nth(1) {
        Some("instantly") => auth == format!("Bearer {INSTANTLY_KEY}"),
        Some("smartlead") => query.iter().any(|(k, v)| k == "api_key" && v == SMARTLEAD_KEY),
        Some("lemlist") => auth == lemlist,
        _ => false,
    };
    if !ok {
        return (StatusCode::UNAUTHORIZED, Json(json!({"message": "Invalid API key"}))).into_response();
    }
    next.run(req).await
}

fn page_after(items: &[Value], after: Option<&str>, size: usize, key: &str) -> Value {
    let start = after.and_then(|a| items.iter().position(|i| i[key] == a).map(|p| p + 1)).unwrap_or(0);
    let page: Vec<Value> = items.iter().skip(start).take(size).cloned().collect();
    let next = (start + size < items.len()).then(|| page.last().map(|i| i[key].clone())).flatten();
    json!({ "items": page, "next_starting_after": next })
}

fn instantly_campaign_list() -> Vec<Value> {
    let steps = json!([{"sequences": [{"steps": [
        {"type": "email", "delay": 3, "variants": [{"subject": "{{companyName}} x Fieldnote", "body": "<div>Hi {{firstName}},</div><div><br /></div><div>{{personalization}} Saw {{companyName}} is hiring.</div><div>{{sendingAccountFirstName}}</div>"}]},
        {"type": "email", "delay": 4, "variants": [{"subject": "", "body": "<div>Any thoughts, {{firstName}}?</div>"}]},
        {"type": "email", "variants": [{"subject": "", "body": "Last note from me."}]},
    ]}]}]);
    vec![
        json!({"id": DENTISTS, "name": "Dentists UK", "status": 1, "sequences": steps[0]["sequences"]}),
        json!({"id": AGENCIES, "name": "Agencies", "status": 2, "sequences": [{"steps": [{"type": "email", "variants": [{"subject": "Hi", "body": "Hello {{firstName}}"}]}]}]}),
    ]
}

async fn instantly_campaigns(Query(q): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let mut page = page_after(&instantly_campaign_list(), q.get("starting_after").map(String::as_str), 1, "id");
    // The list leaves sequences out in this fake, as a reminder that steps come from the campaign itself.
    for c in page["items"].as_array_mut().unwrap() {
        c.as_object_mut().unwrap().remove("sequences");
    }
    Json(page)
}

async fn instantly_campaign(Path(id): Path<String>) -> Response {
    match instantly_campaign_list().into_iter().find(|c| c["id"] == id.as_str()) {
        Some(c) => Json(c).into_response(),
        None => (StatusCode::NOT_FOUND, Json(json!({"message": "Campaign not found"}))).into_response(),
    }
}

/// Seven leads in Dentists UK: new, contacted, replied, bounced, unsubscribed, one at a blocklisted domain and one without a
/// usable address.
fn instantly_leads() -> Vec<(String, Value)> {
    let lead = |id: &str, v: Value| {
        let mut v = v;
        v["id"] = json!(id);
        (DENTISTS.to_string(), v)
    };
    vec![
        lead("l1", json!({"email": "ana@delta.io", "first_name": "Ana", "last_name": "Silva", "company_name": "Delta", "job_title": "Head of Ops",
            "website": "https://delta.io", "status": 1, "personalization": "Loved the clinic tour video.",
            "payload": {"firstName": "Ana", "City": "Leeds", "Chairs": 6}})),
        lead("l2", json!({"email": "bo@acme.com", "first_name": "Bo", "company_name": "Acme", "status": 1, "timestamp_last_contact": "2026-10-01T09:00:00.000Z"})),
        lead("l3", json!({"email": "cy@corp.com", "first_name": "Cy", "status": 3, "email_reply_count": 1, "lt_interest_status": 1,
            "timestamp_last_contact": "2026-10-02T09:00:00.000Z"})),
        lead("l4", json!({"email": "di@bounce.com", "first_name": "Di", "status": -1, "timestamp_last_contact": "2026-10-03T09:00:00.000Z"})),
        lead("l5", json!({"email": "ed@unsub.com", "first_name": "Ed", "status": -2, "timestamp_last_contact": "2026-10-04T09:00:00.000Z"})),
        lead("l6", json!({"email": "fay@blocked.io", "first_name": "Fay", "status": 1})),
        lead("l7", json!({"email": "not-an-email", "status": 1})),
    ]
}

async fn instantly_lead_page(State(fake): State<Fake>, Json(body): Json<Value>) -> Json<Value> {
    let campaign = body["campaign"].as_str().unwrap_or_default().to_string();
    let leads: Vec<Value> = fake.instantly_leads.lock().unwrap().iter().filter(|(c, _)| *c == campaign).map(|(_, l)| l.clone()).collect();
    Json(page_after(&leads, body["starting_after"].as_str(), 2, "id"))
}

fn smartlead_sequences() -> Value {
    json!([
        {"id": 2, "seq_number": 2, "subject": "", "email_body": "<p>Following up, {{first_name}}.</p>", "seq_delay_details": {"delayInDays": 2}},
        {"id": 1, "seq_number": 1, "subject": "Quick question, {{first_name}}", "email_body": "<p>Hi {{first_name}},</p><p>How does {{company_name}} handle {{pain}}?</p>",
         "seq_delay_details": {"delayInDays": 0}},
    ])
}

/// Three leads, two a page whatever the limit, with the total as the docs show it.
async fn smartlead_lead_page(Query(q): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let leads = [
        json!({"status": "INPROGRESS", "last_sent_time": "2026-10-05T10:00:00Z", "email": "sam@saas.io", "first_name": "Sam", "company_name": "SaaS Co",
               "custom_fields": {"Job Title": "Founder"}}),
        json!({"status": "STARTED", "email": "tia@saas.io", "first_name": "Tia", "company_name": "SaaS Co"}),
        json!({"status": "COMPLETED", "email": "uma@gone.io", "first_name": "Uma", "email_stats": {"is_bounced": true}}),
    ];
    let offset: usize = q.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let limit: usize = q.get("limit").and_then(|o| o.parse().ok()).unwrap_or(100).min(2);
    let page: Vec<&Value> = leads.iter().skip(offset).take(limit).collect();
    Json(json!({ "total": leads.len(), "leads": page, "offset": offset, "limit": limit }))
}

/// 101 campaigns, one more than a page.
async fn lemlist_campaigns(Query(q): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let offset: usize = q.get("offset").and_then(|o| o.parse().ok()).unwrap_or(0);
    let limit: usize = q.get("limit").and_then(|o| o.parse().ok()).unwrap_or(100);
    let all: Vec<Value> = (1..=101).map(|i| json!({"_id": format!("cam_{i}"), "name": format!("Campaign {i}"), "status": "running"})).collect();
    Json(json!(all.into_iter().skip(offset).take(limit).collect::<Vec<_>>()))
}

async fn lemlist_reports(Query(q): Query<std::collections::HashMap<String, String>>) -> Json<Value> {
    let ids = q.get("campaignIds").cloned().unwrap_or_default();
    Json(json!(ids.split(',').map(|id| json!({"_id": id, "totalCount": if id == "cam_1" { 2 } else { 0 }})).collect::<Vec<_>>()))
}

fn lemlist_sequences() -> Value {
    json!({"seq_a": {"_id": "seq_a", "steps": [
        {"_id": "stp_1", "type": "email", "index": 0, "delay": 0, "subject": "{{companyName}} + Fieldnote", "message": "<p>Hi {{firstName}},</p><p>{{icebreaker}}</p>"},
        {"_id": "stp_2", "type": "linkedinVisit", "index": 1, "delay": 1},
        {"_id": "stp_3", "type": "email", "index": 2, "delay": 2, "subject": "", "message": "<p>Worth a look, {{firstName}}?</p>"},
    ]}})
}

fn lemlist_leads() -> Value {
    json!([
        {"_id": "lea_1", "email": "vic@lem.io", "firstName": "Vic", "companyName": "Lem", "jobTitle": "CEO", "icebreaker": "Congrats on the round.",
         "lastState": "emailsReplied", "status": "done"},
        {"_id": "lea_2", "email": "gone@lemlist-lead.com", "firstName": "Wes", "lastState": "emailsUnsubscribed", "status": "done"},
    ])
}
