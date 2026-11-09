//! Copies hot and booked leads into the CRMs a project connected, each with a note saying what happened, so the sales
//! team finds them where they work. Tokens are kept encrypted like mailbox passwords and never sent back to the browser.

mod hubspot;
mod pipedrive;

use std::time::Duration;

use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, Alert};
use crate::models::Lead;
use crate::state::AppState;

#[derive(Clone, Copy, Debug)]
pub enum Crm {
    HubSpot,
    Pipedrive,
}

impl Crm {
    pub fn parse(name: &str) -> Option<Crm> {
        match name {
            "hubspot" => Some(Crm::HubSpot),
            "pipedrive" => Some(Crm::Pipedrive),
            _ => None,
        }
    }

    /// Also the prefix of its project_settings columns: {name}_token_enc, {name}_connected, {name}_error, {name}_failed_at.
    pub fn name(self) -> &'static str {
        match self {
            Crm::HubSpot => "hubspot",
            Crm::Pipedrive => "pipedrive",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Crm::HubSpot => "HubSpot",
            Crm::Pipedrive => "Pipedrive",
        }
    }

    /// Checks the CRM accepts the token, so a typo shows up when connecting and not on the first hot lead.
    pub async fn check(self, token: &str) -> anyhow::Result<()> {
        match self {
            Crm::HubSpot => hubspot::check(token).await,
            Crm::Pipedrive => pipedrive::check(token).await,
        }
    }

    async fn push(self, token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
        match self {
            Crm::HubSpot => hubspot::push(token, email, lead, note).await,
            Crm::Pipedrive => pipedrive::push(token, email, lead, note).await,
        }
    }
}

/// Adds or updates the lead in every connected CRM and puts `note` on it. Failures are logged and shown on the
/// integrations page, they never fail the caller.
pub async fn sync(state: &AppState, project_id: Uuid, lead: &Lead, note: &str) {
    let Some(email) = lead.email.as_deref() else { return };
    let tokens: Option<(String, String)> = sqlx::query_as("select hubspot_token_enc, pipedrive_token_enc from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(&state.db)
        .await
        .ok()
        .flatten();
    let Some((hubspot, pipedrive)) = tokens else { return };
    for (crm, enc) in [(Crm::HubSpot, hubspot), (Crm::Pipedrive, pipedrive)] {
        if enc.is_empty() {
            continue;
        }
        let result = match crypto::decrypt(&state.cfg.app_secret, &enc) {
            Ok(token) => crm.push(&token, email, lead, note).await,
            Err(e) => Err(e),
        };
        if let Err(e) = &result {
            tracing::warn!(error = %e, crm = crm.name(), "crm sync failed");
        }
        hooks::record(&state.db, project_id, Alert::Crm(crm), &enc, &result).await;
    }
}

fn client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder().timeout(Duration::from_secs(20)).build()
}

/// Both CRMs render notes as HTML, so the reply is escaped and its line breaks kept.
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
