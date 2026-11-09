//! HubSpot: the lead becomes a contact, matched by email, with a note. The token is a service key or an older private
//! app's token.

use anyhow::Context;
use serde_json::{Value, json};

use super::{client, note_html, split_name};
use crate::models::Lead;

const API: &str = "https://api.hubapi.com";

/// Checks the token can read contacts, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    client()?.get(format!("{API}/crm/v3/objects/contacts?limit=1")).bearer_auth(token).send().await?.error_for_status()?;
    Ok(())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let upserted: Value = client
        .post(format!("{API}/crm/v3/objects/contacts/batch/upsert"))
        .bearer_auth(token)
        .json(&json!({"inputs": [{"idProperty": "email", "id": email.to_lowercase(), "properties": contact(email, lead)}]}))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let contact_id = upserted["results"][0]["id"].as_str().context("HubSpot didn't return the contact")?;
    // 202 is HubSpot's own note-to-contact association.
    let association = json!({"to": {"id": contact_id}, "types": [{"associationCategory": "HUBSPOT_DEFINED", "associationTypeId": 202}]});
    client
        .post(format!("{API}/crm/v3/objects/notes"))
        .bearer_auth(token)
        .json(&json!({
            "properties": {"hs_timestamp": chrono::Utc::now().to_rfc3339(), "hs_note_body": note_html(note)},
            "associations": [association],
        }))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// The contact's properties. Blank fields are left out so they don't wipe what HubSpot already has.
fn contact(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let fields = [
        ("email", email.to_lowercase()),
        ("firstname", first.to_string()),
        ("lastname", last.to_string()),
        ("jobtitle", lead.title.clone()),
        ("company", lead.company_name.clone()),
        ("website", lead.company_domain.clone().unwrap_or_default()),
    ];
    fields.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), Value::String(v))).collect()
}
