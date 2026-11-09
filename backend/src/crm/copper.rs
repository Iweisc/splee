//! Copper: the lead becomes a person, found by email or created, and gets a note. Copper takes the API key with the
//! email of the user it belongs to, so the saved token is the two a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

use super::client;
use crate::models::Lead;

const API: &str = "https://api.copper.com/developer_api/v1";

/// Checks the email and key reach a Copper account, before they're saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, "/account", None).await?.context("Copper didn't find the account").map(|_| ())
}

/// None for a 404, which is how fetch_by_email says nobody has the address. A wrong email or key gets a 401 `{error}`,
/// and other failures `{message}`.
async fn call(token: &str, method: Method, path: &str, body: Option<Value>) -> anyhow::Result<Option<Value>> {
    let (user, key) = token.split_once('\n').context("reconnect Copper with your email")?;
    let mut request = client()?
        .request(method, format!("{API}{path}"))
        .header("X-PW-AccessToken", key)
        .header("X-PW-Application", "developer_api")
        .header("X-PW-UserEmail", user);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != StatusCode::UNAUTHORIZED, "the email or API key was rejected");
    if status == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Value = res.json().await.unwrap_or_default();
    let said = body["message"].as_str().or(body["error"].as_str());
    anyhow::ensure!(status.is_success(), "{}", said.map_or_else(|| status.to_string(), str::to_string));
    Ok(Some(body))
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    let found = call(token, Method::POST, "/people/fetch_by_email", Some(json!({"email": email}))).await?;
    let person_id = match found.and_then(|p| p["id"].as_i64()) {
        Some(id) => id,
        None => {
            // Copper needs a name, so a lead without one is named after their address.
            let name = match lead.full_name.trim() {
                "" => email.split('@').next().unwrap_or(&email),
                name => name,
            };
            let person = json!({"name": name, "emails": [{"email": email, "category": "work"}]});
            let created = call(token, Method::POST, "/people", Some(person)).await?;
            created.and_then(|p| p["id"].as_i64()).context("Copper didn't return the person")?
        }
    };
    let note = json!({"parent": {"type": "person", "id": person_id}, "type": {"category": "user", "id": 0}, "details": note});
    call(token, Method::POST, "/activities", Some(note)).await?.context("Copper didn't find the person for the note")?;
    Ok(())
}
