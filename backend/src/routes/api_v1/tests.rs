use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use reqwest::Method;
use reqwest::header::HeaderMap;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use crate::crypto::sha256_hex;
use crate::rate_limit::{LIMITS, Limits, RateLimiter};
use crate::state::AppState;

const KEYS: [&str; 2] = ["splee_idempotency_a", "splee_idempotency_b"];

#[derive(Clone)]
struct Api {
    base: String,
    client: reqwest::Client,
}

impl Api {
    async fn start(db: &PgPool, limits: Limits) -> Self {
        Self::start_with(db, limits, Arc::new(crate::company_filters::Claude)).await
    }

    async fn start_with(db: &PgPool, limits: Limits, model: Arc<dyn crate::company_filters::Model>) -> Self {
        let state = AppState { rate_limits: RateLimiter::new(limits), ..crate::autopilot::tests::state(db.clone()) };
        Self::serve(state, model).await
    }

    async fn serve(state: AppState, model: Arc<dyn crate::company_filters::Model>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", super::router_with(&state, model)).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { base, client: reqwest::Client::new() }
    }

    async fn send(&self, method: Method, key: &str, path: &str, idempotency_key: Option<&str>, body: Option<Value>) -> (u16, HeaderMap, Value) {
        let mut req = self.client.request(method, format!("{}{path}", self.base)).bearer_auth(key);
        if let Some(k) = idempotency_key {
            req = req.header("idempotency-key", k);
        }
        if let Some(body) = body {
            req = req.json(&body);
        }
        let res = req.send().await.unwrap();
        let (status, headers) = (res.status().as_u16(), res.headers().clone());
        (status, headers, res.json().await.unwrap_or(Value::Null))
    }

    async fn post(&self, key: &str, path: &str, idempotency_key: Option<&str>, body: Value) -> (u16, HeaderMap, Value) {
        self.send(Method::POST, key, path, idempotency_key, Some(body)).await
    }

    async fn get(&self, path: &str) -> (u16, Value) {
        let (status, _, body) = self.send(Method::GET, KEYS[0], path, None, None).await;
        (status, body)
    }

    async fn patch(&self, path: &str, body: Value) -> (u16, Value) {
        let (status, _, body) = self.send(Method::PATCH, KEYS[0], path, None, Some(body)).await;
        (status, body)
    }
}

/// Two organizations, each with a project and an API key from KEYS.
async fn seed(db: &PgPool) -> [Uuid; 2] {
    let mut projects = vec![];
    for (name, key) in [("Acme", KEYS[0]), ("Other", KEYS[1])] {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
            .bind(org)
            .bind(name)
            .fetch_one(db)
            .await
            .unwrap();
        sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'test', 'splee_idempo', $2)")
            .bind(org)
            .bind(sha256_hex(key))
            .execute(db)
            .await
            .unwrap();
        projects.push(project);
    }
    [projects[0], projects[1]]
}

/// A campaign in the project with one lead and a first email waiting as a draft. Returns the campaign and the email.
async fn campaign(db: &PgPool, project: Uuid, status: &str) -> (Uuid, Uuid) {
    let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Dentists', $2) returning id")
        .bind(project)
        .bind(status)
        .fetch_one(db)
        .await
        .unwrap();
    let email: Uuid = sqlx::query_scalar(
        "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Ana', 'ana@smile.example') returning id)
         insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
         select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'draft' from l returning id",
    )
    .bind(project)
    .bind(segment)
    .fetch_one(db)
    .await
    .unwrap();
    (segment, email)
}

async fn count(db: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(db).await.unwrap()
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_retry_with_the_same_key_replays_the_first_response_and_runs_nothing(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let campaigns = format!("/v1/projects/{a}/campaigns");
    let leeds = json!({ "prompt": "dental clinics in Leeds" });

    let (status, headers, first) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None), "{first}");
    let (status, headers, again) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
    assert_eq!((status, header(&headers, "idempotent-replayed"), &again), (200, Some("true"), &first));
    assert_eq!(header(&headers, "content-type"), Some("application/json"));
    let made = count(&db, "select (select count(*) from segments) + (select count(*) from agent_runs)").await;
    assert_eq!(made, 2, "one campaign and its run");

    // The same key on another request is a client bug, never a replay.
    let (status, _, body) = api.post(KEYS[0], &campaigns, Some("order-1"), json!({ "prompt": "gyms in York" })).await;
    assert_eq!(status, 422, "{body}");
    assert!(body["error"].as_str().unwrap().contains("different request"));
    assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{a}/leads"), Some("order-1"), leeds.clone()).await.0, 422);
    let campaign = first["campaign_id"].as_str().unwrap();
    assert_eq!(api.send(Method::PATCH, KEYS[0], &format!("/v1/campaigns/{campaign}"), Some("order-1"), Some(leeds.clone())).await.0, 422);

    // Keys are per organization.
    let (status, headers, theirs) = api.post(KEYS[1], &format!("/v1/projects/{b}/campaigns"), Some("order-1"), leeds.clone()).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
    assert_ne!(theirs["campaign_id"], first["campaign_id"]);

    // A 4xx is kept too, so a retry gets the same answer.
    let (status, _, refused) = api.post(KEYS[0], &campaigns, Some("order-2"), json!({ "prompt": " " })).await;
    assert_eq!(status, 400);
    let (status, headers, replayed) = api.post(KEYS[0], &campaigns, Some("order-2"), json!({ "prompt": " " })).await;
    assert_eq!((status, header(&headers, "idempotent-replayed"), replayed), (400, Some("true"), refused));
    assert_eq!(api.post(KEYS[0], &campaigns, Some(&"k".repeat(256)), leeds.clone()).await.0, 400);

    // Without the header nothing changes: each request runs and nothing is kept.
    let before = count(&db, "select count(*) from idempotency_keys").await;
    for _ in 0..2 {
        let (status, headers, _) = api.post(KEYS[0], &campaigns, None, leeds.clone()).await;
        assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
    }
    assert_eq!(count(&db, "select count(*) from segments").await, 4);
    assert_eq!(count(&db, "select count(*) from idempotency_keys").await, before);
    // A read ignores the header.
    let (status, headers, _) = api.send(Method::GET, KEYS[0], &campaigns, Some("order-1"), None).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));

    sqlx::query("update idempotency_keys set expires_at = now() - interval '1 second'").execute(&db).await.unwrap();
    crate::agent::expire_visitor_data(&db).await.unwrap();
    assert_eq!(count(&db, "select count(*) from idempotency_keys").await, 0);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_duplicate_while_the_first_runs_is_409_and_a_5xx_releases_the_key(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let campaigns = format!("/v1/projects/{a}/campaigns");
    let leeds = json!({ "prompt": "dental clinics in Leeds" });

    // Holds the first request at its insert into segments, after it claimed the key.
    let mut lock = db.begin().await.unwrap();
    sqlx::query("lock table segments in exclusive mode").execute(&mut *lock).await.unwrap();
    let first = tokio::spawn({
        let (api, campaigns, leeds) = (api.clone(), campaigns.clone(), leeds.clone());
        async move { api.post(KEYS[0], &campaigns, Some("order-1"), leeds).await }
    });
    for _ in 0..100 {
        if count(&db, "select count(*) from idempotency_keys where status is null").await == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let (status, _, body) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
    assert_eq!(status, 409, "{body}");
    lock.rollback().await.unwrap();
    let (status, _, made) = first.await.unwrap();
    assert_eq!(status, 200, "{made}");
    let (status, headers, again) = api.post(KEYS[0], &campaigns, Some("order-1"), leeds.clone()).await;
    assert_eq!((status, header(&headers, "idempotent-replayed"), again), (200, Some("true"), made));

    // A 5xx isn't kept, so the retry runs.
    let refuse = "create function refuse() returns trigger language plpgsql as $$ begin raise exception 'refused'; end $$";
    sqlx::query(refuse).execute(&db).await.unwrap();
    sqlx::query("create trigger refuse before insert on segments for each statement execute function refuse()").execute(&db).await.unwrap();
    assert_eq!(api.post(KEYS[0], &campaigns, Some("order-2"), leeds.clone()).await.0, 500);
    assert_eq!(count(&db, "select count(*) from idempotency_keys where key = 'order-2'").await, 0);
    sqlx::query("drop trigger refuse on segments").execute(&db).await.unwrap();
    let (status, headers, _) = api.post(KEYS[0], &campaigns, Some("order-2"), leeds).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None));
    assert_eq!(count(&db, "select count(*) from segments").await, 2);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_description_becomes_filters_within_the_organizations_hour(db: PgPool) {
    use crate::company_filters::tests::{Fake, leeds_dentists, seed as seed_directory};

    seed(&db).await;
    seed_directory(&db).await;
    let answer = crate::company_filters::Answer { note: "Company size isn't a filter.".into(), ..leeds_dentists() };
    let api = Api::start_with(&db, LIMITS, Arc::new(Fake::new(answer))).await;
    let path = "/v1/companies/filters";
    let query = json!({ "query": "dentists in Leeds with an email that use Shopify, 10 to 50 staff" });

    let (status, _, body) = api.post(KEYS[0], path, None, query.clone()).await;
    assert_eq!(status, 200, "{body}");
    let filters = json!({ "q": null, "country": "GB", "city": "Leeds", "category": "dental_clinic", "has_email": true, "has_phone": false, "tech": "Shopify" });
    assert_eq!(body, json!({ "filters": filters, "note": "Company size isn't a filter." }));
    assert_eq!(api.post(KEYS[0], path, None, json!({ "query": " " })).await.0, 400);
    assert_eq!(api.post(KEYS[0], path, None, json!({})).await.0, 422);
    assert_eq!(api.post("splee_wrong", path, None, query.clone()).await.0, 401);

    for _ in 1..30 {
        assert_eq!(api.post(KEYS[0], path, None, query.clone()).await.0, 200);
    }
    let (status, headers, body) = api.post(KEYS[0], path, None, query.clone()).await;
    assert_eq!(status, 429, "{body}");
    assert!(header(&headers, "retry-after").unwrap().parse::<u64>().unwrap() > 3500);
    assert!(body["error"].as_str().unwrap().contains("30 searches from a description this hour"), "{body}");
    assert_eq!(api.post(KEYS[1], path, None, query).await.0, 200, "another organization has its own hour");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn each_organization_gets_its_hour_of_requests_then_a_429(db: PgPool) {
    seed(&db).await;
    let api = Api::start(&db, Limits { per_hour: 2, in_flight: 150, what: "API requests" }).await;

    for remaining in ["1", "0"] {
        let (status, headers, _) = api.send(Method::GET, KEYS[0], "/v1/projects", None, None).await;
        assert_eq!((status, header(&headers, "ratelimit-limit"), header(&headers, "ratelimit-remaining")), (200, Some("2"), Some(remaining)));
        let reset: u64 = header(&headers, "ratelimit-reset").unwrap().parse().unwrap();
        assert!((3500..=3600).contains(&reset), "{reset}");
    }
    let (status, headers, body) = api.send(Method::GET, KEYS[0], "/v1/projects", None, None).await;
    assert_eq!((status, header(&headers, "ratelimit-remaining")), (429, Some("0")));
    assert!(header(&headers, "retry-after").unwrap().parse::<u64>().unwrap() > 3500);
    assert!(body["error"].as_str().unwrap().contains("2 API requests this hour"), "{body}");

    let (status, headers, _) = api.send(Method::GET, KEYS[1], "/v1/projects", None, None).await;
    assert_eq!((status, header(&headers, "ratelimit-remaining")), (200, Some("1")), "another organization has its own hour");
    let (status, headers, _) = api.send(Method::GET, "splee_wrong", "/v1/projects", None, None).await;
    assert_eq!((status, header(&headers, "ratelimit-limit")), (401, None));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaigns_start_stop_and_approve_like_the_app_buttons(db: PgPool) {
    let [a, b] = seed(&db).await;
    let (segment, _) = campaign(&db, a, "draft").await;
    let (theirs, _) = campaign(&db, b, "draft").await;
    let api = Api::start(&db, LIMITS).await;
    let state = |id: Uuid| {
        let db = db.clone();
        async move {
            sqlx::query_as::<_, (String, String, Option<String>, String)>(
                "select s.status, e.status, e.approved_via, l.status from segments s join email_messages e on e.segment_id = s.id
                 join leads l on l.id = e.lead_id where s.id = $1",
            )
            .bind(id)
            .fetch_one(&db)
            .await
            .unwrap()
        }
    };
    let row = |status: &str, email: &str, via: Option<&str>, lead: &str| (status.to_string(), email.to_string(), via.map(String::from), lead.to_string());

    let (status, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
    assert_eq!((status, detail["status"].as_str(), detail["project_id"].as_str()), (200, Some("draft"), Some(a.to_string().as_str())), "{detail}");
    assert!(detail["status_reason"].as_str().unwrap().starts_with("Not started"), "{detail}");
    assert_eq!((detail["schedule"]["sending"].clone(), detail["schedule"]["daily_email_limit"].is_number()), (json!(false), true));
    assert!(detail["follow_ups"].is_array() && detail["targeting"].is_object(), "{detail}");
    assert_eq!(detail["own_follow_ups"], false, "the project's until it has its own");
    let read_only: Vec<&str> = detail["read_only"].as_array().unwrap().iter().map(|r| r["field"].as_str().unwrap()).collect();
    assert_eq!(read_only, ["schedule"]);

    // Another organization's campaign looks like one that doesn't exist, and nothing happens to it.
    assert_eq!(api.get(&format!("/v1/campaigns/{theirs}")).await.0, 404);
    for action in ["start", "stop", "approve"] {
        assert_eq!(api.post(KEYS[0], &format!("/v1/campaigns/{theirs}/{action}"), None, json!({})).await.0, 404, "{action}");
    }
    assert_eq!(state(theirs).await, row("draft", "draft", None, "new"));

    // Start is the app's Start: the campaign sends and its drafts are approved. A retry with the key replays, and a
    // repeat without one changes nothing, so it's audited once, under the key.
    let start = format!("/v1/campaigns/{segment}/start");
    let (status, headers, body) = api.post(KEYS[0], &start, Some("start-1"), json!({})).await;
    assert_eq!((status, body, header(&headers, "idempotent-replayed")), (200, json!({ "accepted": true }), None));
    assert_eq!(state(segment).await, row("working", "approved", Some("person"), "queued"));
    let (status, headers, _) = api.post(KEYS[0], &start, Some("start-1"), json!({})).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, Some("true")));
    sqlx::query("update segments set status = 'scaling' where id = $1").bind(segment).execute(&db).await.unwrap();
    assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.2, json!({ "accepted": true }));
    assert_eq!(state(segment).await.0, "scaling", "starting a sending campaign leaves it as it is");
    let audited = "select count(*) from audit_events where action = 'campaign.launched' and actor_key_id is not null";
    assert_eq!(count(&db, audited).await, 1);

    // Stop is the app's Pause: unsent emails go back to drafts. Twice is the same as once.
    for _ in 0..2 {
        let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/stop"), None, json!({})).await;
        assert_eq!((status, body), (200, json!({ "accepted": true })));
        assert_eq!(state(segment).await, row("paused", "draft", None, "new"));
    }
    assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.paused'").await, 1);
    let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
    assert!(detail["status_reason"].as_str().unwrap().starts_with("Paused."), "{detail}");
    // The optimizer paused it after the person last did.
    sqlx::query("update segments set auto_paused_at = now() + interval '1 minute' where id = $1").bind(segment).execute(&db).await.unwrap();
    let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
    assert!(detail["status_reason"].as_str().unwrap().contains("optimizer"), "{detail}");

    // Approve is each draft's Approve button. A paused campaign keeps them until it starts.
    let approve = format!("/v1/campaigns/{segment}/approve");
    assert_eq!(api.post(KEYS[0], &approve, None, json!({})).await.2, json!({ "accepted": true, "approved": 1 }));
    assert_eq!(state(segment).await, row("paused", "approved", Some("person"), "queued"));
    assert_eq!(api.post(KEYS[0], &approve, None, json!({})).await.2, json!({ "accepted": true, "approved": 0 }));

    // An imported campaign's emails are sent as written.
    sqlx::query("update segments set imported_from = 'instantly' where id = $1").bind(segment).execute(&db).await.unwrap();
    sqlx::query("insert into segment_steps (segment_id, position, subject, body) values ($1, 0, 'Quick one', 'Hi {first_name}')")
        .bind(segment)
        .execute(&db)
        .await
        .unwrap();
    let (_, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
    assert_eq!((detail["imported_from"].as_str(), detail["sequence"][0]["body"].as_str()), (Some("Instantly"), Some("Hi {first_name}")));
    let read_only: Vec<&str> = detail["read_only"].as_array().unwrap().iter().map(|r| r["field"].as_str().unwrap()).collect();
    assert_eq!(read_only, ["schedule", "sequence", "follow_ups"]);
    let steps = json!([{ "delay_days": 3, "enabled": true }]);
    assert_eq!(api.patch(&format!("/v1/campaigns/{segment}"), json!({ "follow_ups": steps })).await.0, 400, "it sends the steps it came with");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_campaign_patch_changes_only_the_fields_it_sends(db: PgPool) {
    let [a, b] = seed(&db).await;
    let (segment, _) = campaign(&db, a, "draft").await;
    let (theirs, _) = campaign(&db, b, "draft").await;
    sqlx::query("update segments set targeting = $2, angle = 'Fewer no-shows' where id = $1")
        .bind(segment)
        .bind(json!({ "roles": ["Practice Manager"], "countries": ["GB"] }))
        .execute(&db)
        .await
        .unwrap();
    let api = Api::start(&db, LIMITS).await;
    let path = format!("/v1/campaigns/{segment}");

    let body = json!({
        "name": " Leeds dentists ",
        "targeting": { "countries": ["GB", "IE"], "keywords": [] },
        "pain_points": "Empty chairs",
        "track_opens": true,
        "language": "de",
    });
    let (status, detail) = api.patch(&path, body).await;
    assert_eq!((status, detail["name"].as_str(), detail["language"].as_str()), (200, Some("Leeds dentists"), Some("de")), "{detail}");
    assert_eq!(detail["targeting"], json!({ "roles": ["Practice Manager"], "countries": ["GB", "IE"], "keywords": [] }));
    assert_eq!((detail["pain_points"].as_str(), detail["angle"].as_str()), (Some("Empty chairs"), Some("Fewer no-shows")));
    assert_eq!((detail["track_opens"].clone(), detail["read_only"][0]["field"].clone()), (json!(true), json!("schedule")), "{detail}");
    assert_eq!(detail["daily_limit"], Value::Null, "no limit of its own until one is sent");
    assert_eq!(api.patch(&path, json!({ "daily_limit": 25 })).await.1["daily_limit"], 25);
    assert_eq!(api.patch(&path, json!({ "angle": "Fewer no-shows" })).await.1["daily_limit"], 25, "left out, it stays");
    assert_eq!(api.patch(&path, json!({ "daily_limit": null })).await.1["daily_limit"], Value::Null, "null takes it off");
    let own = json!([{ "kind": "call", "delay_days": 2, "enabled": true, "instructions": "Ask about the new clinic" }]);
    let (_, detail) = api.patch(&path, json!({ "follow_ups": own })).await;
    assert_eq!((detail["follow_ups"].clone(), detail["own_follow_ups"].clone()), (own, json!(true)), "{detail}");
    let (_, detail) = api.patch(&path, json!({ "follow_ups": null })).await;
    assert_eq!((detail["own_follow_ups"].clone(), detail["follow_ups"].is_array()), (json!(false), true), "null goes back to the project's");

    // A refused body changes nothing, and another organization's campaign looks like one that doesn't exist.
    for (body, status) in [
        (json!({ "name": " " }), 422),
        (json!({ "angle": "New", "targeting": { "role": ["CEO"] } }), 422),
        (json!({ "angle": "New", "schedule": { "send_days": [1] } }), 422),
        (json!({ "angle": "New", "language": "xx" }), 400),
        (json!({ "angle": "New", "daily_limit": 0 }), 400),
        (json!({ "angle": "New", "daily_limit": 5001 }), 400),
        (json!({ "angle": "New", "follow_ups": [{ "delay_days": 0, "enabled": true }] }), 400),
        (json!({ "angle": "New", "follow_ups": [{ "delay_days": 3 }] }), 400),
        (json!({ "angle": "New", "follow_ups": [{ "kind": "text", "delay_days": 3, "enabled": true }] }), 400),
    ] {
        assert_eq!(api.patch(&path, body.clone()).await.0, status, "{body}");
    }
    assert_eq!(api.patch(&format!("/v1/campaigns/{theirs}"), json!({ "name": "Mine now" })).await.0, 404);
    let names: Vec<(String, String)> =
        sqlx::query_as("select name, angle from segments order by name").fetch_all(&db).await.unwrap();
    assert_eq!(names, [("Dentists".into(), "".into()), ("Leeds dentists".into(), "Fewer no-shows".into())]);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_archived_campaign_stops_sending_and_leaves_the_list_until_it_comes_back_paused(db: PgPool) {
    let [a, b] = seed(&db).await;
    let (segment, email) = campaign(&db, a, "draft").await;
    let (theirs, _) = campaign(&db, b, "working").await;
    let api = Api::start(&db, LIMITS).await;
    let campaigns = format!("/v1/projects/{a}/campaigns");
    let ids = |body: Value| body.as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let email_status = || {
        let db = db.clone();
        async move { sqlx::query_scalar::<_, String>("select status from email_messages where id = $1").bind(email).fetch_one(&db).await.unwrap() }
    };
    let start = format!("/v1/campaigns/{segment}/start");
    assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.0, 200);
    assert_eq!(email_status().await, "approved");

    // Archiving stops it like Stop and takes it off the list. Twice is the same as once.
    for _ in 0..2 {
        let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/archive"), None, json!({})).await;
        assert_eq!((status, body["status"].as_str(), body["archived_at"].is_string()), (200, Some("paused"), true), "{body}");
    }
    assert_eq!(email_status().await, "draft");
    assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.archived' and actor_key_id is not null").await, 1);
    assert_eq!(ids(api.get(&campaigns).await.1), Vec::<String>::new());
    assert_eq!(ids(api.get(&format!("{campaigns}?archived=true")).await.1), [segment.to_string()]);
    let (status, detail) = api.get(&format!("/v1/campaigns/{segment}")).await;
    assert_eq!((status, detail["leads"].as_i64(), detail["archived_at"].is_string()), (200, Some(1), true), "its leads stay: {detail}");
    let (status, _, body) = api.post(KEYS[0], &start, None, json!({})).await;
    assert_eq!(status, 409, "it can't start while archived: {body}");
    assert_eq!(email_status().await, "draft");
    let lead = json!({ "email": "new@acme.example", "full_name": "New Lead", "segment_id": segment });
    let (_, _, added) = api.post(KEYS[0], &format!("/v1/projects/{a}/leads"), None, json!({ "leads": [lead.clone()] })).await;
    assert_eq!((added["imported"].as_i64(), added["errors"][0]["error"].as_str()), (Some(0), Some("Unknown or archived campaign")), "{added}");
    assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{a}/leads/upsert"), None, lead).await.0, 400, "no leads go into it");

    // Unarchiving lists it again, paused, so nothing goes out until someone starts it.
    for _ in 0..2 {
        let (status, _, body) = api.post(KEYS[0], &format!("/v1/campaigns/{segment}/unarchive"), None, json!({})).await;
        assert_eq!((status, body["status"].as_str(), body["archived_at"].is_null()), (200, Some("paused"), true), "{body}");
    }
    assert_eq!((ids(api.get(&campaigns).await.1), email_status().await), (vec![segment.to_string()], "draft".to_string()));
    assert_eq!(count(&db, "select count(*) from audit_events where action = 'campaign.unarchived'").await, 1);
    assert_eq!(api.post(KEYS[0], &start, None, json!({})).await.0, 200);
    assert_eq!(email_status().await, "approved");

    // Another organization's campaign looks like one that doesn't exist, and keeps sending.
    for action in ["archive", "unarchive"] {
        assert_eq!(api.post(KEYS[0], &format!("/v1/campaigns/{theirs}/{action}"), None, json!({})).await.0, 404, "{action}");
    }
    let untouched = format!("select count(*) from segments where id = '{theirs}' and status = 'working' and archived_at is null");
    assert_eq!(count(&db, &untouched).await, 1);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn sending_domains_are_suggested_and_checked_with_the_organizations_porkbun(db: PgPool) {
    let [a, _] = seed(&db).await;
    let (porkbun, _) = crate::porkbun::fake::start().await;
    let base = crate::autopilot::tests::state(db.clone());
    let cfg = crate::config::Config { porkbun_api_url: porkbun, ..(*base.cfg).clone() };
    let state = AppState { cfg: Arc::new(cfg), rate_limits: RateLimiter::new(LIMITS), ..base };
    let secret = state.cfg.app_secret.clone();
    let api = Api::serve(state, Arc::new(crate::company_filters::Claude)).await;

    let (status, body) = api.get("/v1/sending-domains/suggestions?domain=acme.com").await;
    assert_eq!((status, body["error"].as_str()), (400, Some("Connect your Porkbun account first")));
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
    sqlx::query("insert into registrar_accounts (org_id, account, api_key_enc, secret_enc) values ($1, 'acme-ops', $2, $3)")
        .bind(org)
        .bind(crate::crypto::encrypt(&secret, "pk1_good"))
        .bind(crate::crypto::encrypt(&secret, "sk1_good"))
        .execute(&db)
        .await
        .unwrap();

    let (status, found) = api.get("/v1/sending-domains/suggestions?domain=acme.com").await;
    assert_eq!((status, found[0]["domain"].as_str(), found[0]["available"].as_bool()), (200, Some("getacme.com"), Some(true)), "{found}");
    // Cleaned up and counted once, buyable first, then the rest in the order asked. A registry that didn't answer is null.
    let asked = "acmeslow.com,https://www.GetAcme.com/,acmetaken.com,getacme.com";
    let (status, found) = api.get(&format!("/v1/sending-domains/availability?domains={asked}")).await;
    assert_eq!(status, 200, "{found}");
    let summary: Vec<(&str, Option<bool>, i64)> = found.as_array().unwrap().iter().map(|a| {
        (a["domain"].as_str().unwrap(), a["available"].as_bool(), a["price_micros"].as_i64().unwrap())
    }).collect();
    assert_eq!(summary, [("getacme.com", Some(true), 11_080_000), ("acmeslow.com", None, 0), ("acmetaken.com", Some(false), 11_080_000)]);
    let too_many = (0..21).map(|i| format!("acme{i}.com")).collect::<Vec<_>>().join(",");
    for domains in ["", "acme.com,not a domain", too_many.as_str()] {
        assert_eq!(api.get(&format!("/v1/sending-domains/availability?domains={domains}")).await.0, 400, "{domains}");
    }

    sqlx::query("insert into sending_domains (org_id, domain, status, price_micros) values ($1, 'getacme.com', 'registered', 11080000)")
        .bind(org)
        .execute(&db)
        .await
        .unwrap();
    let (status, owned) = api.get("/v1/sending-domains").await;
    assert_eq!((status, owned[0]["domain"].as_str(), owned[0]["status"].as_str()), (200, Some("getacme.com"), Some("registered")), "{owned}");
    assert_eq!(api.send(Method::GET, KEYS[1], "/v1/sending-domains", None, None).await.2, json!([]));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_project_is_made_and_read_with_its_sender_name(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;

    // New project, as in the app: the website is cleaned up and names it, and an explore run researches the business.
    assert_eq!(api.post(KEYS[0], "/v1/projects", None, json!({})).await.0, 400);
    let (status, _, made) = api.post(KEYS[0], "/v1/projects", None, json!({ "domain": "https://www.Fieldnote.example/about" })).await;
    assert_eq!(status, 200, "{made}");
    let id: Uuid = made["project_id"].as_str().unwrap().parse().unwrap();
    let run: Uuid = made["run_id"].as_str().unwrap().parse().unwrap();
    let (kind, project): (String, Uuid) =
        sqlx::query_as("select kind, project_id from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
    assert_eq!((kind.as_str(), project), ("explore", id));
    let (status, project) = api.get(&format!("/v1/projects/{id}")).await;
    assert_eq!((status, project["domain"].as_str(), project["name"].as_str()), (200, Some("fieldnote.example"), Some("fieldnote.example")));
    assert_eq!((project["controls"]["sending"].clone(), project["controls"]["sender_name"].clone()), (json!(false), json!("")));
    assert_eq!(api.get(&format!("/v1/projects/{b}")).await.0, 404);

    // The sender name is a name: its spaces collapse, and an address or one over 100 characters is refused.
    let path = format!("/v1/projects/{a}");
    let (status, controls) = api.patch(&path, json!({ "sender_name": "  Maya   Patel " })).await;
    assert_eq!((status, controls["sender_name"].clone()), (200, json!("Maya Patel")), "{controls}");
    for name in ["maya@fieldnote.example".to_string(), "M".repeat(101), "Maya\nPatel".to_string()] {
        assert_eq!(api.patch(&path, json!({ "sender_name": name })).await.0, 400, "{name}");
    }
    assert_eq!(api.get(&path).await.1["controls"]["sender_name"], "Maya Patel");
    assert_eq!(api.patch(&path, json!({ "sender_name": "" })).await.1["sender_name"], "");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn project_controls_refuse_a_schedule_limit_or_cap_out_of_range(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let project = format!("/v1/projects/{a}");
    let day = |n: i64| (chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap() + chrono::Duration::days(n)).to_string();
    assert_eq!(api.patch(&project, json!({ "send_start_hour": 9, "send_end_hour": 17 })).await.0, 200);
    let before = api.get(&project).await.1["controls"].clone();

    for body in [
        json!({ "daily_email_limit": 0 }),
        json!({ "daily_email_limit": 5001 }),
        json!({ "daily_text_limit": 0 }),
        json!({ "daily_text_limit": 1001 }),
        json!({ "timezone": "Mars/Olympus_Mons" }),
        json!({ "send_days": [0, 1] }),
        json!({ "send_days": [8] }),
        json!({ "send_start_hour": 17, "send_end_hour": 9 }),
        json!({ "send_start_hour": 9, "send_end_hour": 25 }),
        json!({ "send_start_hour": -1 }),
        // A lone start or end is checked against the saved other end, 17 and 9.
        json!({ "send_start_hour": 17 }),
        json!({ "send_end_hour": 9 }),
        json!({ "autopilot_budget_micros": -1 }),
        json!({ "autopilot_budget_micros": 100_000_000_001_i64 }),
        json!({ "autopilot_lead_cap": -1 }),
        json!({ "autopilot_lead_cap": 100_001 }),
        json!({ "review_window_hours": 0 }),
        json!({ "review_window_hours": 73 }),
        json!({ "monthly_cap_micros": 999_999 }),
        json!({ "monthly_cap_micros": 1_000_000_000_001_i64 }),
        json!({ "days_off": (0..101).map(day).collect::<Vec<_>>() }),
    ] {
        assert_eq!(api.patch(&project, body.clone()).await.0, 400, "{body}");
    }
    assert_eq!(api.get(&project).await.1["controls"], before, "a refusal saves nothing");

    // Each end of each range is allowed.
    for body in [
        json!({ "daily_email_limit": 1, "daily_text_limit": 1000, "send_start_hour": 0, "send_end_hour": 1, "autopilot_budget_micros": 0,
                "autopilot_lead_cap": 0, "review_window_hours": 1, "monthly_cap_micros": 1_000_000_000_000_i64, "days_off": (0..100).map(day).collect::<Vec<_>>() }),
        json!({ "daily_email_limit": 5000, "daily_text_limit": 1, "timezone": "Asia/Kathmandu", "send_days": [1, 7], "send_start_hour": 23, "send_end_hour": 24,
                "autopilot_budget_micros": 100_000_000_000_i64, "autopilot_lead_cap": 100_000, "review_window_hours": 72, "monthly_cap_micros": 1_000_000 }),
    ] {
        let (status, controls) = api.patch(&project, body.clone()).await;
        assert_eq!(status, 200, "{body} {controls}");
        for (field, value) in body.as_object().unwrap() {
            assert_eq!(&controls[field], value, "{field} was saved");
        }
    }
    // Days off are kept in order, once each.
    let (_, controls) = api.patch(&project, json!({ "days_off": [day(9), day(0), day(9)] })).await;
    assert_eq!(controls["days_off"], json!([day(0), day(9)]));
    // A null clears the review window and the monthly cap, rather than leaving them as they were.
    assert_eq!(api.patch(&project, json!({ "review_window_hours": null, "monthly_cap_micros": null })).await.0, 200);
    let controls = api.get(&project).await.1["controls"].clone();
    assert_eq!((controls["review_window_hours"].clone(), controls["monthly_cap_micros"].clone()), (Value::Null, Value::Null));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn project_controls_take_only_fields_they_know(db: PgPool) {
    let [a, b] = seed(&db).await;
    let (segment, _) = campaign(&db, a, "draft").await;
    let api = Api::start(&db, LIMITS).await;
    let project = format!("/v1/projects/{a}");

    let (status, controls) = api.patch(&project, json!({ "daily_email_limit": 80, "review_window_hours": 24, "autopilot": true })).await;
    assert_eq!(status, 200, "{controls}");
    assert_eq!((controls["daily_email_limit"].clone(), controls["review_window_hours"].clone(), controls["autopilot"].clone()), (json!(80), json!(24), json!(true)));
    assert_eq!(controls["sending"], false);
    let (status, body) = api.patch(&project, json!({ "daily_limit": 5 })).await;
    assert!(status == 422 && body["error"].as_str().unwrap().contains("unknown field `daily_limit`"), "{status} {body}");
    assert_eq!(api.patch(&project, json!({ "daily_email_limit": 0 })).await.0, 400);
    assert_eq!(api.patch(&format!("/v1/projects/{b}"), json!({ "daily_email_limit": 70 })).await.0, 404);
    let (status, unchanged) = api.patch(&project, json!({})).await;
    assert_eq!((status, unchanged["daily_email_limit"].clone()), (200, json!(80)));

    // Turning sending on needs a mailbox, like the app's Start sending, and a refusal saves nothing.
    let (status, body) = api.patch(&project, json!({ "sending": true, "daily_email_limit": 90 })).await;
    assert!(status == 409 && body["error"].as_str().unwrap().contains("mailbox"), "{status} {body}");
    assert_eq!(api.patch(&project, json!({})).await.1["daily_email_limit"], 80);
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
    sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'ana@acme.example', 'smtp.test', '', '')")
        .bind(org)
        .execute(&db)
        .await
        .unwrap();
    for _ in 0..2 {
        let (status, controls) = api.patch(&project, json!({ "sending": true })).await;
        assert_eq!((status, controls["sending"].clone()), (200, json!(true)), "{controls}");
    }
    assert_eq!(count(&db, "select count(*) from audit_events where action = 'project.started' and actor_key_id is not null").await, 1);
    let (_, controls) = api.patch(&project, json!({ "sending": false, "review_window_hours": null })).await;
    assert_eq!((controls["sending"].clone(), controls["review_window_hours"].clone()), (json!(false), Value::Null));
    let paused: bool = sqlx::query_scalar("select paused from project_settings where project_id = $1").bind(a).fetch_one(&db).await.unwrap();
    assert!(paused);

    // The monthly cap is set with the controls and read with what the month's emails cost.
    assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": 500_000 })).await.0, 400, "under $1");
    assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": 2_000_000 })).await.1["monthly_cap_micros"], 2_000_000);
    let cap = format!("{project}/monthly-cap");
    let want = json!({ "project_id": a, "monthly_cap_micros": 2_000_000, "spent_this_month_micros": 0, "reached": false, "resumes_at": null });
    assert_eq!(api.get(&cap).await, (200, want));
    assert_eq!(api.get(&format!("/v1/projects/{b}/monthly-cap")).await.0, 404);
    assert_eq!(api.patch(&project, json!({ "monthly_cap_micros": null })).await.1["monthly_cap_micros"], Value::Null);
    assert_eq!(api.patch(&project, json!({ "match_provider": true })).await.1["match_provider"], true);
    assert_eq!(api.patch(&project, json!({ "lead_time_zone": true })).await.1["lead_time_zone"], true);
    assert_eq!(api.patch(&project, json!({ "stop_company_on_reply": true })).await.1["stop_company_on_reply"], true);
    assert_eq!(api.patch(&project, json!({ "company_daily_limit": 2 })).await.1["company_daily_limit"], 2);
    assert_eq!(api.patch(&project, json!({ "company_daily_limit": 0 })).await.0, 400);
    assert_eq!(api.patch(&project, json!({ "company_daily_limit": null })).await.1["company_daily_limit"], Value::Null);
    let days_off = json!({ "days_off": ["2026-12-26", "2026-12-25", "2026-12-26"] });
    assert_eq!(api.patch(&project, days_off).await.1["days_off"], json!(["2026-12-25", "2026-12-26"]), "sorted, each once");
    let too_many: Vec<String> = (0..101).map(|d| (chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap() + chrono::Days::new(d)).to_string()).collect();
    assert_eq!(api.patch(&project, json!({ "days_off": too_many })).await.0, 400);

    // The campaign PATCH refuses fields it doesn't take too, rather than ignoring them.
    let (status, body) = api.patch(&format!("/v1/campaigns/{segment}"), json!({ "language": "fr", "sequence": [] })).await;
    assert_eq!(status, 422, "{body}");
    let language: String = sqlx::query_scalar("select language from segments where id = $1").bind(segment).fetch_one(&db).await.unwrap();
    assert_eq!(language, "lead");
}

/// A lead in the project with an inbound message `ago` before now, classified as given. Returns the lead.
async fn reply(db: &PgPool, project: Uuid, name: &str, classification: &str, ago: &str) -> Uuid {
    let lead: Uuid = sqlx::query_scalar(
        "insert into leads (project_id, full_name, email, title, company_name, company_domain, linkedin_url)
         values ($1, $2, $2 || '@acme.example', 'Founder', $2 || ' Co', $2 || '.example', 'https://www.linkedin.com/in/' || $2) returning id",
    )
    .bind(project)
    .bind(name)
    .fetch_one(db)
    .await
    .unwrap();
    another_reply(db, project, lead, classification, ago).await;
    lead
}

async fn another_reply(db: &PgPool, project: Uuid, lead: Uuid, classification: &str, ago: &str) -> Uuid {
    sqlx::query_scalar(
        "insert into email_messages (project_id, lead_id, direction, kind, subject, body, status, classification, classification_reason, created_at)
         values ($1, $2, 'inbound', 'inbound', 'Re', 'Reply ' || $4, 'received', $3, 'Wants a call', now() - $4::interval) returning id",
    )
    .bind(project)
    .bind(lead)
    .bind(classification)
    .bind(ago)
    .fetch_one(db)
    .await
    .unwrap()
}

fn at(db_time: crate::models::Ts) -> String {
    db_time.to_rfc3339_opts(chrono::SecondsFormat::Micros, true)
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn hot_leads_are_newest_first_and_honor_since_and_project(db: PgPool) {
    let [a, b] = seed(&db).await;
    let second: Uuid = sqlx::query_scalar("insert into projects (org_id, name) select org_id, 'Second' from projects where id = $1 returning id")
        .bind(a)
        .fetch_one(&db)
        .await
        .unwrap();
    let anna = reply(&db, a, "anna", "hot", "3 days").await;
    // A second hot reply doesn't make her new again.
    another_reply(&db, a, anna, "hot", "1 day").await;
    let bo = reply(&db, second, "bo", "hot", "2 days").await;
    reply(&db, a, "cy", "question", "1 hour").await;
    reply(&db, b, "dee", "hot", "1 hour").await;
    let api = Api::start(&db, LIMITS).await;

    let (status, page) = api.get("/v1/hot-leads").await;
    assert_eq!((status, page["total"].clone()), (200, json!(2)), "{page}");
    let ids: Vec<&str> = page["items"].as_array().unwrap().iter().map(|l| l["lead_id"].as_str().unwrap()).collect();
    assert_eq!(ids, [bo.to_string(), anna.to_string()]);
    let first = &page["items"][1];
    assert_eq!((first["name"].as_str(), first["email"].as_str()), (Some("anna"), Some("anna@acme.example")));
    assert_eq!((first["title"].as_str(), first["company_domain"].as_str()), (Some("Founder"), Some("anna.example")));
    assert_eq!((first["reply"].as_str(), first["why_hot"].as_str(), first["linkedin_url"].as_str()), (Some("Reply 3 days"), Some("Wants a call"), Some("https://www.linkedin.com/in/anna")));

    let since: crate::models::Ts = sqlx::query_scalar("select now() - interval '60 hours'").fetch_one(&db).await.unwrap();
    let (_, page) = api.get(&format!("/v1/hot-leads?since={}", at(since))).await;
    assert_eq!((page["total"].clone(), page["items"][0]["lead_id"].clone()), (json!(1), json!(bo)), "{page}");
    let (_, page) = api.get(&format!("/v1/hot-leads?project_id={a}")).await;
    assert_eq!((page["total"].clone(), page["items"][0]["lead_id"].clone()), (json!(1), json!(anna)), "{page}");
    assert_eq!(api.get(&format!("/v1/hot-leads?project_id={b}")).await.0, 404);
}

/// MillionVerifier as a project's verifier, through a fake that accepts only `valid`.
async fn verifier(db: &PgPool, project: Uuid, valid: &'static str) -> reqwest::Url {
    use std::collections::HashMap;

    use axum::Json;
    use axum::extract::Query;
    use axum::routing::get;

    sqlx::query("insert into project_settings (project_id, verifier, verifier_key_enc) values ($1, 'millionverifier', $2)")
        .bind(project)
        .bind(crate::crypto::encrypt("", "mv-key"))
        .execute(db)
        .await
        .unwrap();
    let check = move |Query(q): Query<HashMap<String, String>>| async move {
        Json(json!({ "result": if q["email"] == valid { "ok" } else { "invalid" }, "error": "" }))
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
    tokio::spawn(async move { axum::serve(listener, Router::new().route("/api/v3/", get(check))).await });
    url
}

async fn spend_the_hour(db: &PgPool, project: Uuid, searches: i32) {
    sqlx::query("insert into email_search_attempts (org_id) select org_id from projects, generate_series(1, $2) where id = $1")
        .bind(project)
        .bind(searches)
        .execute(db)
        .await
        .unwrap();
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn finding_an_email_needs_a_finder_and_room_in_the_hour(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let find = format!("/v1/projects/{a}/enrich/email");
    let jane = json!({ "first_name": "Jane", "last_name": "Doe", "company_domain": "https://www.acme.example/about" });

    let (status, _, body) = api.post(KEYS[0], &find, None, jane.clone()).await;
    assert!(status == 409 && body["error"].as_str().unwrap().contains("email finder or verifier"), "{status} {body}");
    let batch = json!({ "people": [jane.clone()] });
    assert_eq!(api.post(KEYS[0], &format!("{find}/batch"), None, batch.clone()).await.0, 409);
    verifier(&db, a, "").await;
    let (status, _, body) = api.post(KEYS[0], &find, None, json!({ "first_name": " ", "company_domain": "acme.example" })).await;
    assert!(status == 400 && body["error"].as_str().unwrap().contains("first_name"), "{status} {body}");

    // The leads the waterfall searched this hour count, and the 429 says when the oldest of them stops counting.
    spend_the_hour(&db, a, 200).await;
    let (status, headers, body) = api.post(KEYS[0], &find, Some("find-1"), jane.clone()).await;
    assert!(status == 429 && body["error"].as_str().unwrap().contains("200 email searches"), "{status} {body}");
    let wait: u64 = header(&headers, "retry-after").unwrap().parse().unwrap();
    assert!((3590..=3600).contains(&wait), "{wait}");
    assert_eq!(count(&db, "select count(*) from email_search_attempts").await, 200);
    // A batch waits for room instead.
    assert_eq!(api.post(KEYS[0], &format!("{find}/batch"), None, batch.clone()).await.0, 200);

    // Another organization's project and batch look like ones that don't exist.
    verifier(&db, b, "").await;
    assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/email"), None, jane.clone()).await.0, 404);
    assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/email/batch"), None, batch.clone()).await.0, 404);
    let (status, _, theirs) = api.post(KEYS[1], &format!("/v1/projects/{b}/enrich/email/batch"), None, batch).await;
    assert_eq!(status, 200, "{theirs}");
    let task = format!("/v1/enrich/email/batch/{}", theirs["task_id"].as_str().unwrap());
    assert_eq!(api.get(&task).await.0, 404);
    assert_eq!(api.send(Method::GET, KEYS[1], &task, None, None).await.0, 200);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
// The finders' answers are in waterfall.rs, since a real call here would reach the finder.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn finding_a_phone_needs_a_profile_a_finder_with_mobiles_and_room_in_the_hour(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let find = format!("/v1/projects/{a}/enrich/phone");
    let finder = |project: Uuid, provider: &'static str| {
        sqlx::query("insert into project_finders (project_id, provider, key_enc, position) values ($1, $2, $3, 0)")
            .bind(project)
            .bind(provider)
            .bind(crate::crypto::encrypt("", "key"))
            .execute(&db)
    };
    let sam = json!({ "linkedin_url": " https://www.linkedin.com/in/sam-lee/ ", "email": "Sam@Lee.example" });

    for bad in [json!({ "linkedin_url": "https://www.linkedin.com/company/acme" }), json!({ "linkedin_url": "https://www.linkedin.com/in/sam", "email": "sam" })] {
        let (status, _, body) = api.post(KEYS[0], &find, None, bad).await;
        assert_eq!(status, 400, "{body}");
    }
    // Hunter finds addresses only.
    finder(a, "hunter").await.unwrap();
    let (status, _, body) = api.post(KEYS[0], &find, None, sam.clone()).await;
    assert!(status == 409 && body["error"].as_str().unwrap().contains("People Data Labs, RocketReach, Lusha or Prospeo"), "{status} {body}");

    // With one, the hour still has to have room, and the 429 comes before any finder is called.
    finder(a, "peopledatalabs").await.unwrap();
    spend_the_hour(&db, a, 200).await;
    let (status, headers, body) = api.post(KEYS[0], &find, None, sam.clone()).await;
    assert!(status == 429 && body["error"].as_str().unwrap().contains("200 email searches"), "{status} {body}");
    assert!(header(&headers, "retry-after").is_some());

    // Another organization's project looks like one that doesn't exist.
    finder(b, "peopledatalabs").await.unwrap();
    assert_eq!(api.post(KEYS[0], &format!("/v1/projects/{b}/enrich/phone"), None, sam).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_batch_of_people_is_searched_in_the_background_within_the_hour(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let fake = verifier(&db, a, "jane.doe@acme.example").await;
    let state = crate::autopilot::tests::state(db.clone());
    let tick = || crate::verify::TEST_API.scope(fake.clone(), crate::email_lookups::tick(&state));
    let batch = format!("/v1/projects/{a}/enrich/email/batch");

    assert_eq!(api.post(KEYS[0], &batch, None, json!({ "people": [] })).await.0, 400);
    let too_many: Vec<Value> = (0..501).map(|_| json!({ "first_name": "Jane", "company_domain": "acme.example" })).collect();
    assert_eq!(api.post(KEYS[0], &batch, None, json!({ "people": too_many })).await.0, 400);
    let one_bad = json!({ "people": [{ "first_name": "Jane", "company_domain": "acme.example" }, { "first_name": "Sam", "company_domain": "nope" }] });
    let (status, _, body) = api.post(KEYS[0], &batch, None, one_bad).await;
    assert!(status == 400 && body["error"].as_str().unwrap().starts_with("people[1]"), "{status} {body}");

    let people = json!({ "people": [
        { "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example", "client_ref": "crm-1" },
        { "first_name": "Sam", "last_name": "Lee", "company_domain": "lee.example" },
    ] });
    let (status, _, started) = api.post(KEYS[0], &batch, None, people).await;
    assert_eq!((status, started["status"].as_str(), started["total"].as_i64()), (200, Some("pending"), Some(2)), "{started}");
    let task = format!("/v1/enrich/email/batch/{}", started["task_id"].as_str().unwrap());
    let (_, waiting) = api.get(&task).await;
    assert_eq!((waiting["status"].as_str(), waiting["searched"].as_i64()), (Some("pending"), Some(0)), "{waiting}");
    assert_eq!((waiting["results"][0]["status"].as_str(), waiting["results"][0]["client_ref"].as_str()), (Some("pending"), Some("crm-1")));

    tick().await.unwrap();
    let (status, done) = api.get(&task).await;
    assert_eq!((status, done["status"].as_str(), done["searched"].as_i64()), (200, Some("completed"), Some(2)), "{done}");
    let jane = &done["results"][0];
    let found = (jane["status"].as_str(), jane["email"].as_str(), jane["email_status"].as_str());
    assert_eq!(found, (Some("guessed"), Some("jane.doe@acme.example"), Some("verified")));
    assert_eq!((jane["source"].as_str(), jane["client_ref"].as_str(), jane["phone"].clone()), (Some("first.last"), Some("crm-1"), Value::Null));
    let sam = &done["results"][1];
    assert_eq!((sam["status"].as_str(), sam["email"].clone(), sam["client_ref"].as_str()), (Some("not_found"), Value::Null, Some("")), "{sam}");
    assert_eq!(count(&db, "select count(*) from email_search_attempts").await, 2);
    assert_eq!(count(&db, "select count(*) from leads").await, 0, "nobody became a lead");

    // With the hour spent a batch waits, and goes on once there's room.
    spend_the_hour(&db, a, 198).await;
    let jane = json!({ "people": [{ "first_name": "Jane", "last_name": "Doe", "company_domain": "acme.example" }] });
    let (_, _, started) = api.post(KEYS[0], &batch, None, jane).await;
    let task = format!("/v1/enrich/email/batch/{}", started["task_id"].as_str().unwrap());
    tick().await.unwrap();
    assert_eq!(api.get(&task).await.1["status"], "pending");
    sqlx::query("update email_search_attempts set at = now() - interval '2 hours'").execute(&db).await.unwrap();
    tick().await.unwrap();
    let (_, done) = api.get(&task).await;
    assert_eq!((done["status"].as_str(), done["results"][0]["email"].as_str()), (Some("completed"), Some("jane.doe@acme.example")), "{done}");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn inbox_changes_return_each_reply_once_across_pages(db: PgPool) {
    let [a, b] = seed(&db).await;
    let anna = reply(&db, a, "anna", "hot", "50 minutes").await;
    for ago in ["40 minutes", "30 minutes", "20 minutes"] {
        another_reply(&db, a, anna, "question", ago).await;
    }
    // Two at the same moment, told apart by id.
    sqlx::query("update email_messages set created_at = (select created_at from email_messages where body = 'Reply 30 minutes') where body = 'Reply 20 minutes'")
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'initial', 'sent')")
        .bind(a)
        .bind(anna)
        .execute(&db)
        .await
        .unwrap();
    let fresh = another_reply(&db, a, anna, "other", "0 seconds").await;
    reply(&db, b, "dee", "hot", "10 minutes").await;
    let expected: Vec<String> = sqlx::query_scalar(
        "select id::text from email_messages where project_id = $1 and direction = 'inbound' and id <> $2 order by created_at, id",
    )
    .bind(a)
    .bind(fresh)
    .fetch_all(&db)
    .await
    .unwrap();
    assert_eq!(expected.len(), 4);
    let api = Api::start(&db, LIMITS).await;
    let changes = format!("/v1/projects/{a}/inbox/changes");

    let (mut seen, mut cursor) = (vec![], String::new());
    loop {
        let (status, page) = api.get(&format!("{changes}?limit=3&cursor={cursor}")).await;
        assert_eq!(status, 200, "{page}");
        seen.extend(page["items"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap().to_string()));
        cursor = page["next_cursor"].as_str().unwrap().to_string();
        if page["has_more"] == false {
            break;
        }
    }
    assert_eq!(seen, expected, "each settled reply once, in order, and the one still settling not yet");

    // Once it has settled it comes next, and then the cursor stays put.
    sqlx::query("update email_messages set created_at = now() - interval '6 seconds' where id = $1").bind(fresh).execute(&db).await.unwrap();
    let (_, page) = api.get(&format!("{changes}?cursor={cursor}")).await;
    assert_eq!((page["items"].as_array().unwrap().len(), page["items"][0]["id"].clone()), (1, json!(fresh)), "{page}");
    assert_eq!(page["items"][0]["classification"], "other");
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let (_, page) = api.get(&format!("{changes}?cursor={cursor}")).await;
    assert_eq!((page["items"].as_array().unwrap().len(), page["next_cursor"].as_str()), (0, Some(cursor.as_str())));

    let third: crate::models::Ts = sqlx::query_scalar("select created_at from email_messages where id::text = $1").bind(&expected[1]).fetch_one(&db).await.unwrap();
    let (_, page) = api.get(&format!("{changes}?since={}", at(third))).await;
    let ids: Vec<&str> = page["items"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, [expected[2].as_str(), expected[3].as_str(), fresh.to_string().as_str()]);
    assert_eq!(api.get(&format!("{changes}?cursor=nope")).await.0, 400);
    assert_eq!(api.get(&format!("/v1/projects/{b}/inbox/changes")).await.0, 404);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_inbox_and_its_changes_filter_by_campaign(db: PgPool) {
    let [a, _] = seed(&db).await;
    let mut campaigns = vec![];
    for (name, ago) in [("anna", "50 minutes"), ("bo", "40 minutes")] {
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, $2) returning id")
            .bind(a)
            .bind(name)
            .fetch_one(&db)
            .await
            .unwrap();
        let lead = reply(&db, a, name, "question", ago).await;
        sqlx::query("update leads set segment_id = $2 where id = $1").bind(lead).bind(segment).execute(&db).await.unwrap();
        sqlx::query("update email_messages set segment_id = $2 where lead_id = $1").bind(lead).bind(segment).execute(&db).await.unwrap();
        campaigns.push((segment, lead));
    }
    reply(&db, a, "cy", "question", "30 minutes").await;
    let api = Api::start(&db, LIMITS).await;
    let [(anna_campaign, anna), (bo_campaign, bo)] = [campaigns[0], campaigns[1]];

    let (status, page) = api.get(&format!("/v1/inbox?segment_id={anna_campaign}")).await;
    assert_eq!(status, 200, "{page}");
    let first = (&page["total"], &page["items"][0]["lead_id"], &page["items"][0]["campaign_id"]);
    assert_eq!(first, (&json!(1), &json!(anna), &json!(anna_campaign)));
    assert_eq!(api.get("/v1/inbox?segment_id=").await.1["total"], 3);

    let (_, changes) = api.get(&format!("/v1/projects/{a}/inbox/changes?segment_id={bo_campaign}")).await;
    let leads: Vec<&Value> = changes["items"].as_array().unwrap().iter().map(|m| &m["lead_id"]).collect();
    assert_eq!((leads, &changes["items"][0]["campaign_id"]), (vec![&json!(bo)], &json!(bo_campaign)), "{changes}");
    assert_eq!(api.get(&format!("/v1/projects/{a}/inbox/changes")).await.1["items"].as_array().unwrap().len(), 3);
}

fn import_lead(email: &str, title: &str) -> Value {
    json!({ "email": email, "first_name": "Ana", "last_name": "Silva", "company_domain": "smile.example", "job_title": title })
}

/// Polls an import until it's no longer pending.
async fn finished_import(api: &Api, task: &str) -> Value {
    for _ in 0..200 {
        let (status, body) = api.get(&format!("/v1/campaigns/import/{task}")).await;
        assert_eq!(status, 200, "{body}");
        if body["status"] != "pending" {
            return body;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the import never finished");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_lead_list_becomes_a_campaign_of_exactly_the_leads_it_could_import(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    // ana@smile.example is already a lead of another campaign, waiting on a draft.
    let (dentists, _) = campaign(&db, a, "working").await;
    sqlx::query(
        "insert into leads (project_id, segment_id, full_name, email, last_contacted_at) values ($1, $2, 'Raj', 'raj@smile.example', now())",
    )
        .bind(a)
        .bind(dentists)
        .execute(&db)
        .await
        .unwrap();
    let (status, _, body) = api.post(KEYS[0], "/v1/suppressions", None, json!({ "values": ["blocked@smile.example"] })).await;
    assert_eq!(status, 200, "{body}");

    let mut first = import_lead("bea@smile.example", "Owner");
    first["client_ref"] = json!("crm-1");
    first["linkedin_url"] = json!("https://www.linkedin.com/in/bea");
    let leads = json!([
        first,
        import_lead("caio@smile.example", "Dentist"),
        import_lead("dora@smile.example", " "),
        { "email": null, "first_name": "Eli" },
        import_lead("BEA@smile.example", "Owner"),
        import_lead("blocked@smile.example", "Owner"),
        import_lead("raj@smile.example", "Owner"),
        import_lead("ana@smile.example", "Owner"),
    ]);
    let body =
        json!({ "project_id": a, "name": "Smile clinics", "leads": leads, "instructions": "Mention our Lisbon office", "language": "German" });
    let (status, _, started) = api.post(KEYS[0], "/v1/campaigns/import", None, body).await;
    assert_eq!((status, &started["status"]), (200, &json!("pending")), "{started}");
    let task = started["task_id"].as_str().unwrap();
    let campaign_id: Uuid = started["campaign_id"].as_str().unwrap().parse().unwrap();

    let done = finished_import(&api, task).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(done["result"], json!({ "campaign_id": campaign_id, "imported": 2, "skipped": 2, "deduped": 4 }));

    let (status, page) = api.get(&format!("/v1/campaigns/import/{task}/rows?per_page=3&page=1")).await;
    assert_eq!((status, page["total"].clone(), page["items"].as_array().unwrap().len()), (200, json!(8), 3), "{page}");
    let (_, page) = api.get(&format!("/v1/campaigns/import/{task}/rows")).await;
    let outcomes: Vec<(String, String)> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["status"].as_str().unwrap().to_string(), r["reason"].as_str().unwrap_or_default().to_string()))
        .collect();
    let expected = [
        ("imported", ""),
        ("imported", ""),
        ("skipped", "missing_job_title"),
        ("skipped", "missing_email"),
        ("deduped", "duplicate_in_request"),
        ("deduped", "do_not_contact"),
        ("deduped", "already_contacted"),
        ("deduped", "already_in_project"),
    ];
    assert_eq!(outcomes, expected.map(|(s, r)| (s.to_string(), r.to_string())));
    assert_eq!((page["items"][0]["row"].clone(), page["items"][0]["client_ref"].clone()), (json!(0), json!("crm-1")));

    // The campaign holds those two leads and no others, and they are the people the rows name.
    let held: Vec<(Uuid, String, String, String)> =
        sqlx::query_as("select id, email::text, title, linkedin_url from leads where segment_id = $1 order by email")
            .bind(campaign_id)
            .fetch_all(&db)
            .await
            .unwrap();
    assert_eq!(
        held.iter().map(|(_, email, title, _)| (email.as_str(), title.as_str())).collect::<Vec<_>>(),
        [("bea@smile.example", "Owner"), ("caio@smile.example", "Dentist")]
    );
    assert_eq!(held[0].3, "https://www.linkedin.com/in/bea");
    assert_eq!(page["items"][0]["person_id"], json!(held[0].0));
    assert_eq!(page["items"][1]["person_id"], json!(held[1].0));
    assert_eq!(page["items"][7]["person_id"], Value::Null);
    let ana: Uuid = sqlx::query_scalar("select segment_id from leads where email = 'ana@smile.example'").fetch_one(&db).await.unwrap();
    assert_eq!(ana, dentists, "a lead already in the project keeps its campaign");

    let (_, detail) = api.get(&format!("/v1/campaigns/{campaign_id}")).await;
    assert_eq!(
        (&detail["name"], &detail["status"], &detail["angle"], &detail["language"], &detail["leads"]),
        (&json!("Smile clinics"), &json!("draft"), &json!("Mention our Lisbon office"), &json!("de"), &json!(2)),
    );

    // After 30 days the task and its rows go, and the campaign and its leads stay.
    sqlx::query("update campaign_imports set created_at = now() - interval '31 days'").execute(&db).await.unwrap();
    crate::agent::expire_visitor_data(&db).await.unwrap();
    assert_eq!(count(&db, "select (select count(*) from campaign_imports) + (select count(*) from campaign_import_rows)").await, 0);
    assert_eq!(api.get(&format!("/v1/campaigns/import/{task}")).await.0, 404);
    assert_eq!(api.get(&format!("/v1/campaigns/{campaign_id}")).await.1["leads"], 2);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaign_imports_check_their_request_belong_to_one_organization_and_replay_by_key(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let import = |project: Uuid, leads: Value| json!({ "project_id": project, "name": "Smile clinics", "leads": leads });
    let one = json!([import_lead("bea@smile.example", "Owner")]);

    let (status, headers, first) = api.post(KEYS[0], "/v1/campaigns/import", Some("import-1"), import(a, one.clone())).await;
    assert_eq!((status, header(&headers, "idempotent-replayed")), (200, None), "{first}");
    let (status, headers, again) = api.post(KEYS[0], "/v1/campaigns/import", Some("import-1"), import(a, one.clone())).await;
    assert_eq!((status, header(&headers, "idempotent-replayed"), &again["task_id"]), (200, Some("true"), &first["task_id"]));
    assert_eq!(count(&db, "select (select count(*) from campaign_imports) + (select count(*) from segments)").await, 2);
    let task = first["task_id"].as_str().unwrap();
    assert_eq!(finished_import(&api, task).await["result"]["imported"], 1);

    // Another organization sees none of it and can't import into this project.
    for path in [format!("/v1/campaigns/import/{task}"), format!("/v1/campaigns/import/{task}/rows")] {
        assert_eq!(api.send(Method::GET, KEYS[1], &path, None, None).await.0, 404, "{path}");
    }
    assert_eq!(api.post(KEYS[1], "/v1/campaigns/import", None, import(a, one.clone())).await.0, 404);
    assert_eq!(api.post(KEYS[1], "/v1/campaigns/import", None, import(b, one.clone())).await.0, 200);

    // A refused request makes no campaign.
    let before = count(&db, "select count(*) from segments").await;
    assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, import(a, json!([]))).await.0, 400);
    let too_many = Value::Array(vec![json!({}); crate::campaign_imports::MAX_LEADS + 1]);
    assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, import(a, too_many)).await.0, 400);
    let mut unnamed = import(a, one.clone());
    unnamed["name"] = json!(" ");
    assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, unnamed).await.0, 400);
    let mut klingon = import(a, one.clone());
    klingon["language"] = json!("Klingon");
    assert_eq!(api.post(KEYS[0], "/v1/campaigns/import", None, klingon).await.0, 400);
    let mut follow_ups = import(a, one.clone());
    follow_ups["followup_instructions"] = json!("Bump it after 3 days");
    let (status, _, body) = api.post(KEYS[0], "/v1/campaigns/import", None, follow_ups).await;
    assert_eq!(status, 422, "{body}");
    assert_eq!(count(&db, "select count(*) from segments").await, before);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn my_copy_switches_mode_and_source_without_losing_text_and_stays_in_its_organization(db: PgPool) {
    let [a, b] = seed(&db).await;
    let (segment, _) = campaign(&db, a, "draft").await;
    let (other, _) = campaign(&db, b, "draft").await;
    // A lead my_copy's pinned split keeps on the user's text.
    sqlx::query("insert into leads (id, project_id, segment_id, full_name, first_name, company_name, email, created_at)
                 values ('00000000-0000-0000-0000-000000000001', $1, $2, 'Ana Ruiz', 'Ana', 'Smile', 'ana@ruiz.example', now() - interval '1 day')")
        .bind(a)
        .bind(segment)
        .execute(&db)
        .await
        .unwrap();
    let api = Api::start(&db, LIMITS).await;
    let mine = format!("/v1/campaigns/{segment}/my-copy");
    let project = format!("/v1/projects/{a}/my-copy");
    let put = |path: String, body: Value| {
        let api = api.clone();
        async move {
            let (status, _, body) = api.send(Method::PUT, KEYS[0], &path, None, Some(body)).await;
            (status, body)
        }
    };

    // A campaign starts on the project's first email, the AI one, and can't change it here while it follows the project.
    let (status, copy) = api.get(&mine).await;
    assert_eq!(status, 200, "{copy}");
    assert_eq!((copy["source"].clone(), copy["mode"].clone(), copy["body"].clone()), (json!("project"), json!("instructions"), json!("")));
    assert!(copy["results"]["note"].as_str().unwrap().starts_with("Not enough data"));
    assert_eq!(api.patch(&mine, json!({ "body": "Hi" })).await.0, 409);

    // The project's text is checked before anything is saved.
    let (status, body) = api.patch(&project, json!({ "mode": "my_copy" })).await;
    assert!(status == 422 && body["error"].as_str().unwrap().contains("Write the email's body"), "{status} {body}");
    let (status, body) = api.patch(&project, json!({ "mode": "my_copy", "body": "Hi {{frist_name}}" })).await;
    assert!(status == 422 && body["error"].as_str().unwrap().contains("{{frist_name}} isn't a variable"), "{status} {body}");
    assert_eq!(api.patch(&project, json!({ "tone": "direct" })).await.0, 422);
    assert_eq!(api.patch(&project, json!({ "mode": "sometimes" })).await.0, 422);
    assert_eq!(api.get(&project).await.1["mode"], "instructions");
    let (status, saved) = api.patch(&project, json!({ "mode": "my_copy", "subject": "{Hi|Hello} {{first_name}}", "body": "How is {{company}}?" })).await;
    assert_eq!(status, 200, "{saved}");
    assert_eq!(saved["campaign_ids"], json!([segment]));
    assert_eq!(api.get(&mine).await.1["body"], "How is {{company}}?");

    // What leads get: the text filled in for each, with no Claude call.
    let (status, preview) = api.get(&format!("{mine}/preview")).await;
    assert_eq!(status, 200, "{preview}");
    let emails = preview["emails"].as_array().unwrap();
    let ana = emails.iter().find(|e| e["lead_id"] == "00000000-0000-0000-0000-000000000001").unwrap();
    assert!(ana["subject"] == "Hi Ana" || ana["subject"] == "Hello Ana", "{ana}");
    assert_eq!((ana["body"].clone(), ana["gets"].clone()), (json!("How is Smile?"), json!("my_copy")));

    // Custom starts as a copy of the project's, and switching either way loses neither text.
    let (status, custom) = put(format!("{mine}/source"), json!({ "source": "campaign" })).await;
    assert_eq!((status, custom["source"].clone(), custom["body"].clone()), (200, json!("campaign"), json!("How is {{company}}?")));
    assert_eq!(api.patch(&mine, json!({ "body": "Does your {{job_title|team}} plan visits?" })).await.0, 200);
    let (_, custom) = api.patch(&mine, json!({ "mode": "instructions" })).await;
    assert_eq!((custom["mode"].clone(), custom["body"].clone()), (json!("instructions"), json!("Does your {{job_title|team}} plan visits?")));
    let (_, back) = put(format!("{mine}/source"), json!({ "source": "project" })).await;
    assert_eq!((back["mode"].clone(), back["body"].clone()), (json!("my_copy"), json!("How is {{company}}?")));
    let (_, again) = put(format!("{mine}/source"), json!({ "source": "campaign" })).await;
    assert_eq!((again["mode"].clone(), again["body"].clone()), (json!("instructions"), json!("Does your {{job_title|team}} plan visits?")));
    assert_eq!(put(format!("{mine}/source"), json!({ "source": "elsewhere" })).await.0, 422);
    let (_, kept) = api.get(&project).await;
    assert_eq!((kept["body"].clone(), kept["campaign_ids"].clone()), (json!("How is {{company}}?"), json!([])));

    // Another organization's campaign and project are 404 on every route.
    let theirs = format!("/v1/campaigns/{other}/my-copy");
    assert_eq!(api.get(&theirs).await.0, 404);
    assert_eq!(api.patch(&theirs, json!({ "mode": "instructions" })).await.0, 404);
    assert_eq!(put(format!("{theirs}/source"), json!({ "source": "campaign" })).await.0, 404);
    assert_eq!(api.get(&format!("{theirs}/preview")).await.0, 404);
    assert_eq!(api.get(&format!("/v1/projects/{b}/my-copy")).await.0, 404);
    assert_eq!(api.patch(&format!("/v1/projects/{b}/my-copy"), json!({ "body": "Hi" })).await.0, 404);
    let theirs: String = sqlx::query_scalar("select my_copy_source from segments where id = $1").bind(other).fetch_one(&db).await.unwrap();
    assert_eq!(theirs, "project");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn erased_leads_come_once_each_oldest_first_for_the_keys_organization(db: PgPool) {
    async fn erased(db: &PgPool, project: Uuid, ago: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into erased_leads (lead_id, org_id, project_id, forgotten, erased_at)
             select gen_random_uuid(), org_id, id, false, now() - $2::interval from projects where id = $1 returning lead_id",
        )
        .bind(project)
        .bind(ago)
        .fetch_one(db)
        .await
        .unwrap()
    }
    fn ids(page: &Value) -> Vec<Uuid> {
        page["items"].as_array().unwrap().iter().map(|e| e["lead_id"].as_str().unwrap().parse().unwrap()).collect()
    }
    let [a, b] = seed(&db).await;
    let other: Uuid = sqlx::query_scalar("insert into projects (org_id, name) select org_id, 'Other' from projects where id = $1 returning id")
        .bind(a)
        .fetch_one(&db)
        .await
        .unwrap();
    let (first, second, third) = (erased(&db, a, "3 minutes").await, erased(&db, other, "2 minutes").await, erased(&db, a, "1 minute").await);
    erased(&db, a, "-30 seconds").await;
    erased(&db, b, "1 minute").await;
    let api = Api::start(&db, LIMITS).await;

    // The organization's projects, a page at a time. One erased this moment waits a few seconds, and B's never show.
    let (status, page) = api.get("/v1/erased-leads?limit=2").await;
    assert_eq!((status, ids(&page), page["has_more"].clone()), (200, vec![first, second], json!(true)), "{page}");
    let (_, page) = api.get(&format!("/v1/erased-leads?limit=2&cursor={}", page["next_cursor"].as_str().unwrap())).await;
    assert_eq!((ids(&page), page["has_more"].clone()), (vec![third], json!(false)));
    let cursor = page["next_cursor"].as_str().unwrap().to_string();
    let (_, page) = api.get(&format!("/v1/erased-leads?cursor={cursor}")).await;
    assert_eq!((ids(&page), page["next_cursor"].as_str()), (vec![], Some(cursor.as_str())), "an empty page keeps the cursor");

    let (_, page) = api.get(&format!("/v1/erased-leads?project_id={a}")).await;
    assert_eq!(ids(&page), [first, third]);
    let since = page["items"][0]["erased_at"].as_str().unwrap().to_string();
    let (_, page) = api.get(&format!("/v1/erased-leads?since={}", since.replace('+', "%2B"))).await;
    assert_eq!(ids(&page), [second, third], "since leaves out one erased at exactly that time");
    assert_eq!(api.get(&format!("/v1/erased-leads?project_id={b}")).await.0, 404);
    assert_eq!(api.get("/v1/erased-leads?cursor=nope").await.0, 400);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_reply_marked_answered_leaves_needs_reply_until_the_lead_writes_again(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let lead: Uuid = sqlx::query_scalar(
        "insert into leads (project_id, full_name, email, status, remind_at)
         values ($1, 'Ana', 'ana@smile.example', 'replied', now() + interval '1 day') returning id",
    )
    .bind(a)
    .fetch_one(&db)
    .await
    .unwrap();
    let path = format!("/v1/leads/{lead}/mark-replied");
    let (status, _, body) = api.post(KEYS[0], &path, None, json!({})).await;
    assert_eq!(status, 409, "{body}");

    let write = |at: &'static str| {
        sqlx::query(
            "insert into email_messages (project_id, lead_id, direction, kind, status, created_at)
             values ($1, $2, 'inbound', 'inbound', 'received', $3::timestamptz)",
        )
        .bind(a)
        .bind(lead)
        .bind(at)
        .execute(&db)
    };
    write("2026-11-20T09:00:00Z").await.unwrap();
    write("2026-11-20T10:00:00Z").await.unwrap();
    sqlx::query("insert into email_messages (project_id, lead_id, direction, kind, status) values ($1, $2, 'outbound', 'auto_reply', 'draft')")
        .bind(a)
        .bind(lead)
        .execute(&db)
        .await
        .unwrap();
    let needs_reply = || async { api.get("/v1/inbox?filter=needs_reply").await.1["total"].clone() };
    assert_eq!(needs_reply().await, json!(1));

    assert_eq!(api.post(KEYS[1], &path, None, json!({})).await.0, 404, "another organization's lead");
    let (status, _, body) = api.post(KEYS[0], &path, None, json!({})).await;
    assert_eq!((status, body), (200, json!({ "lead_id": lead, "answered_through": "2026-11-20T10:00:00Z" })));
    assert_eq!(needs_reply().await, json!(0));
    let left: (String, bool) = sqlx::query_as(
        "select l.status, l.remind_at is null and not exists (select 1 from email_messages e where e.lead_id = l.id and e.status = 'draft')
         from leads l where l.id = $1",
    )
    .bind(lead)
    .fetch_one(&db)
    .await
    .unwrap();
    assert_eq!(left, ("replied".to_string(), true), "the status stays, and the reminder and the agent's draft are gone");

    write("2026-11-20T11:00:00Z").await.unwrap();
    assert_eq!(needs_reply().await, json!(1), "a new reply brings the thread back");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn tasks_of_every_type_are_listed_newest_first_for_the_keys_organization(db: PgPool) {
    let [a, b] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
    let second: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Second') returning id")
        .bind(org)
        .fetch_one(&db)
        .await
        .unwrap();
    /// Runs the insert with {ago} as that many minutes before now, so the rows have an order.
    async fn task(db: &PgPool, sql: &str, project: Uuid, minutes_ago: i32) -> Uuid {
        let sql = sql.replace("{ago}", "now() - make_interval(mins => $2)");
        sqlx::query_scalar(&sql).bind(project).bind(minutes_ago).fetch_one(db).await.unwrap()
    }
    let import = task(
        &db,
        "insert into campaign_imports (org_id, project_id, status, created_at) select org_id, id, 'failed', {ago} from projects where id = $1
         returning id",
        second,
        5,
    )
    .await;
    let batch_sql = "insert into email_lookup_batches (org_id, project_id, created_at) select org_id, id, {ago} from projects where id = $1
                     returning id";
    let batch = task(&db, batch_sql, a, 4).await;
    // A run counts for the organization through its project, or through org_id when it has none.
    let found_sql = "insert into agent_runs (project_id, kind, status, created_at, finished_at)
                     values ($1, 'find_people', 'succeeded', {ago}, now()) returning id";
    let found = task(&db, found_sql, a, 3).await;
    let cancelled_sql = "insert into agent_runs (org_id, kind, status, created_at)
                         select org_id, 'classify_reply', 'cancelled', {ago} from projects where id = $1 returning id";
    let cancelled = task(&db, cancelled_sql, a, 2).await;
    let writing_sql = "insert into agent_runs (project_id, kind, status, created_at) values ($1, 'write_emails', 'running', {ago}) returning id";
    let writing = task(&db, writing_sql, second, 1).await;
    let theirs = task(&db, "insert into agent_runs (project_id, kind, created_at) values ($1, 'find_people', {ago}) returning id", b, 0).await;

    let ids = |page: &Value| -> Vec<Uuid> {
        page["items"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().parse().unwrap()).collect()
    };
    let (status, all) = api.get("/v1/tasks").await;
    assert_eq!((status, &all["total"]), (200, &json!(5)), "{all}");
    assert_eq!(ids(&all), [writing, cancelled, found, batch, import]);
    let summary: Vec<(&str, Option<&str>, &str, String)> = all["items"].as_array().unwrap().iter().map(|t| {
        (t["type"].as_str().unwrap(), t["kind"].as_str(), t["status"].as_str().unwrap(), t["url"].as_str().unwrap().to_string())
    }).collect();
    assert_eq!(summary, [
        ("run", Some("write_emails"), "pending", format!("/v1/runs/{writing}")),
        ("run", Some("classify_reply"), "failed", format!("/v1/runs/{cancelled}")),
        ("run", Some("find_people"), "completed", format!("/v1/runs/{found}")),
        ("email_batch", None, "pending", format!("/v1/enrich/email/batch/{batch}")),
        ("campaign_import", None, "failed", format!("/v1/campaigns/import/{import}")),
    ]);
    // Each url is the route that follows the task.
    for task in all["items"].as_array().unwrap() {
        let (status, body) = api.get(task["url"].as_str().unwrap()).await;
        assert_eq!(status, 200, "{task} {body}");
    }

    assert_eq!(ids(&api.get("/v1/tasks?status=pending").await.1), [writing, batch]);
    assert_eq!(ids(&api.get("/v1/tasks?type=run&kind=find_people").await.1), [found]);
    assert_eq!(ids(&api.get(&format!("/v1/tasks?project_id={second}")).await.1), [writing, import]);
    let (_, page) = api.get("/v1/tasks?per_page=2&page=2").await;
    assert_eq!((ids(&page), &page["total"]), (vec![found, batch], &json!(5)));
    for bad in ["status=done", "type=job"] {
        assert_eq!(api.get(&format!("/v1/tasks?{bad}")).await.0, 400, "{bad}");
    }
    assert_eq!(ids(&api.send(Method::GET, KEYS[1], "/v1/tasks", None, None).await.2), [theirs]);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_campaigns_send_state_names_what_holds_it_for_the_keys_organization(db: PgPool) {
    let [a, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let (segment, email) = campaign(&db, a, "working").await;
    // Sending, every hour of every day, so the test doesn't depend on when it runs. New projects start paused.
    sqlx::query(
        "insert into project_settings (project_id, paused, send_days, send_start_hour, send_end_hour) values ($1, false, '{1,2,3,4,5,6,7}', 0, 24)
         on conflict (project_id) do update set paused = false, send_days = excluded.send_days, send_start_hour = 0, send_end_hour = 24",
    )
    .bind(a)
    .execute(&db)
    .await
    .unwrap();
    let path = format!("/v1/campaigns/{segment}/send-state");
    let state = || async { api.get(&path).await.1 };

    assert_eq!(state().await["state"], "no_mailbox");
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(a).fetch_one(&db).await.unwrap();
    sqlx::query("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'sam@acme.test', 'x', '', '')")
        .bind(org)
        .execute(&db)
        .await
        .unwrap();
    let waiting = state().await;
    assert_eq!(
        (&waiting["state"], &waiting["message"], &waiting["sending_now"], &waiting["today"]["drafts"]),
        (&json!("awaiting_approval"), &json!("1 email waits for approval"), &json!(false), &json!(1)),
        "{waiting}"
    );
    assert_eq!(waiting["next_action"], format!("POST /v1/campaigns/{segment}/approve"));
    assert_eq!(waiting["allowed_actions"], json!(["stop", "approve", "archive"]));

    sqlx::query("update email_messages set status = 'approved' where id = $1").bind(email).execute(&db).await.unwrap();
    let sending = state().await;
    let summary = (&sending["state"], &sending["sending_now"], &sending["today"]["queued"]);
    assert_eq!(summary, (&json!("sending"), &json!(true), &json!(1)), "{sending}");
    assert_eq!(sending["next_action"], Value::Null);

    // A follow-up waits for its thread's mailbox, here one still warming up, even with another mailbox free.
    let warming: Uuid = sqlx::query_scalar(
        "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc, warmup, total_daily_limit)
         values ($1, 'ana@acme.test', 'x', '', '', true, 0) returning id",
    )
    .bind(org)
    .fetch_one(&db)
    .await
    .unwrap();
    sqlx::query("update email_messages set kind = 'follow_up', mailbox_id = $2 where id = $1")
        .bind(email)
        .bind(warming)
        .execute(&db)
        .await
        .unwrap();
    assert_eq!(state().await["state"], "mailboxes_full");
    // A first email due too can go out from the free mailbox, until autopilot's lead cap holds it.
    sqlx::query(
        "with l as (insert into leads (project_id, segment_id, full_name, email) values ($1, $2, 'Bo', 'bo@smile.example') returning id)
         insert into email_messages (project_id, lead_id, segment_id, direction, kind, subject, body, status)
         select $1, l.id, $2, 'outbound', 'initial', 'Hi', 'Hello', 'approved' from l",
    )
    .bind(a)
    .bind(segment)
    .execute(&db)
    .await
    .unwrap();
    assert_eq!(state().await["state"], "sending");
    sqlx::query("update project_settings set autopilot = true, autopilot_lead_cap = 0 where project_id = $1").bind(a).execute(&db).await.unwrap();
    assert_eq!(state().await["state"], "mailboxes_full", "the follow-up still waits for its mailbox, and the first email for the 1st");

    sqlx::query("update project_settings set paused = true where project_id = $1").bind(a).execute(&db).await.unwrap();
    let paused = state().await;
    let resume = format!("PATCH /v1/projects/{a} with {{\"sending\": true}}");
    assert_eq!((&paused["state"], &paused["next_action"]), (&json!("project_paused"), &json!(resume)));

    assert_eq!(api.send(Method::GET, KEYS[1], &path, None, None).await.0, 404);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn people_are_found_at_companies_picked_by_domain_in_the_titles_asked_for(db: PgPool) {
    let [project, _] = seed(&db).await;
    let api = Api::start(&db, LIMITS).await;
    let (segment, _) = campaign(&db, project, "working").await;
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
    // Two the database lists with an email on a do-not-contact list, one of the organization's and one of a list for the project.
    sqlx::query(
        "insert into directory_companies (domain, name, website, email, city, country_code, source_id, release)
         values ('smile.example', 'Smile Dental', 'https://smile.example', null, 'Leeds', 'GB', 'x', 'r'),
                ('mailed.example', 'Mailed', 'https://mailed.example', 'hi@mailed.example', 'Leeds', 'GB', 'x', 'r'),
                ('listed.example', 'Listed', 'https://listed.example', 'hi@listed.example', 'Leeds', 'GB', 'x', 'r')",
    )
    .execute(&db)
    .await
    .unwrap();
    sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'hi@mailed.example', 'email', 'manual')").bind(org).execute(&db).await.unwrap();
    sqlx::query(
        "with l as (insert into suppression_lists (org_id, name, kind, project_ids) values ($1, 'Customers', 'people', array[$2]) returning id)
         insert into suppression_list_entries (list_id, org_id, value) select id, $1, 'hi@listed.example' from l",
    )
    .bind(org)
    .bind(project)
    .execute(&db)
    .await
    .unwrap();
    // One the project has, one it excluded, and one on the do-not-contact list.
    sqlx::query("insert into companies (project_id, domain, name, status) values ($1, 'known.example', 'Known', 'people_found'), ($1, 'gone.example', 'Gone', 'excluded')")
        .bind(project)
        .execute(&db)
        .await
        .unwrap();
    sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.example', 'domain', 'manual')").bind(org).execute(&db).await.unwrap();

    let path = format!("/v1/segments/{segment}/find-people");
    let domains =
        ["https://Smile.example/team", "bare.example", "known.example", "gone.example", "blocked.example", "smile.example", "mailed.example", "listed.example"];
    let body = json!({ "domains": domains, "titles": [" Practice Manager ", "Owner", ""], "per_company": 1 });
    let (status, _, body) = api.post(KEYS[0], &path, None, body).await;
    assert_eq!(status, 200, "{body}");
    let run: Uuid = body["run_id"].as_str().unwrap().parse().unwrap();
    let input: Value = sqlx::query_scalar("select input from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
    assert_eq!((input["roles"].clone(), input["max_per_company"].clone()), (json!("Practice Manager, Owner"), json!(1)));
    let ids: Vec<Uuid> = serde_json::from_value(input["company_ids"].clone()).unwrap();
    let searched: Vec<(String, String, bool)> =
        sqlx::query_as("select domain::text, name, segment_id is not distinct from $2 from companies where id = any($1) order by domain")
            .bind(&ids)
            .bind(segment)
            .fetch_all(&db)
            .await
            .unwrap();
    let searched: Vec<_> = searched.iter().map(|(d, n, here)| (d.as_str(), n.as_str(), *here)).collect();
    assert_eq!(searched, [("bare.example", "bare.example", true), ("known.example", "Known", false), ("smile.example", "Smile Dental", true)]);
    assert_eq!(count(&db, "select count(*) from companies where domain in ('mailed.example', 'listed.example')").await, 0, "not even bare");

    crate::agent::fake::answer_with(run, |_| json!({ "people": [] }));
    crate::agent::start_now(&crate::autopilot::tests::state(db.clone()), run).await.unwrap();
    let mut status = String::new();
    for _ in 0..250 {
        status = sqlx::query_scalar("select status from agent_runs where id = $1").bind(run).fetch_one(&db).await.unwrap();
        if status != "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(status, "succeeded");
    let prompt = crate::agent::fake::prompts(run).concat();
    assert!(prompt.contains("Only people in these roles: Practice Manager, Owner.") && prompt.contains("bare.example"), "{prompt}");

    for (body, why) in [
        (json!({ "domains": ["blocked.example", "gone.example"] }), "every one left out"),
        (json!({ "domains": ["not a domain"] }), "not a domain"),
        (json!({ "domains": [] }), "no domains"),
        (json!({ "domains": ["bare.example"], "company_ids": [] }), "both"),
        (json!({ "per_company": 6 }), "too many a company"),
        (json!({ "titles": vec!["CEO"; 21] }), "too many titles"),
    ] {
        assert_eq!(api.post(KEYS[0], &path, None, body).await.0, 400, "{why}");
    }
    assert_eq!(api.post(KEYS[1], &path, None, json!({ "domains": ["bare.example"] })).await.0, 404, "another organization's campaign");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_company_another_request_adds_at_the_same_moment_is_searched_too(db: PgPool) {
    let [project, _] = seed(&db).await;
    let (segment, _) = campaign(&db, project, "working").await;
    let org: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(project).fetch_one(&db).await.unwrap();
    // The other request's insert, not committed yet, which this one's waits on.
    let mut other = db.begin().await.unwrap();
    sqlx::query("insert into companies (project_id, segment_id, domain, name) values ($1, $2, 'race.example', 'race.example')")
        .bind(project)
        .bind(segment)
        .execute(&mut *other)
        .await
        .unwrap();
    let ask = {
        let db = db.clone();
        tokio::spawn(async move { crate::directory::companies_at(&db, org, project, segment, &["race.example".to_string()]).await })
    };
    let mut waiting = 0;
    for _ in 0..250 {
        waiting = count(&db, "select count(*) from pg_stat_activity where datname = current_database() and wait_event_type = 'Lock'").await;
        if waiting == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(waiting, 1, "the request waits on the other's insert, so the test races them");
    other.commit().await.unwrap();
    assert_eq!(ask.await.unwrap().unwrap().len(), 1, "the company the other request added");
}
