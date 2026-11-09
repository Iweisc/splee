//! Affinity: the lead becomes a person, found by exact email or created, and gets a plain-text note. The token is an API
//! key, sent as the basic auth password with no username.

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

const API: &str = "https://api.affinity.co";

/// Checks the key belongs to an Affinity user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, Url::parse(&format!("{API}/auth/whoami"))?, None).await.map(|_| ())
}

/// A wrong key gets a plain-text "Invalid or missing API key". Other failures may be JSON with a `message`.
async fn call(token: &str, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
    let mut request = client()?.request(method, url).basic_auth("", Some(token));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    let body: Value = serde_json::from_str(&text).unwrap_or_default();
    if !status.is_success() {
        let said = body["message"].as_str().or(body["errors"][0]["message"].as_str()).map(str::to_string);
        let plain = (!text.trim_start().starts_with(['{', '<'])).then(|| text.trim().chars().take(200).collect::<String>());
        anyhow::bail!("{}", said.or(plain).filter(|s| !s.is_empty()).unwrap_or_else(|| status.to_string()));
    }
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    let found = call(token, Method::GET, Url::parse_with_params(&format!("{API}/persons"), [("term", email.as_str())])?, None).await?;
    let person_id = match matching_person(&found, &email) {
        Some(id) => id,
        None => {
            let created = call(token, Method::POST, Url::parse(&format!("{API}/persons"))?, Some(person(&email, lead))).await?;
            created["id"].as_i64().context("Affinity didn't return the person")?
        }
    };
    let note = json!({"person_ids": [person_id], "content": note, "type": 0});
    call(token, Method::POST, Url::parse(&format!("{API}/notes"))?, Some(note)).await?;
    Ok(())
}

/// The search matches names too, so a result counts only when one of its addresses is exactly this one.
fn matching_person(found: &Value, email: &str) -> Option<i64> {
    let has = |p: &Value| {
        let primary = p["primary_email"].as_str().into_iter();
        let all = p["emails"].as_array().into_iter().flatten().filter_map(Value::as_str);
        primary.chain(all).any(|e| e.trim().eq_ignore_ascii_case(email))
    };
    found["persons"].as_array()?.iter().find(|p| has(p))?["id"].as_i64()
}

/// A new person. Affinity needs a first name, so a lead without a name is named after their address.
fn person(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let first = if first.is_empty() { email.split('@').next().unwrap_or(email) } else { first };
    json!({"first_name": first, "last_name": last, "emails": [email]})
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::matching_person;

    #[test]
    fn only_a_person_with_the_exact_address_matches() {
        // Affinity's documented search result, trimmed.
        let found = json!({"persons": [
            {"id": 1, "type": 0, "first_name": "Sarah", "last_name": "Chen", "primary_email": "sarah@acme.com.au", "emails": ["sarah@acme.com.au"]},
            {"id": 2, "type": 0, "first_name": "Sarah", "last_name": "Chen", "primary_email": "s.chen@gmail.com", "emails": ["s.chen@gmail.com", "Sarah@Acme.com"]},
        ], "next_page_token": null});
        assert_eq!(matching_person(&found, "sarah@acme.com"), Some(2));
        assert_eq!(matching_person(&found, "nobody@acme.com"), None);
        assert_eq!(matching_person(&json!({}), "sarah@acme.com"), None);
    }
}
