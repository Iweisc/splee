//! Meta Custom Audiences, made from a customer list in the ad account, through the Graph API base URL and the Meta app the
//! Facebook Page sign-in uses (crate::meta). The token goes in the query or the form, as Meta's docs show it.
//!
//! Docs:
//! https://developers.facebook.com/docs/marketing-api/audiences/guides/custom-audiences
//! https://developers.facebook.com/docs/marketing-api/audiences/reference/custom-audience-terms
//! https://developers.facebook.com/docs/marketing-api/reference/custom-audience/users
//! https://developers.facebook.com/docs/graph-api/overview/rate-limiting

use reqwest::{Method, RequestBuilder, StatusCode, Url};
use serde_json::{Value, json};

use super::{AdAccount, AdError, Client, Member, Verdict, digits};

/// Listing the ad accounts and managing their audiences. Asked for only when audiences are connected.
pub const SCOPE: &str = "ads_management";

fn get(c: &Client, path: &str, query: &[(&str, &str)]) -> Result<RequestBuilder, AdError> {
    let mut url = Url::parse_with_params(&format!("{}{path}", c.base), query).map_err(|e| AdError::Failed(e.to_string()))?;
    url.query_pairs_mut().append_pair("access_token", &c.token);
    Ok(c.http.get(url))
}

/// Posts the form, or deletes with it, so 10,000 hashes stay out of the URL.
fn send_form(c: &Client, method: Method, path: &str, form: &[(&str, &str)]) -> RequestBuilder {
    let mut form = form.to_vec();
    form.push(("access_token", &c.token));
    c.http.request(method, format!("{}{path}", c.base)).form(&form)
}

pub(super) fn verdict(status: StatusCode, body: &Value) -> Verdict {
    let error = &body["error"];
    let code = error["code"].as_i64().unwrap_or_default();
    let said = error["error_user_msg"].as_str().or(error["message"].as_str()).filter(|m| !m.is_empty());
    match status {
        s if s.is_success() => Verdict::Ok,
        StatusCode::UNAUTHORIZED => Verdict::Unauthorized,
        _ if code == 190 || code == 102 => Verdict::Unauthorized,
        // 4 is the app's limit, 17 the user's, 613 a call's and 80003 the Custom Audiences one.
        StatusCode::TOO_MANY_REQUESTS => Verdict::Limited,
        _ if matches!(code, 4 | 17 | 32 | 613 | 80003) => Verdict::Limited,
        // An object that doesn't exist, like an audience deleted in Ads Manager.
        _ if code == 100 && error["error_subcode"] == 33 => Verdict::Gone,
        s => Verdict::Failed(said.map_or_else(|| format!("Meta answered {s}"), |m| format!("Meta said: {m}"))),
    }
}

pub(super) async fn ad_accounts(c: &Client) -> Result<Vec<AdAccount>, AdError> {
    let found = c.send(get(c, "/me/adaccounts", &[("fields", "account_id,name"), ("limit", "100")])?).await?;
    Ok(found.body["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let id = digits(a["account_id"].as_str()?).ok()?.to_string();
            Some(AdAccount { name: a["name"].as_str().filter(|n| !n.is_empty()).map_or_else(|| format!("Ad account {id}"), str::to_string), id })
        })
        .collect())
}

/// Meta turns this down until someone on the account accepts the Custom Audience terms, and says so.
pub(super) async fn create(c: &Client, account: &str, name: &str) -> Result<String, AdError> {
    let form = [("name", name), ("subtype", "CUSTOM"), ("description", "Kept in sync by Splee"), ("customer_file_source", "USER_PROVIDED_ONLY")];
    let made = c.send(send_form(c, Method::POST, &format!("/act_{}/customaudiences", digits(account)?), &form)).await?;
    let id = made.body["id"].as_str().unwrap_or_default();
    digits(id).map(str::to_string).map_err(|_| AdError::Failed("Meta didn't say which audience it made".into()))
}

/// Leads with a number go with the EMAIL and PHONE schema, and the rest in their own request with EMAIL alone, so no row has
/// an empty key.
pub(super) async fn users(c: &Client, audience: &str, members: &[Member], add: bool) -> Result<(), AdError> {
    let path = format!("/{}/users", digits(audience)?);
    let method = if add { Method::POST } else { Method::DELETE };
    let (with_phone, without): (Vec<&Member>, Vec<&Member>) = members.iter().partition(|m| !m.phone_hash.is_empty());
    for (group, phone) in [(with_phone, true), (without, false)] {
        if group.is_empty() {
            continue;
        }
        let schema = if phone { json!(["EMAIL", "PHONE"]) } else { json!(["EMAIL"]) };
        let data: Vec<Value> = group.iter().map(|m| if phone { json!([m.email_hash, m.phone_hash]) } else { json!([m.email_hash]) }).collect();
        let payload = json!({ "schema": schema, "data": data }).to_string();
        c.send(send_form(c, method.clone(), &path, &[("payload", &payload)])).await?;
    }
    Ok(())
}

/// Meta only gives a range, and -1 for an audience still being matched.
pub(super) async fn size(c: &Client, audience: &str) -> Result<Option<i64>, AdError> {
    let found = c.send(get(c, &format!("/{}", digits(audience)?), &[("fields", "approximate_count_lower_bound")])?).await?;
    Ok(found.body["approximate_count_lower_bound"].as_i64().filter(|n| *n >= 0))
}

pub(super) async fn delete(c: &Client, audience: &str) -> Result<(), AdError> {
    c.send(send_form(c, Method::DELETE, &format!("/{}", digits(audience)?), &[])).await.map(drop)
}
