//! Pipedrive: the lead becomes a person, found by exact email or created, with a note. The token is a user's personal
//! API token. It goes in a header, never the URL, because request errors quote the URL and those get saved and shown.

use anyhow::Context;
use reqwest::Url;
use serde_json::{Value, json};

use super::{client, note_html};
use crate::models::Lead;

const API: &str = "https://api.pipedrive.com/api";

/// Checks the token belongs to a user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    client()?.get(format!("{API}/v1/users/me")).header("x-api-token", token).send().await?.error_for_status()?;
    Ok(())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    let search = Url::parse_with_params(&format!("{API}/v2/persons/search"), [("term", email.as_str()), ("fields", "email"), ("exact_match", "true")])?;
    let found: Value = client.get(search).header("x-api-token", token).send().await?.error_for_status()?.json().await?;
    let person_id = match found["data"]["items"][0]["item"]["id"].as_i64() {
        Some(id) => id,
        None => {
            let name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
            let created: Value = client
                .post(format!("{API}/v2/persons"))
                .header("x-api-token", token)
                .json(&json!({"name": name, "emails": [{"value": email, "primary": true, "label": "work"}]}))
                .send()
                .await?
                .error_for_status()?
                .json()
                .await?;
            created["data"]["id"].as_i64().context("Pipedrive didn't return the person")?
        }
    };
    client
        .post(format!("{API}/v1/notes"))
        .header("x-api-token", token)
        .json(&json!({"content": note_html(note), "person_id": person_id}))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}
