//! Close: the lead becomes a contact on a Close lead (Close's word for a company), found by exact email or created
//! with its company, and the note goes on that lead. The token is an API key, sent as the Basic auth username.

use anyhow::Context;
use serde_json::{Value, json};

use super::client;
use crate::models::Lead;

const API: &str = "https://api.close.com/api/v1";

/// Checks the key belongs to a user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    client()?.get(format!("{API}/me/")).basic_auth(token, Some("")).send().await?.error_for_status()?;
    Ok(())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    // Search reads an index that lags a few seconds behind writes, so a lead pushed twice that quickly can be created
    // twice. Close has no lookup by email that avoids the index.
    let found: Value =
        client.post(format!("{API}/data/search/")).basic_auth(token, Some("")).json(&search(&email)).send().await?.error_for_status()?.json().await?;
    let (lead_id, contact_id) = match found["data"][0]["lead_id"].as_str() {
        Some(lead_id) => (lead_id.to_string(), found["data"][0]["id"].as_str().map(str::to_string)),
        None => {
            let created: Value =
                client.post(format!("{API}/lead/")).basic_auth(token, Some("")).json(&new_lead(&email, lead)).send().await?.error_for_status()?.json().await?;
            (created["id"].as_str().context("Close didn't return the lead")?.to_string(), None)
        }
    };
    client
        .post(format!("{API}/activity/note/"))
        .basic_auth(token, Some(""))
        .json(&json!({"lead_id": lead_id, "contact_id": contact_id, "title": "From Splee", "note": note}))
        .send()
        .await?
        .error_for_status()?;
    Ok(())
}

/// The first contact with exactly this email address, with the Close lead it's on.
fn search(email: &str) -> Value {
    json!({
        "query": {"type": "and", "queries": [
            {"type": "object_type", "object_type": "contact"},
            {"type": "has_related", "this_object_type": "contact", "related_object_type": "contact_email", "related_query": {
                "type": "field_condition",
                "field": {"type": "regular_field", "object_type": "contact_email", "field_name": "email"},
                "condition": {"type": "text", "mode": "exact_value", "value": email},
            }},
        ]},
        "_fields": {"contact": ["id", "lead_id"]},
        "_limit": 1,
    })
}

/// A Close lead for the lead's company, holding the lead as its contact. Blank fields are left out.
fn new_lead(email: &str, lead: &Lead) -> Value {
    let mut contact = json!({"emails": [{"email": email, "type": "office"}]});
    if !lead.full_name.trim().is_empty() {
        contact["name"] = json!(lead.full_name.trim());
    }
    if !lead.title.trim().is_empty() {
        contact["title"] = json!(lead.title.trim());
    }
    let mut new = json!({"contacts": [contact]});
    if !lead.company_name.trim().is_empty() {
        new["name"] = json!(lead.company_name.trim());
    }
    if let Some(domain) = lead.company_domain.as_deref().map(str::trim).filter(|d| !d.is_empty()) {
        new["url"] = json!(format!("https://{domain}"));
    }
    new
}
