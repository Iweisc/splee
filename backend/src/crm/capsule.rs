//! Capsule: the lead becomes a person, found by exact email or created with their organisation, and gets a note. The
//! token is a personal access token.

use anyhow::Context;
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

const API: &str = "https://api.capsulecrm.com/api/v2";

/// Checks the token belongs to a Capsule user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(client()?.get(format!("{API}/users/current")).bearer_auth(token)).await.map(|_| ())
}

/// Capsule explains a failure in `message`, the first of `errors` when validation failed, or `error` when rate limited.
async fn call(request: reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let message = body["errors"][0]["message"].as_str().or(body["message"].as_str()).or(body["error"].as_str()).unwrap_or(status.as_str());
    anyhow::ensure!(status.is_success(), "{message}");
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    // An organisation can have the same email, and the note belongs on the person.
    let conditions = json!([{"field": "type", "operator": "is", "value": "person"}, {"field": "email", "operator": "is", "value": email}]);
    let filter = json!({"filter": {"conditions": conditions}});
    let found = call(client.post(format!("{API}/parties/filters/results?perPage=1")).bearer_auth(token).json(&filter)).await?;
    let party_id = match found["parties"][0]["id"].as_i64() {
        Some(id) => id,
        None => {
            let created = call(client.post(format!("{API}/parties")).bearer_auth(token).json(&person(&email, lead))).await?;
            created["party"]["id"].as_i64().context("Capsule didn't return the person")?
        }
    };
    let entry = json!({"entry": {"type": "note", "party": {"id": party_id}, "content": note}});
    call(client.post(format!("{API}/entries")).bearer_auth(token).json(&entry)).await?;
    Ok(())
}

/// A new person. Blank fields are left out, and Capsule creates the organisation when none has that name.
fn person(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let mut new = json!({"type": "person", "emailAddresses": [{"type": "Work", "address": email}]});
    for (key, value) in [("firstName", first), ("lastName", last), ("jobTitle", lead.title.trim())] {
        if !value.is_empty() {
            new[key] = json!(value);
        }
    }
    if !lead.company_name.trim().is_empty() {
        new["organisation"] = json!({"name": lead.company_name.trim()});
    }
    json!({"party": new})
}
