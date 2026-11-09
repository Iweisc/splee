//! Copies hot and booked leads into the project's HubSpot as contacts, each with a note saying what happened, so the
//! sales team finds them in their CRM. The token is a HubSpot service key or an older private app's token. It's kept
//! encrypted like mailbox passwords and never sent back to the browser.

use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;

const API: &str = "https://api.hubapi.com";

/// Adds or updates the lead's contact and puts `note` on it, when the project has a token. Failures are logged and shown
/// on the integrations page, they never fail the caller.
pub async fn sync(state: &AppState, project_id: Uuid, lead: &Lead, note: &str) {
    let Some(email) = lead.email.as_deref() else { return };
    let enc: String = sqlx::query_scalar("select hubspot_token_enc from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    if enc.is_empty() {
        return;
    }
    let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
        Ok(token) => push(&token, email, lead, note).await,
        Err(e) => Err(e),
    };
    if let Err(e) = &result {
        tracing::warn!(error = %e, "hubspot sync failed");
    }
    hooks::record(&state.db, project_id, Alert::HubSpot, &enc, &result).await;
}

/// Checks the token can read contacts, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    client()?.get(format!("{API}/crm/v3/objects/contacts?limit=1")).bearer_auth(token).send().await?.error_for_status()?;
    Ok(())
}

async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
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

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(20)).build()
}

/// The contact's properties. Blank fields are left out so they don't wipe what HubSpot already has.
fn contact(email: &str, lead: &Lead) -> Value {
    let (first, last) = lead.full_name.trim().split_once(' ').unwrap_or((lead.full_name.trim(), ""));
    let fields = [
        ("email", email.to_lowercase()),
        ("firstname", first.to_string()),
        ("lastname", last.trim().to_string()),
        ("jobtitle", lead.title.clone()),
        ("company", lead.company_name.clone()),
        ("website", lead.company_domain.clone().unwrap_or_default()),
    ];
    fields.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), Value::String(v))).collect()
}

/// HubSpot renders note bodies as HTML, so the reply is escaped and its line breaks kept.
fn note_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('\n', "<br>")
}

#[cfg(test)]
mod tests {
    use super::note_html;

    #[test]
    fn notes_keep_lines_and_escape_html() {
        assert_eq!(note_html("Tuesday works <3\nSarah & co"), "Tuesday works &lt;3<br>Sarah &amp; co");
    }
}
