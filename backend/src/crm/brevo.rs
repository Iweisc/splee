//! Brevo: the lead becomes a contact, found by email or created with their name, and gets a CRM note. The token is an
//! API key.

use anyhow::Context;
use serde_json::{Value, json};

use super::{client, note_html, split_name};
use crate::models::Lead;

const API: &str = "https://api.brevo.com/v3";

/// Checks the key opens the account, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(client()?.get(format!("{API}/account")).header("api-key", token)).await?.context("Brevo didn't find the account")?;
    Ok(())
}

/// Brevo explains a failure in `message`, like "Key not found". A missing contact is None, not an error.
async fn call(request: reqwest::RequestBuilder) -> anyhow::Result<Option<Value>> {
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["message"].as_str().unwrap_or(status.as_str()));
    Ok(Some(body))
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    let found = call(client.get(format!("{API}/contacts/{}?identifierType=email_id", encode(&email))).header("api-key", token)).await?;
    let contact_id = match found {
        Some(contact) => contact["id"].as_i64(),
        None => call(client.post(format!("{API}/contacts")).header("api-key", token).json(&contact(&email, lead))).await?.and_then(|c| c["id"].as_i64()),
    }
    .context("Brevo didn't return the contact")?;
    let note = json!({"text": note_html(note), "contactIds": [contact_id]});
    call(client.post(format!("{API}/crm/notes")).header("api-key", token).json(&note)).await?.context("Brevo didn't take the note")?;
    Ok(())
}

/// A new contact with the name attributes every Brevo account has. Blank names are left out.
fn contact(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let mut attributes = json!({});
    for (key, value) in [("FIRSTNAME", first), ("LASTNAME", last)] {
        if !value.is_empty() {
            attributes[key] = json!(value);
        }
    }
    json!({"email": email, "attributes": attributes})
}

/// The email goes in the path, where a `+` or `/` would change what Brevo looks up.
fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-._~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn emails_are_encoded_for_the_path() {
        assert_eq!(super::encode("sam+leads@acme.io"), "sam%2Bleads%40acme.io");
    }
}
