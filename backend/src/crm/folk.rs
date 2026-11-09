//! folk: the lead becomes a person, found by exact email or created with their company, and gets a note. The token is
//! a workspace API key.

use anyhow::Context;
use reqwest::Url;
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

const API: &str = "https://api.folk.app/v1";

/// Checks the key belongs to a workspace user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(client()?.get(format!("{API}/users/me")).bearer_auth(token)).await.map(|_| ())
}

/// folk explains a failure in `error.message`, which reads better than the status and the URL, and the search URL holds
/// the lead's email.
async fn call(request: reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["error"]["message"].as_str().unwrap_or(status.as_str()));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    let search = Url::parse_with_params(&format!("{API}/people"), [("filter[emails][eq]", email.as_str())])?;
    let found = call(client.get(search).bearer_auth(token)).await?;
    let person_id = match found["data"]["items"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let created = call(client.post(format!("{API}/people")).bearer_auth(token).json(&person(&email, lead))).await?;
            created["data"]["id"].as_str().context("folk didn't return the person")?.to_string()
        }
    };
    let note = json!({"entity": {"id": person_id}, "visibility": "public", "content": note});
    call(client.post(format!("{API}/notes")).bearer_auth(token).json(&note)).await?;
    Ok(())
}

/// A new person. Blank fields are left out.
fn person(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let mut new = json!({"emails": [email]});
    for (key, value) in [("firstName", first), ("lastName", last), ("jobTitle", lead.title.trim())] {
        if !value.is_empty() {
            new[key] = json!(value);
        }
    }
    if !lead.company_name.trim().is_empty() {
        new["companies"] = json!([{"name": lead.company_name.trim()}]);
    }
    new
}
