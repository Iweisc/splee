//! Google Ads Customer Match: a CRM-based user list in the customer account, filled through an offline user data job that's
//! run once its operations are in, and applied by Google later. Each request carries the operator's developer token.
//!
//! Docs:
//! https://developers.google.com/google-ads/api/docs/remarketing/audience-segments/customer-match/get-started
//! https://developers.google.com/google-ads/api/docs/remarketing/audience-segments/customer-match/manage-user-data
//! https://developers.google.com/google-ads/api/rest/reference/rest/v26/customers/listAccessibleCustomers
//! https://developers.google.com/google-ads/api/docs/best-practices/quotas

use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};

use super::{AdAccount, AdError, Client, Member, Platform, Verdict, digits};

pub const API: &str = "https://googleads.googleapis.com";
pub const SCOPE: &str = "https://www.googleapis.com/auth/adwords";
/// Google sunsets each major version about a year after it ships.
const VERSION: &str = "v26";
/// Accounts read when listing, which covers any team. Each one is a request.
const MAX_ACCOUNTS: usize = 20;

fn request(c: &Client, method: Method, path: &str) -> RequestBuilder {
    c.http.request(method, format!("{}/{VERSION}{path}", c.base)).bearer_auth(&c.token).header("developer-token", &c.developer_token)
}

pub(super) fn verdict(status: StatusCode, body: &Value) -> Verdict {
    let error = &body["error"];
    // The details say what was wrong, like a list the account can't use. The message alone is often just "Request contains an
    // invalid argument."
    let detail = error["details"].as_array().into_iter().flatten().flat_map(|d| d["errors"].as_array().into_iter().flatten()).find_map(|e| e["message"].as_str());
    let said = detail.or(error["message"].as_str()).filter(|m| !m.is_empty());
    match status {
        s if s.is_success() => Verdict::Ok,
        StatusCode::UNAUTHORIZED => Verdict::Unauthorized,
        StatusCode::TOO_MANY_REQUESTS => Verdict::Limited,
        _ if error["status"] == "RESOURCE_EXHAUSTED" => Verdict::Limited,
        s => Verdict::Failed(said.map_or_else(|| format!("Google Ads answered {s}"), |m| format!("Google Ads said: {m}"))),
    }
}

/// The customer in a resource name like customers/1234567890, or the list or job in customers/1234567890/userLists/42.
fn resource<'a>(name: &'a str, customer: &str, kind: &str) -> Result<&'a str, AdError> {
    let rest = name.strip_prefix(&format!("customers/{customer}/{kind}/"));
    rest.and_then(|id| digits(id).ok()).map(|_| name).ok_or_else(|| AdError::Failed("Google Ads gave back something Splee didn't expect".into()))
}

async fn search(c: &Client, customer: &str, query: &str) -> Result<Vec<Value>, AdError> {
    let path = format!("/customers/{}/googleAds:search", digits(customer)?);
    Ok(c.send(request(c, Method::POST, &path).json(&json!({ "query": query }))).await?.body["results"].as_array().cloned().unwrap_or_default())
}

/// The accounts the sign-in reaches directly. Manager accounts hold no lists of their own, so they're left out.
pub(super) async fn ad_accounts(c: &Client) -> Result<Vec<AdAccount>, AdError> {
    let found = c.send(request(c, Method::GET, "/customers:listAccessibleCustomers")).await?;
    let ids: Vec<String> = found.body["resourceNames"].as_array().into_iter().flatten().filter_map(|r| r.as_str()?.strip_prefix("customers/").map(str::to_string)).collect();
    let mut accounts = Vec::new();
    for id in ids.iter().filter(|id| digits(id).is_ok()).take(MAX_ACCOUNTS) {
        let rows = match search(c, id, "SELECT customer.id, customer.descriptive_name, customer.manager FROM customer").await {
            Ok(rows) => rows,
            // One account the sign-in can't open, like a cancelled one, doesn't hide the rest.
            Err(AdError::Failed(_)) => continue,
            Err(e) => return Err(e),
        };
        let Some(customer) = rows.first().map(|r| &r["customer"]) else { continue };
        if customer["manager"] == true {
            continue;
        }
        let name = customer["descriptiveName"].as_str().filter(|n| !n.is_empty()).map_or_else(|| format!("Account {id}"), str::to_string);
        accounts.push(AdAccount { id: id.clone(), name });
    }
    Ok(accounts)
}

/// A list of contact info the advertiser collected, kept for the longest Google allows.
pub(super) async fn create(c: &Client, customer: &str, name: &str) -> Result<String, AdError> {
    let list = json!({
        "name": name,
        "description": "Kept in sync by Splee",
        "membershipLifeSpan": 10000,
        "crmBasedUserList": { "uploadKeyType": "CONTACT_INFO", "dataSourceType": "FIRST_PARTY" },
    });
    let path = format!("/customers/{}/userLists:mutate", digits(customer)?);
    let made = c.send(request(c, Method::POST, &path).json(&json!({ "operations": [{ "create": list }] }))).await?;
    let name = made.body["results"][0]["resourceName"].as_str().unwrap_or_default();
    resource(name, customer, "userLists").map(str::to_string)
}

/// One job for the whole change: made, given the operations Platform::batch at a time, then run, and returned so a later sync
/// can read how it went. Google wants a job's adds and removes apart, so it only does one. Consent isn't sent, since Splee
/// can't know what each lead agreed to, which Google takes as unknown.
pub(super) async fn submit(c: &Client, customer: &str, list: &str, members: &[Member], add: bool) -> Result<String, AdError> {
    let list = resource(list, customer, "userLists")?;
    let job = json!({ "job": { "type": "CUSTOMER_MATCH_USER_LIST", "customerMatchUserListMetadata": { "userList": list } } });
    let made = c.send(request(c, Method::POST, &format!("/customers/{customer}/offlineUserDataJobs:create")).json(&job)).await?;
    let job = resource(made.body["resourceName"].as_str().unwrap_or_default(), customer, "offlineUserDataJobs")?.to_string();
    let op = if add { "create" } else { "remove" };
    for batch in members.chunks(Platform::Google.batch()) {
        let operations: Vec<Value> = batch
            .iter()
            .map(|m| {
                let mut ids = vec![json!({ "hashedEmail": m.email_hash })];
                if !m.phone_hash.is_empty() {
                    ids.push(json!({ "hashedPhoneNumber": m.phone_hash }));
                }
                json!({ op: { "userIdentifiers": ids } })
            })
            .collect();
        let added = c.send(request(c, Method::POST, &format!("/{job}:addOperations")).json(&json!({ "operations": operations, "enablePartialFailure": true }))).await?;
        if let Some(m) = added.body["partialFailureError"]["message"].as_str().filter(|m| !m.is_empty()) {
            return Err(AdError::Failed(format!("Google Ads turned down some of the people: {m}")));
        }
    }
    c.send(request(c, Method::POST, &format!("/{job}:run")).json(&json!({}))).await?;
    Ok(job)
}

/// How a job Splee ran is going. Google takes from minutes to a day.
pub(super) enum Job {
    Running,
    Done,
    Failed(String),
}

pub(super) async fn job(c: &Client, customer: &str, job: &str) -> Result<Job, AdError> {
    let job = resource(job, customer, "offlineUserDataJobs")?;
    let query = format!(
        "SELECT offline_user_data_job.status, offline_user_data_job.failure_reason FROM offline_user_data_job WHERE offline_user_data_job.resource_name = '{job}'"
    );
    let rows = search(c, customer, &query).await?;
    let found = rows.first().map(|r| &r["offlineUserDataJob"]);
    Ok(match found.and_then(|j| j["status"].as_str()) {
        Some("SUCCESS") => Job::Done,
        Some("PENDING" | "RUNNING") => Job::Running,
        Some(_) => Job::Failed(found.and_then(|j| j["failureReason"].as_str()).unwrap_or("no reason given").to_string()),
        // Google forgets old jobs. One it no longer has is sent again, which does no harm.
        None => Job::Failed("Google Ads no longer has it".into()),
    })
}

/// The larger of the list's sizes on Search and on Display. Google reports int64 as strings, and rounds small lists down.
pub(super) async fn size(c: &Client, customer: &str, list: &str) -> Result<Option<i64>, AdError> {
    let list = resource(list, customer, "userLists")?;
    let rows = search(c, customer, &format!("SELECT user_list.size_for_display, user_list.size_for_search FROM user_list WHERE user_list.resource_name = '{list}'")).await?;
    let Some(row) = rows.first() else { return Err(AdError::Gone) };
    let number = |v: &Value| v.as_i64().or_else(|| v.as_str()?.parse().ok());
    Ok([&row["userList"]["sizeForDisplay"], &row["userList"]["sizeForSearch"]].into_iter().filter_map(number).max())
}

pub(super) async fn delete(c: &Client, customer: &str, list: &str) -> Result<(), AdError> {
    let list = resource(list, customer, "userLists")?;
    c.send(request(c, Method::POST, &format!("/customers/{customer}/userLists:mutate")).json(&json!({ "operations": [{ "remove": list }] }))).await.map(drop)
}
