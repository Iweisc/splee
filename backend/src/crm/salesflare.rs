//! Salesflare: the lead becomes a contact, found by exact email or created with an account for their company, and the
//! note goes on that account, since Salesflare keeps internal notes on accounts only. The token is an API key.

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

const API: &str = "https://api.salesflare.com";

/// Checks the key belongs to a Salesflare user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, url("/me", &[])?, None).await.map(|_| ())
}

/// A failure comes back as `{statusCode, error, message}`. A wrong key gets "Missing authentication", which reads like
/// no key was sent, so a 401 says what happened.
async fn call(token: &str, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
    let mut request = client()?.request(method, url).bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "The API key is wrong or was deleted");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

fn url(path: &str, query: &[(&str, &str)]) -> anyhow::Result<Url> {
    let mut url = Url::parse(&format!("{API}{path}"))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    Ok(url)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    let found = call(token, Method::GET, url("/contacts", &[("email", &email)])?, None).await?;
    let account_id = match matching_contact(&found, &email) {
        Some((_, Some(account_id))) => account_id,
        Some((contact_id, None)) => {
            let account_id = account(token, &email, lead).await?;
            call(token, Method::POST, url(&format!("/accounts/{account_id}/contacts"), &[])?, Some(json!([{"id": contact_id}]))).await?;
            account_id
        }
        None => {
            let account_id = account(token, &email, lead).await?;
            call(token, Method::POST, url("/contacts", &[])?, Some(contact(&email, lead, account_id))).await?;
            account_id
        }
    };
    call(token, Method::POST, url("/messages", &[])?, Some(json!({"account": account_id, "body": note}))).await?;
    Ok(())
}

/// The contact with exactly this address, as (contact id, account id), since the email filter may match loosely. A
/// contact lists its account as an object, or as a bare id in some responses.
fn matching_contact(found: &Value, email: &str) -> Option<(i64, Option<i64>)> {
    let contact = found.as_array()?.iter().find(|c| c["email"].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email)))?;
    Some((contact["id"].as_i64()?, contact["account"]["id"].as_i64().or(contact["account"].as_i64())))
}

/// The account of the lead's company, found by name or created. A lead without a company gets an account of their own,
/// because a note needs one.
async fn account(token: &str, email: &str, lead: &Lead) -> anyhow::Result<i64> {
    let name = [lead.company_name.trim(), lead.full_name.trim(), email].into_iter().find(|n| !n.is_empty()).unwrap_or(email);
    let found = call(token, Method::GET, url("/accounts", &[("name", name)])?, None).await?;
    let same_name = found.as_array().and_then(|a| a.iter().find(|a| a["name"].as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(name))));
    if let Some(id) = same_name.and_then(|a| a["id"].as_i64()) {
        return Ok(id);
    }
    let created = call(token, Method::POST, url("/accounts", &[])?, Some(json!({"name": name}))).await?;
    created["id"].as_i64().context("Salesflare didn't return the account")
}

/// A new contact on `account_id`. Blank fields are left out.
fn contact(email: &str, lead: &Lead, account_id: i64) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let mut new = json!({"email": email, "account": account_id});
    for (key, value) in [("firstname", first), ("lastname", last)] {
        if !value.is_empty() {
            new[key] = json!(value);
        }
    }
    if !lead.title.trim().is_empty() {
        new["position"] = json!({"role": lead.title.trim(), "organisation": lead.company_name.trim()});
    }
    new
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::matching_contact;

    #[test]
    fn only_the_exact_address_matches_and_its_account_is_read_either_way() {
        let found = json!([
            {"id": 1, "email": "sarah@acme.com.au", "account": {"id": 10}},
            {"id": 2, "email": "Sarah@Acme.com", "account": {"id": 20, "name": "Acme"}},
        ]);
        assert_eq!(matching_contact(&found, "sarah@acme.com"), Some((2, Some(20))));
        assert_eq!(matching_contact(&json!([{"id": 3, "email": "jo@acme.com", "account": 30}]), "jo@acme.com"), Some((3, Some(30))));
        assert_eq!(matching_contact(&json!([{"id": 4, "email": "jo@acme.com", "account": null}]), "jo@acme.com"), Some((4, None)));
        assert_eq!(matching_contact(&found, "nobody@acme.com"), None);
        assert_eq!(matching_contact(&json!({}), "sarah@acme.com"), None);
    }
}
