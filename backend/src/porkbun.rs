//! Porkbun's API v3 (https://porkbun.com/api/json/v3/documentation), for checking, buying and setting up sending domains.
//! Keys are made at porkbun.com/account/api. Buying spends the account's prepaid credit, never a saved card on its own.

use std::time::Duration;

use chrono::NaiveDate;
use reqwest::{Method, StatusCode};
use serde::Serialize;
use serde_json::{Value, json};

pub const API_URL: &str = "https://api.porkbun.com/api/json/v3";

pub struct Porkbun {
    client: reqwest::Client,
    base: String,
    key: String,
    secret: String,
}

/// Why a call didn't go through. Only `Unknown` can have spent money: the request went out and no clear answer came back.
#[derive(Debug)]
pub enum Failure {
    Refused { code: String, message: String },
    Unknown(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::Refused { message, .. } | Failure::Unknown(message) => f.write_str(message),
        }
    }
}

impl Failure {
    fn code(&self) -> &str {
        match self {
            Failure::Refused { code, .. } => code,
            Failure::Unknown(_) => "",
        }
    }
}

#[derive(Debug, Serialize, PartialEq)]
pub struct Availability {
    pub domain: String,
    /// None when the registry didn't answer in time, which says nothing either way.
    pub available: Option<bool>,
    /// The first registration, for the registry's minimum term, which is what buying it charges.
    pub price_micros: i64,
    pub renewal_micros: i64,
    /// Porkbun doesn't sell premium names through the API.
    pub premium: bool,
}

/// A DNS record as Porkbun's /dns/create takes it, with `name` relative to the domain ("" for the root). Splee keeps the
/// root's URL forward in the same list as type "URL".
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct Record {
    #[serde(rename = "type")]
    pub kind: String,
    pub name: String,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prio: Option<i32>,
    #[serde(default)]
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub error: String,
    /// Pasted from the mail provider rather than planned by Splee, so setting DNS again keeps it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub added: bool,
}

impl Record {
    pub fn new(kind: &str, name: &str, content: impl Into<String>, prio: Option<i32>) -> Self {
        Self { kind: kind.into(), name: name.into(), content: content.into(), prio, ok: false, error: String::new(), added: false }
    }
}

fn dollars_to_micros(v: &Value) -> i64 {
    v.as_str().and_then(|s| s.parse::<f64>().ok()).map_or(0, |usd| (usd * 100.0).round() as i64 * 10_000)
}

/// Porkbun's message, in plain words for the codes people can do something about.
fn explain(body: &Value) -> Failure {
    let code = body["code"].as_str().unwrap_or_default().to_string();
    let said = body["message"].as_str().map(str::trim).filter(|m| !m.is_empty()).unwrap_or("Porkbun refused the request");
    let message = match code.as_str() {
        "INSUFFICIENT_FUNDS" => {
            let short = body["shortfall"].as_i64().map(|c| format!(" by ${}.{:02}", c / 100, c % 100)).unwrap_or_default();
            format!("Your Porkbun credit is too low{short}. Add credit at porkbun.com/account/credit, then buy it again.")
        }
        "COST_MISMATCH" => "Porkbun's price changed since the search. Search again to see the new one.".into(),
        "MONTHLY_SPEND_LIMIT_EXCEEDED" => "This goes past your Porkbun monthly API spend limit. Raise it at porkbun.com/account/api.".into(),
        "DOMAIN_NOT_AVAILABLE" => "Someone registered it since the search.".into(),
        "API_ACCESS_DISABLED" => "Porkbun hasn't opened this domain to the API. Turn on API access for it at porkbun.com/account, then try again.".into(),
        "RATE_LIMIT_EXCEEDED" => "Porkbun is limiting requests. Wait a minute and try again.".into(),
        "INSUFFICIENT_SCOPE" => "This Porkbun API key isn't allowed to do that. Give it more permissions at porkbun.com/account/api.".into(),
        _ => said.to_string(),
    };
    Failure::Refused { code, message }
}

impl Porkbun {
    pub fn new(base: &str, key: &str, secret: &str) -> Self {
        // Buying waits on the registry, which can take a while. A call cut off sooner would leave the purchase unclear.
        let client = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().expect("reqwest client builds");
        Self { client, base: base.trim_end_matches('/').into(), key: key.trim().into(), secret: secret.trim().into() }
    }

    async fn call(&self, method: Method, path: &str, body: Option<Value>, idempotency_key: Option<&str>) -> Result<Value, Failure> {
        let mut request = self.client.request(method, format!("{}{path}", self.base)).header("X-API-Key", &self.key).header("X-Secret-API-Key", &self.secret);
        if let Some(body) = body {
            request = request.json(&body);
        }
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        let res = match request.send().await {
            Ok(res) => res,
            // A connection that never opened sent nothing, so nothing was bought.
            Err(e) if e.is_connect() => {
                return Err(Failure::Refused { code: String::new(), message: format!("Couldn't reach Porkbun: {}", e.without_url()) });
            }
            Err(e) => return Err(Failure::Unknown(format!("Porkbun didn't answer: {}", e.without_url()))),
        };
        let status = res.status();
        let body: Option<Value> = res.json().await.ok();
        match body {
            Some(b) if status.is_success() && b["status"] == "SUCCESS" => Ok(b),
            // Porkbun charges nothing for an error it reports itself.
            Some(b) if (status.is_client_error() || status == StatusCode::OK) && b["status"] == "ERROR" => Err(explain(&b)),
            _ => Err(Failure::Unknown(format!("Porkbun answered {status} without a clear result"))),
        }
    }

    /// The Porkbun username the key belongs to.
    pub async fn ping(&self) -> Result<String, Failure> {
        let b = self.call(Method::POST, "/ping", Some(json!({})), None).await?;
        if b["credentialsValid"] != true {
            return Err(Failure::Refused { code: String::new(), message: "Porkbun didn't accept this API key and secret.".into() });
        }
        Ok(b["account"].as_str().unwrap_or_default().to_string())
    }

    /// Up to 25 domains in one call. Invalid names are left out.
    pub async fn check(&self, domains: &[String]) -> Result<Vec<Availability>, Failure> {
        let b = self.call(Method::POST, "/domain/checkDomain", Some(json!({ "domains": domains })), None).await?;
        let unresolved: Vec<&str> = b["unresolved"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
        Ok(domains
            .iter()
            .filter_map(|d| {
                let r = &b["domains"][d];
                if r.is_object() {
                    let years = r["minDuration"].as_i64().unwrap_or(1).max(1);
                    Some(Availability {
                        domain: d.clone(),
                        available: Some(r["avail"] == "yes"),
                        price_micros: dollars_to_micros(&r["price"]) * years,
                        renewal_micros: dollars_to_micros(&r["additional"]["renewal"]["price"]),
                        premium: r["premium"] == "yes",
                    })
                } else {
                    unresolved.contains(&d.as_str()).then(|| Availability { domain: d.clone(), available: None, price_micros: 0, renewal_micros: 0, premium: false })
                }
            })
            .collect())
    }

    /// Registers the domain for its minimum term at exactly `price_micros`, else Porkbun refuses with COST_MISMATCH. The
    /// key makes Porkbun replay its first answer to a repeated request instead of buying twice.
    pub async fn create(&self, domain: &str, price_micros: i64, idempotency_key: &str) -> Result<String, Failure> {
        let body = json!({ "cost": price_micros / 10_000, "agreeToTerms": "yes" });
        let b = self.call(Method::POST, &format!("/domain/create/{domain}"), Some(body), Some(idempotency_key)).await?;
        Ok(match &b["orderId"] {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        })
    }

    /// Some(expiry) when the domain is in the account, None when it isn't.
    pub async fn find(&self, domain: &str) -> Result<Option<Option<NaiveDate>>, Failure> {
        match self.call(Method::GET, &format!("/domain/get/{domain}"), None, None).await {
            Ok(b) => Ok(Some(b["domain"]["expireDate"].as_str().and_then(|d| NaiveDate::parse_from_str(d.get(..10)?, "%Y-%m-%d").ok()))),
            Err(e) if e.code() == "DOMAIN_NOT_FOUND" => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Adds the record, or the root's URL forward for type "URL". One that's already there counts as added, so setting a
    /// domain up again only fills in what's missing.
    pub async fn apply(&self, domain: &str, r: &Record) -> Result<(), Failure> {
        let result = if r.kind == "URL" {
            let body = json!({ "subdomain": r.name, "location": r.content, "type": "permanent", "includePath": "no", "wildcard": "no" });
            self.call(Method::POST, &format!("/domain/addUrlForward/{domain}"), Some(body), None).await
        } else {
            let mut body = json!({ "name": r.name, "type": r.kind, "content": r.content, "ttl": 600 });
            if let Some(prio) = r.prio {
                body["prio"] = json!(prio);
            }
            self.call(Method::POST, &format!("/dns/create/{domain}"), Some(body), None).await
        };
        match result {
            Err(e) if matches!(e.code(), "DUPLICATE_RECORD" | "DUPLICATE_FORWARD" | "FORWARD_EXISTS") => Ok(()),
            other => other.map(|_| ()),
        }
    }
}

/// Porkbun as its docs describe it, for tests. The key pair pk1_good / sk1_good works, anything else is refused. Names
/// pick the outcome: "taken" is registered elsewhere, "slow" goes unanswered by its registry, "timeout" is bought but
/// answered too late for the client, and "noapi" is bought with API access off. The account starts with $20 of credit.
#[cfg(test)]
pub mod fake {
    use std::collections::{HashMap, HashSet};
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde_json::{Value, json};

    pub const PRICE_CENTS: i64 = 1108;

    #[derive(Default)]
    pub struct Account {
        pub balance: i64,
        /// Every create call that bought a domain.
        pub bought: Vec<String>,
        pub owned: HashSet<String>,
        /// "domain TYPE name content", in the order they were added.
        pub records: Vec<String>,
        pub replays: HashMap<String, Value>,
    }

    pub type Fake = Arc<Mutex<Account>>;

    type Answer = (StatusCode, Json<Value>);

    fn error(status: StatusCode, code: &str, message: &str) -> Answer {
        (status, Json(json!({ "status": "ERROR", "code": code, "message": message })))
    }

    fn authed(headers: &HeaderMap) -> Result<(), Answer> {
        let pair = (headers.get("x-api-key").and_then(|v| v.to_str().ok()), headers.get("x-secret-api-key").and_then(|v| v.to_str().ok()));
        if pair == (Some("pk1_good"), Some("sk1_good")) { Ok(()) } else { Err(error(StatusCode::BAD_REQUEST, "", "Invalid API key. (002)")) }
    }

    async fn ping(headers: HeaderMap) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        (StatusCode::OK, Json(json!({ "status": "SUCCESS", "yourIp": "203.0.113.9", "credentialsValid": true, "account": "acme-ops" })))
    }

    async fn check(headers: HeaderMap, Json(body): Json<Value>) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        let (mut domains, mut unresolved) = (serde_json::Map::new(), vec![]);
        for d in body["domains"].as_array().unwrap().iter().filter_map(Value::as_str) {
            if d.contains("slow") {
                unresolved.push(d);
                continue;
            }
            let avail = if d.contains("taken") { "no" } else { "yes" };
            let price = if d.ends_with(".io") { "34.00" } else { "11.08" };
            let renewal = json!({ "type": "renewal", "price": "11.08", "regularPrice": "11.08" });
            domains.insert(d.into(), json!({ "avail": avail, "type": "registration", "price": price, "premium": "no", "minDuration": 1, "additional": { "renewal": renewal } }));
        }
        (StatusCode::OK, Json(json!({ "status": "SUCCESS", "checked": domains.len(), "domains": domains, "unresolved": unresolved, "invalid": [] })))
    }

    async fn create(State(fake): State<Fake>, Path(domain): Path<String>, headers: HeaderMap, Json(body): Json<Value>) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        if let Some(replay) = fake.lock().unwrap().replays.get(&key) {
            return (StatusCode::OK, Json(replay.clone()));
        }
        let cost = body["cost"].as_i64().unwrap_or_default();
        let answer = {
            let mut a = fake.lock().unwrap();
            if domain.contains("taken") || a.owned.contains(&domain) {
                return error(StatusCode::BAD_REQUEST, "DOMAIN_NOT_AVAILABLE", "Domain is not available for registration.");
            }
            if cost != PRICE_CENTS {
                return error(StatusCode::BAD_REQUEST, "COST_MISMATCH", "The cost does not match the current price.");
            }
            if a.balance < cost {
                let shortfall = cost - a.balance;
                let b = json!({ "status": "ERROR", "code": "INSUFFICIENT_FUNDS", "message": "Insufficient funds.", "cost": cost, "balance": a.balance, "shortfall": shortfall });
                return (StatusCode::BAD_REQUEST, Json(b));
            }
            a.balance -= cost;
            a.bought.push(domain.clone());
            a.owned.insert(domain.clone());
            let answer = json!({ "status": "SUCCESS", "domain": domain, "cost": cost, "orderId": 12345678, "balance": a.balance });
            a.replays.insert(key, answer.clone());
            answer
        };
        if domain.contains("timeout") {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        (StatusCode::OK, Json(answer))
    }

    async fn find(State(fake): State<Fake>, Path(domain): Path<String>, headers: HeaderMap) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        if !fake.lock().unwrap().owned.contains(&domain) {
            return error(StatusCode::NOT_FOUND, "DOMAIN_NOT_FOUND", "Domain not found in your account.");
        }
        (StatusCode::OK, Json(json!({ "status": "SUCCESS", "domain": { "domain": domain, "status": "ACTIVE", "expireDate": "2027-11-10 23:59:59", "apiAccess": 1 } })))
    }

    fn add(fake: &Fake, domain: &str, line: String) -> Answer {
        let mut a = fake.lock().unwrap();
        if domain.contains("noapi") {
            return error(StatusCode::BAD_REQUEST, "API_ACCESS_DISABLED", "API access is not enabled for this domain.");
        }
        if !a.owned.contains(domain) {
            return error(StatusCode::BAD_REQUEST, "INVALID_DOMAIN", "Invalid domain.");
        }
        if a.records.contains(&line) {
            return error(StatusCode::BAD_REQUEST, "DUPLICATE_RECORD", "A record with this name, type and content already exists.");
        }
        a.records.push(line);
        (StatusCode::OK, Json(json!({ "status": "SUCCESS", "id": 123456789 })))
    }

    async fn dns(State(fake): State<Fake>, Path(domain): Path<String>, headers: HeaderMap, Json(b): Json<Value>) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        let prio = b["prio"].as_i64().map(|p| format!("{p} ")).unwrap_or_default();
        add(&fake, &domain, format!("{domain} {} {} {prio}{}", b["type"].as_str().unwrap(), b["name"].as_str().unwrap(), b["content"].as_str().unwrap()))
    }

    async fn forward(State(fake): State<Fake>, Path(domain): Path<String>, headers: HeaderMap, Json(b): Json<Value>) -> Answer {
        if let Err(e) = authed(&headers) {
            return e;
        }
        add(&fake, &domain, format!("{domain} URL {} {}", b["subdomain"].as_str().unwrap(), b["location"].as_str().unwrap()))
    }

    /// The base URL of a fake Porkbun on a free local port, and its account.
    pub async fn start() -> (String, Fake) {
        let fake = Fake::new(Mutex::new(Account { balance: 2000, ..Default::default() }));
        let app = Router::new()
            .route("/ping", post(ping))
            .route("/domain/checkDomain", post(check))
            .route("/domain/create/{domain}", post(create))
            .route("/domain/get/{domain}", get(find))
            .route("/dns/create/{domain}", post(dns))
            .route("/domain/addUrlForward/{domain}", post(forward))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (base, fake)
    }
}

#[cfg(test)]
pub fn with_timeout(base: &str, key: &str, secret: &str, timeout: Duration) -> Porkbun {
    let client = reqwest::Client::builder().timeout(timeout).build().unwrap();
    Porkbun { client, base: base.into(), key: key.into(), secret: secret.into() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_key_is_checked_and_prices_come_in_micros() {
        let (base, _) = fake::start().await;
        assert_eq!(Porkbun::new(&base, "pk1_good", "sk1_good").ping().await.unwrap(), "acme-ops");
        let bad = Porkbun::new(&base, "pk1_good", "sk1_wrong").ping().await.unwrap_err();
        assert!(matches!(&bad, Failure::Refused { .. }) && bad.to_string().contains("Invalid API key"), "{bad}");

        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        let names = ["getacme.com", "acmetaken.com", "acmeslow.com", "acme.io"].map(String::from);
        let got = pb.check(&names).await.unwrap();
        let summary: Vec<(&str, Option<bool>, i64)> = got.iter().map(|a| (a.domain.as_str(), a.available, a.price_micros)).collect();
        assert_eq!(summary, [("getacme.com", Some(true), 11_080_000), ("acmetaken.com", Some(false), 11_080_000), ("acmeslow.com", None, 0), ("acme.io", Some(true), 34_000_000)]);
        assert_eq!(got[0].renewal_micros, 11_080_000);
    }

    #[tokio::test]
    async fn refusals_say_what_to_do_and_a_late_answer_is_unknown() {
        let (base, fake) = fake::start().await;
        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        let mismatch = pb.create("getacme.com", 9_000_000, "k1").await.unwrap_err();
        assert!(mismatch.to_string().starts_with("Porkbun's price changed"), "{mismatch}");

        fake.lock().unwrap().balance = 500;
        let poor = pb.create("getacme.com", 11_080_000, "k2").await.unwrap_err();
        assert_eq!(poor.to_string(), "Your Porkbun credit is too low by $6.08. Add credit at porkbun.com/account/credit, then buy it again.");
        fake.lock().unwrap().balance = 5000;

        assert_eq!(pb.create("getacme.com", 11_080_000, "k3").await.unwrap(), "12345678");
        // The same key replays the first answer instead of buying again.
        assert_eq!(pb.create("getacme.com", 11_080_000, "k3").await.unwrap(), "12345678");
        assert_eq!(fake.lock().unwrap().bought, ["getacme.com"]);

        let impatient = with_timeout(&base, "pk1_good", "sk1_good", Duration::from_millis(500));
        assert!(matches!(impatient.create("timeoutacme.com", 11_080_000, "k4").await, Err(Failure::Unknown(_))));
        let closed = Porkbun::new("http://127.0.0.1:9", "pk1_good", "sk1_good");
        assert!(matches!(closed.create("getacme.com", 11_080_000, "k5").await, Err(Failure::Refused { .. })));

        assert_eq!(pb.find("getacme.com").await.unwrap(), Some(NaiveDate::from_ymd_opt(2027, 11, 10)));
        assert_eq!(pb.find("tryacme.com").await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_record_already_there_counts_as_added() {
        let (base, fake) = fake::start().await;
        let pb = Porkbun::new(&base, "pk1_good", "sk1_good");
        pb.create("getacme.com", 11_080_000, "k").await.unwrap();
        let mx = Record::new("MX", "", "smtp.google.com", Some(1));
        pb.apply("getacme.com", &mx).await.unwrap();
        pb.apply("getacme.com", &mx).await.unwrap();
        pb.apply("getacme.com", &Record::new("URL", "", "https://acme.com", None)).await.unwrap();
        assert_eq!(fake.lock().unwrap().records, ["getacme.com MX  1 smtp.google.com", "getacme.com URL  https://acme.com"]);
        let off = pb.apply("noapiacme.com", &mx).await.unwrap_err();
        assert!(off.to_string().contains("Turn on API access"), "{off}");
    }
}
