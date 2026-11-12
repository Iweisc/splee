//! LinkedIn's Matched Audiences: a DMP segment in the ad account, which hashed addresses are streamed into. LinkedIn matches
//! them to members and makes the ad segment campaigns target.
//!
//! Docs:
//! https://learn.microsoft.com/en-us/linkedin/marketing/matched-audiences/create-and-manage-segments
//! https://learn.microsoft.com/en-us/linkedin/marketing/matched-audiences/create-and-manage-segment-users
//! https://learn.microsoft.com/en-us/linkedin/marketing/matched-audiences/create-and-manage-segment-destinations
//! https://learn.microsoft.com/en-us/linkedin/marketing/integrations/ads/account-structure/create-and-manage-accounts

use reqwest::{Method, RequestBuilder, StatusCode};
use serde_json::{Value, json};

use super::{AdAccount, AdError, Client, Member, Verdict, digits};

pub const API: &str = "https://api.linkedin.com";
pub const AUTHORIZE_URL: &str = "https://www.linkedin.com/oauth/v2/authorization";
pub const TOKEN_URL: &str = "https://www.linkedin.com/oauth/v2/accessToken";
/// Reading the ad accounts, and making and filling DMP segments, which the Audiences program grants.
pub const SCOPE: &str = "r_ads rw_dmp_segments";
/// The Marketing API version every request names. LinkedIn sunsets each one a year after it ships.
const VERSION: &str = "202610";

fn request(c: &Client, method: Method, path: &str) -> RequestBuilder {
    c.http
        .request(method, format!("{}{path}", c.base))
        .bearer_auth(&c.token)
        .header("LinkedIn-Version", VERSION)
        .header("X-Restli-Protocol-Version", "2.0.0")
}

pub(super) fn verdict(status: StatusCode, body: &Value) -> Verdict {
    let said = body["message"].as_str().filter(|m| !m.is_empty()).map(str::to_string);
    match status {
        s if s.is_success() => Verdict::Ok,
        StatusCode::UNAUTHORIZED => Verdict::Unauthorized,
        // Also a 429, but waiting won't help: the account has 1,000 segments.
        StatusCode::TOO_MANY_REQUESTS if body["code"] == "SEGMENT_LIMIT_EXCEEDED" => {
            Verdict::Failed("This LinkedIn ad account has 1,000 audiences already. Delete some in Campaign Manager.".into())
        }
        StatusCode::TOO_MANY_REQUESTS => Verdict::Limited,
        StatusCode::NOT_FOUND => Verdict::Gone,
        StatusCode::FORBIDDEN => Verdict::Failed(format!(
            "LinkedIn didn't let Splee manage audiences in this ad account{}. You need a role above Viewer on it.",
            said.map(|m| format!(" ({m})")).unwrap_or_default()
        )),
        s => Verdict::Failed(said.map_or_else(|| format!("LinkedIn answered {s}"), |m| format!("LinkedIn said: {m}"))),
    }
}

/// The accounts the member can see, without the ones closed for good.
pub(super) async fn ad_accounts(c: &Client) -> Result<Vec<AdAccount>, AdError> {
    let found = c.send(request(c, Method::GET, "/rest/adAccounts?q=search&count=100")).await?;
    let accounts = found.body["elements"].as_array().into_iter().flatten();
    let open = accounts.filter(|a| !matches!(a["status"].as_str(), Some("CANCELED" | "REMOVED")));
    Ok(open
        .filter_map(|a| {
            let id = a["id"].as_i64()?.to_string();
            Some(AdAccount { name: a["name"].as_str().filter(|n| !n.is_empty()).map_or_else(|| format!("Ad account {id}"), str::to_string), id })
        })
        .collect())
}

pub(super) async fn create(c: &Client, account: &str, name: &str) -> Result<String, AdError> {
    let body = json!({
        "name": name,
        "description": "Kept in sync by Splee",
        "sourcePlatform": "PARTNER_API",
        "account": format!("urn:li:sponsoredAccount:{}", digits(account)?),
        "type": "USER",
        "destinations": [{ "destination": "LINKEDIN" }],
    });
    let made = c.send(request(c, Method::POST, "/rest/dmpSegments").json(&body)).await?;
    let id = made.headers.get("x-restli-id").and_then(|v| v.to_str().ok()).unwrap_or_default();
    digits(id).map(str::to_string).map_err(|_| AdError::Failed("LinkedIn didn't say which audience it made".into()))
}

/// LinkedIn answers each element on its own, so one it turned down fails the batch, and the next sync sends it again.
pub(super) async fn users(c: &Client, segment: &str, members: &[Member], add: bool) -> Result<(), AdError> {
    let action = if add { "ADD" } else { "REMOVE" };
    let elements: Vec<Value> =
        members.iter().map(|m| json!({ "action": action, "userIds": [{ "idType": "SHA256_EMAIL", "idValue": m.email_hash }] })).collect();
    let path = format!("/rest/dmpSegments/{}/users", digits(segment)?);
    let sent = c.send(request(c, Method::POST, &path).header("X-RestLi-Method", "BATCH_CREATE").json(&json!({ "elements": elements }))).await?;
    let answers = sent.body["elements"].as_array().cloned().unwrap_or_default();
    let failed: Vec<&Value> = answers.iter().filter(|e| e["status"].as_u64().is_some_and(|s| s >= 400)).collect();
    match failed.first() {
        None => Ok(()),
        Some(first) => Err(AdError::Failed(format!(
            "LinkedIn turned down {} of {} people: {}",
            failed.len(),
            members.len(),
            first["error"]["message"].as_str().unwrap_or("no reason given")
        ))),
    }
}

/// audienceSize, which LinkedIn only reports once the segment is ready, and which leaves out members who opted out.
pub(super) async fn size(c: &Client, segment: &str) -> Result<Option<i64>, AdError> {
    let path = format!("/rest/dmpSegments/{}/destinations/LINKEDIN", digits(segment)?);
    Ok(c.send(request(c, Method::GET, &path)).await?.body["audienceSize"].as_i64())
}

pub(super) async fn delete(c: &Client, segment: &str) -> Result<(), AdError> {
    c.send(request(c, Method::DELETE, &format!("/rest/dmpSegments/{}", digits(segment)?))).await.map(drop)
}
