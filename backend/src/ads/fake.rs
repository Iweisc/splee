//! LinkedIn's Marketing API, the Google Ads API and the Graph API's Custom Audiences, with their sign-ins, as their docs describe
//! them, on one local port under /linkedin, /google and /graph. The token endpoints hand out access-{n} for a code or a refresh
//! token, and turn the refresh token "revoked" down. The APIs take only tokens in World::live, and Meta's code exchange gives
//! meta-long. Each audience keeps the hashes it holds, so tests see what a platform would.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use reqwest::Url;
use serde_json::{Value, json};
use tokio::sync::Notify;

use crate::config::{Config, OAuthApp};
use crate::mail::oauth::tests::{FakeProvider, config};

/// One request the fake got. `body` is the JSON or the form, as an object, and for Meta's GETs the query.
#[derive(Clone, Debug)]
pub struct Seen {
    pub method: String,
    /// Without /linkedin, /google/v26 or /graph.
    pub path: String,
    pub platform: &'static str,
    pub body: Value,
    pub token: String,
}

#[derive(Debug, Default)]
pub struct FakeAudience {
    pub platform: &'static str,
    pub account: String,
    pub name: String,
    pub emails: BTreeSet<String>,
    pub phones: BTreeSet<String>,
}

#[derive(Default)]
pub struct World {
    pub requests: Vec<Seen>,
    pub live: HashSet<String>,
    /// By the id the platform gave each: digits, or a Google user list's resource name.
    pub audiences: BTreeMap<String, FakeAudience>,
    /// The next this many API calls are told to slow down, with Retry-After: 1.
    pub busy: usize,
    /// What Google's jobs do once run: "SUCCESS" (the default, applied straight away), "RUNNING" until finish_jobs, or "FAILED".
    pub job_outcome: &'static str,
    /// Meta turns down the request for leads without a number, after taking the one for leads with one.
    pub refuse_email_only: bool,
    jobs: HashMap<String, FakeJob>,
    n: usize,
}

/// A Google job: its list, whether it adds, the operations given so far, and its status once run.
struct FakeJob {
    list: String,
    add: Option<bool>,
    ops: Vec<Value>,
    status: &'static str,
}

/// Holds the next request to a /users endpoint until the test lets it go, so a test can act while a sync waits on it.
#[derive(Default)]
pub struct Stall {
    pub armed: AtomicBool,
    pub reached: Notify,
    pub release: Notify,
}

#[derive(Clone)]
pub struct Fake {
    pub base: String,
    pub world: Arc<Mutex<World>>,
    pub stall: Arc<Stall>,
}

/// The only account Google lists that isn't a manager, and Meta's only account. LinkedIn lists two, so one is picked by hand.
pub const GOOGLE_ACCOUNT: &str = "1112223333";
pub const META_ACCOUNT: &str = "777";

impl Fake {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Fake { base: format!("http://{}", listener.local_addr().unwrap()), world: Arc::default(), stall: Arc::default() };
        let app = Router::new().fallback(answer).with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        fake
    }

    pub fn cfg(&self) -> Config {
        let app = |authorize: String, token: String| OAuthApp { client_id: "client-123".into(), client_secret: "shh".into(), authorize_url: authorize, token_url: token };
        let b = &self.base;
        let unused = FakeProvider { url: b.clone(), hits: Arc::default(), forms: Arc::default() };
        Config {
            linkedin_ads: Some(app(format!("{b}/linkedin/authorize"), format!("{b}/linkedin/token"))),
            google: Some(app(format!("{b}/google/authorize"), format!("{b}/google/token"))),
            google_ads_developer_token: Some("dev-token".into()),
            meta: Some(app("https://www.facebook.com/v21.0/dialog/oauth".into(), format!("{b}/graph/oauth/access_token"))),
            graph_api_url: format!("{b}/graph"),
            ads_api_url: b.clone(),
            ..config(&unused)
        }
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.world.lock().unwrap().requests.clone()
    }

    /// The requests that reached an API rather than a token endpoint.
    pub fn api_requests(&self) -> Vec<Seen> {
        self.requests().into_iter().filter(|r| !r.path.ends_with("/token") && !r.path.starts_with("/oauth/")).collect()
    }

    pub fn token_requests(&self) -> Vec<Seen> {
        self.requests().into_iter().filter(|r| r.path == "/token").collect()
    }

    /// The email hashes in each of the platform's audiences.
    pub fn emails(&self, platform: &str) -> Vec<BTreeSet<String>> {
        self.world.lock().unwrap().audiences.values().filter(|a| a.platform == platform).map(|a| a.emails.clone()).collect()
    }

    /// Ends Google's running jobs, applying them when `status` is SUCCESS.
    pub fn finish_jobs(&self, status: &'static str) {
        let mut world = self.world.lock().unwrap();
        let World { jobs, audiences, .. } = &mut *world;
        for job in jobs.values_mut().filter(|j| j.status == "RUNNING") {
            job.status = status;
            if status == "SUCCESS" {
                apply(audiences.get_mut(&job.list).unwrap(), job);
            }
        }
    }

    /// The Google jobs made so far.
    pub fn job_count(&self) -> usize {
        self.world.lock().unwrap().jobs.len()
    }

    pub fn audience(&self, platform: &str) -> Option<(String, String, BTreeSet<String>, BTreeSet<String>)> {
        let world = self.world.lock().unwrap();
        let (_, a) = world.audiences.iter().find(|(_, a)| a.platform == platform)?;
        Some((a.account.clone(), a.name.clone(), a.emails.clone(), a.phones.clone()))
    }
}

fn form(raw: &[u8]) -> Value {
    let url = Url::parse(&format!("http://x/?{}", String::from_utf8_lossy(raw))).unwrap();
    url.query_pairs().map(|(k, v)| (k.into_owned(), Value::String(v.into_owned()))).collect()
}

fn hash(v: &Value) -> String {
    let h = v.as_str().unwrap_or_default();
    assert!(h.len() == 64 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "not a SHA-256 hex hash: {h:?}");
    h.to_string()
}

fn json_answer(status: StatusCode, body: Value) -> Response {
    (status, axum::Json(body)).into_response()
}

async fn answer(State(fake): State<Fake>, method: Method, uri: Uri, headers: HeaderMap, raw: Bytes) -> Response {
    let full = uri.path().to_string();
    let query: Value = form(uri.query().unwrap_or_default().as_bytes());
    let is_form = headers.get("content-type").is_some_and(|c| c.to_str().unwrap_or_default().starts_with("application/x-www-form-urlencoded"));
    let mut body = if is_form { form(&raw) } else { serde_json::from_slice(&raw).unwrap_or_default() };
    let (platform, path) = match () {
        _ if full.starts_with("/linkedin") => ("linkedin", full.trim_start_matches("/linkedin").to_string()),
        _ if full.starts_with("/google/v26") => ("google", full.trim_start_matches("/google/v26").to_string()),
        _ if full.starts_with("/google") => ("google", full.trim_start_matches("/google").to_string()),
        _ => ("meta", full.trim_start_matches("/graph").to_string()),
    };
    if platform == "meta" && body.is_null() {
        body = query.clone();
    }
    let bearer = headers.get("authorization").and_then(|a| a.to_str().ok()).and_then(|a| a.strip_prefix("Bearer ")).unwrap_or_default();
    if uri.path().ends_with("/users") && fake.stall.armed.swap(false, Ordering::SeqCst) {
        fake.stall.reached.notify_one();
        fake.stall.release.notified().await;
    }
    let token = if bearer.is_empty() { body["access_token"].as_str().or(query["access_token"].as_str()).unwrap_or_default().to_string() } else { bearer.to_string() };
    let mut world = fake.world.lock().unwrap();
    world.requests.push(Seen { method: method.to_string(), path: path.clone(), platform, body: body.clone(), token: token.clone() });
    world.n += 1;
    let n = world.n;
    let ok = |v: Value| json_answer(StatusCode::OK, v);

    if path == "/token" {
        let access = format!("access-{n}");
        return match (body["grant_type"].as_str(), body["refresh_token"].as_str()) {
            (Some("refresh_token"), Some("revoked")) => json_answer(StatusCode::BAD_REQUEST, json!({"error": "invalid_grant", "error_description": "The token was revoked"})),
            (Some("refresh_token"), _) => {
                world.live.insert(access.clone());
                ok(json!({"access_token": access, "expires_in": 3600}))
            }
            _ => {
                world.live.insert(access.clone());
                ok(json!({"access_token": access, "expires_in": 3600, "refresh_token": format!("refresh-for-{}", body["code"].as_str().unwrap_or_default())}))
            }
        };
    }
    if path == "/oauth/access_token" {
        return match (body["code"].as_str(), body["fb_exchange_token"].as_str()) {
            (Some("the-code"), _) => ok(json!({"access_token": "SHORT", "token_type": "bearer"})),
            (_, Some("SHORT")) => {
                world.live.insert("meta-long".into());
                ok(json!({"access_token": "meta-long", "token_type": "bearer"}))
            }
            _ => json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": "Invalid verification code format.", "code": 100}})),
        };
    }
    if !world.live.contains(&token) {
        return match platform {
            "meta" => json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": "Error validating access token", "type": "OAuthException", "code": 190}})),
            _ => json_answer(StatusCode::UNAUTHORIZED, json!({"message": "Invalid access token", "status": 401})),
        };
    }
    if world.busy > 0 {
        world.busy -= 1;
        let mut res = match platform {
            "meta" => json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": "There have been too many calls", "code": 80003}})),
            _ => json_answer(StatusCode::TOO_MANY_REQUESTS, json!({"message": "Too many requests", "status": 429})),
        };
        res.headers_mut().insert("retry-after", "1".parse().unwrap());
        return res;
    }
    match platform {
        "linkedin" => linkedin(&mut world, &method, &path, &headers, &body, n),
        "google" => google(&mut world, &method, &path, &headers, &body, n),
        _ => meta(&mut world, &method, &path, &body, n),
    }
}

fn linkedin(world: &mut World, method: &Method, path: &str, headers: &HeaderMap, body: &Value, n: usize) -> Response {
    assert_eq!(headers["linkedin-version"], "202610");
    assert_eq!(headers["x-restli-protocol-version"], "2.0.0");
    let parts: Vec<&str> = path.trim_start_matches("/rest/").split('/').collect();
    let gone = || json_answer(StatusCode::NOT_FOUND, json!({"message": "Entity is not found", "status": 404}));
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["adAccounts"]) => json_answer(StatusCode::OK, json!({"elements": [
            {"id": 5001, "name": "Acme Ads", "status": "ACTIVE"},
            {"id": 5002, "name": "Acme EU", "status": "ACTIVE"},
            {"id": 5003, "name": "Closed", "status": "CANCELED"},
        ]})),
        ("POST", ["dmpSegments"]) => {
            assert_eq!((body["type"].as_str(), body["sourcePlatform"].as_str()), (Some("USER"), Some("PARTNER_API")));
            let account = body["account"].as_str().unwrap().trim_start_matches("urn:li:sponsoredAccount:").to_string();
            let id = format!("70{n}");
            world.audiences.insert(id.clone(), FakeAudience { platform: "linkedin", account, name: body["name"].as_str().unwrap().into(), ..Default::default() });
            let mut res = json_answer(StatusCode::CREATED, Value::Null);
            res.headers_mut().insert("x-restli-id", id.parse().unwrap());
            res
        }
        ("POST", ["dmpSegments", id, "users"]) => {
            assert_eq!(headers["x-restli-method"], "BATCH_CREATE");
            let Some(audience) = world.audiences.get_mut(*id) else { return gone() };
            let elements = body["elements"].as_array().unwrap();
            assert!(elements.len() <= 5000, "LinkedIn takes 5,000 at a time, got {}", elements.len());
            for e in elements {
                let ids = e["userIds"].as_array().unwrap();
                assert_eq!(ids[0]["idType"], "SHA256_EMAIL");
                let h = hash(&ids[0]["idValue"]);
                match e["action"].as_str() {
                    Some("ADD") => audience.emails.insert(h),
                    Some("REMOVE") => audience.emails.remove(&h),
                    other => panic!("unknown action {other:?}"),
                };
            }
            json_answer(StatusCode::OK, json!({"elements": elements.iter().map(|_| json!({"status": 201})).collect::<Vec<_>>()}))
        }
        ("GET", ["dmpSegments", id, "destinations", "LINKEDIN"]) => match world.audiences.get(*id) {
            Some(a) => json_answer(StatusCode::OK, json!({"status": "READY", "audienceSize": a.emails.len() * 3, "matchedCount": a.emails.len()})),
            None => gone(),
        },
        ("DELETE", ["dmpSegments", id]) => match world.audiences.remove(*id) {
            Some(_) => json_answer(StatusCode::NO_CONTENT, Value::Null),
            None => gone(),
        },
        _ => json_answer(StatusCode::NOT_FOUND, json!({"message": format!("not on the fake: {method} {path}")})),
    }
}

fn google(world: &mut World, method: &Method, path: &str, headers: &HeaderMap, body: &Value, n: usize) -> Response {
    assert_eq!(headers["developer-token"], "dev-token");
    let ok = |v: Value| json_answer(StatusCode::OK, v);
    let bad = |m: &str| json_answer(StatusCode::BAD_REQUEST, json!({"error": {"code": 400, "message": m, "status": "INVALID_ARGUMENT"}}));
    if method == Method::GET && path == "/customers:listAccessibleCustomers" {
        return ok(json!({"resourceNames": [format!("customers/{GOOGLE_ACCOUNT}"), "customers/9998887777"]}));
    }
    let Some((resource, action)) = path.trim_start_matches('/').rsplit_once(':') else { return bad("no method") };
    let customer = resource.split('/').nth(1).unwrap_or_default().to_string();
    match action {
        "search" => {
            let q = body["query"].as_str().unwrap();
            if q.contains("FROM customer") {
                let manager = customer != GOOGLE_ACCOUNT;
                return ok(json!({"results": [{"customer": {"id": customer, "descriptiveName": if manager { "Acme MCC" } else { "Acme Search" }, "manager": manager}}]}));
            }
            let quoted = q.split('\'').nth(1).unwrap();
            if q.contains("FROM offline_user_data_job") {
                return match world.jobs.get(quoted) {
                    Some(j) => ok(json!({"results": [{"offlineUserDataJob": {"resourceName": quoted, "status": j.status, "failureReason": "INVALID_USER_LIST"}}]})),
                    None => ok(json!({})),
                };
            }
            match world.audiences.get(quoted) {
                Some(a) => ok(json!({"results": [{"userList": {"sizeForDisplay": a.emails.len().to_string(), "sizeForSearch": "0"}}]})),
                None => ok(json!({})),
            }
        }
        "mutate" => {
            let op = &body["operations"][0];
            if let Some(name) = op["remove"].as_str() {
                world.audiences.remove(name);
                return ok(json!({"results": [{"resourceName": name}]}));
            }
            let list = &op["create"];
            assert_eq!(list["crmBasedUserList"]["uploadKeyType"], "CONTACT_INFO");
            let name = format!("customers/{customer}/userLists/{n}");
            world.audiences.insert(name.clone(), FakeAudience { platform: "google", account: customer, name: list["name"].as_str().unwrap().into(), ..Default::default() });
            ok(json!({"results": [{"resourceName": name}]}))
        }
        "create" => {
            let list = body["job"]["customerMatchUserListMetadata"]["userList"].as_str().unwrap().to_string();
            if !world.audiences.contains_key(&list) {
                return bad("The user list doesn't exist");
            }
            let job = format!("customers/{customer}/offlineUserDataJobs/{n}");
            world.jobs.insert(job.clone(), FakeJob { list, add: None, ops: vec![], status: "PENDING" });
            ok(json!({"resourceName": job}))
        }
        "addOperations" => {
            let ops = body["operations"].as_array().unwrap().clone();
            assert!(ops.len() <= 10_000, "Splee sends Google 10,000 at a time, got {}", ops.len());
            let Some(job) = world.jobs.get_mut(resource) else { return bad("no such job") };
            assert_eq!(job.status, "PENDING", "operations only go into a job that hasn't run");
            for op in &ops {
                let add = op.get("create").is_some();
                assert!(job.add.is_none_or(|a| a == add), "a job only adds or only removes");
                job.add = Some(add);
            }
            job.ops.extend(ops);
            ok(json!({}))
        }
        "run" => {
            let outcome = if world.job_outcome.is_empty() { "SUCCESS" } else { world.job_outcome };
            let World { jobs, audiences, .. } = world;
            let Some(job) = jobs.get_mut(resource) else { return bad("no such job") };
            let list = job.list.clone();
            let running = jobs.values().filter(|j| j.list == list && j.status == "RUNNING").count();
            assert_eq!(running, 0, "Google turns down a second job on a list while one runs");
            let job = jobs.get_mut(resource).unwrap();
            job.status = outcome;
            if outcome == "SUCCESS" {
                apply(audiences.get_mut(&list).unwrap(), job);
            }
            ok(json!({"name": "operations/1"}))
        }
        _ => bad("not on the fake"),
    }
}

fn apply(audience: &mut FakeAudience, job: &FakeJob) {
    let add = job.add == Some(true);
    for op in &job.ops {
        for id in op[if add { "create" } else { "remove" }]["userIdentifiers"].as_array().unwrap() {
            match (id.get("hashedEmail"), id.get("hashedPhoneNumber")) {
                (Some(e), _) if add => audience.emails.insert(hash(e)),
                (Some(e), _) => audience.emails.remove(&hash(e)),
                (_, Some(p)) if add => audience.phones.insert(hash(p)),
                (_, Some(p)) => audience.phones.remove(&hash(p)),
                _ => panic!("unknown identifier {id}"),
            };
        }
    }
}

fn meta(world: &mut World, method: &Method, path: &str, body: &Value, n: usize) -> Response {
    let ok = |v: Value| json_answer(StatusCode::OK, v);
    let gone = || json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": "Unsupported request. Object does not exist", "code": 100, "error_subcode": 33}}));
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match (method.as_str(), parts.as_slice()) {
        ("GET", ["me", "adaccounts"]) => ok(json!({"data": [{"id": format!("act_{META_ACCOUNT}"), "account_id": META_ACCOUNT, "name": "Acme Meta"}]})),
        ("POST", [account, "customaudiences"]) => {
            assert_eq!((body["subtype"].as_str(), body["customer_file_source"].as_str()), (Some("CUSTOM"), Some("USER_PROVIDED_ONLY")));
            let id = format!("80{n}");
            let account = account.trim_start_matches("act_").to_string();
            world.audiences.insert(id.clone(), FakeAudience { platform: "meta", account, name: body["name"].as_str().unwrap().into(), ..Default::default() });
            ok(json!({"id": id}))
        }
        (m @ ("POST" | "DELETE"), [id, "users"]) => {
            let Some(audience) = world.audiences.get_mut(*id) else { return gone() };
            let payload: Value = serde_json::from_str(body["payload"].as_str().unwrap()).unwrap();
            let schema: Vec<&str> = payload["schema"].as_array().unwrap().iter().map(|s| s.as_str().unwrap()).collect();
            let rows = payload["data"].as_array().unwrap();
            assert!(rows.len() <= 10_000, "Meta takes 10,000 at a time, got {}", rows.len());
            if m == "POST" && world.refuse_email_only && schema == ["EMAIL"] {
                return json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": "Invalid parameter", "code": 100, "error_user_msg": "The payload couldn't be read."}}));
            }
            for row in rows {
                for (key, value) in schema.iter().zip(row.as_array().unwrap()) {
                    let set = if *key == "EMAIL" { &mut audience.emails } else { &mut audience.phones };
                    if m == "POST" { set.insert(hash(value)) } else { set.remove(&hash(value)) };
                }
            }
            ok(json!({"audience_id": id, "num_received": rows.len(), "num_invalid_entries": 0}))
        }
        ("GET", [id]) => match world.audiences.get(*id) {
            Some(a) => ok(json!({"id": id, "approximate_count_lower_bound": if a.emails.is_empty() { -1 } else { a.emails.len() as i64 * 2 }})),
            None => gone(),
        },
        ("DELETE", [id]) => match world.audiences.remove(*id) {
            Some(_) => ok(json!({"success": true})),
            None => gone(),
        },
        _ => json_answer(StatusCode::BAD_REQUEST, json!({"error": {"message": format!("Unsupported request: {method} {path}"), "code": 100}})),
    }
}
