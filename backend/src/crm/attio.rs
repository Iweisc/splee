//! Attio: the lead becomes a person, asserted by email, with a plain-text note. The token is a workspace access token.

use anyhow::Context;
use serde_json::{Value, json};

use super::{client, send, split_name};
use crate::models::Lead;

const API: &str = "https://api.attio.com/v2";

/// The people and notes calls below need these.
const SCOPES: [&str; 3] = ["record_permission:read-write", "object_configuration:read", "note:read-write"];

/// Attio answers 400 for a malformed token and 200 with `active: false` for a revoked one, so both are checked. It also
/// names any missing scopes.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let res = client()?.get(format!("{API}/self")).bearer_auth(token).send().await.map_err(reqwest::Error::without_url)?;
    anyhow::ensure!(!res.status().is_client_error(), "it isn't an Attio access token");
    anyhow::ensure!(res.status().is_success(), "{}", res.status());
    let me: Value = res.json().await?;
    anyhow::ensure!(me["active"] == true, "the token isn't active");
    let missing = missing_scopes(me["scope"].as_str().unwrap_or_default());
    anyhow::ensure!(missing.is_empty(), "the token is missing the {} scopes", missing.join(", "));
    Ok(())
}

/// The SCOPES a token's space-separated grant lacks. A read-write grant covers a read requirement.
fn missing_scopes(granted: &str) -> Vec<&'static str> {
    let granted: Vec<&str> = granted.split_whitespace().collect();
    SCOPES.into_iter().filter(|s| !granted.contains(s) && !granted.contains(&format!("{s}-write").as_str())).collect()
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let (first, last) = split_name(&lead.full_name);
    let mut values = json!({
        "email_addresses": [email.to_lowercase()],
        "name": [{"first_name": first, "last_name": last, "full_name": lead.full_name.trim()}],
    });
    if !lead.title.trim().is_empty() {
        values["job_title"] = json!(lead.title.trim());
    }
    let people = format!("{API}/objects/people/records?matching_attribute=email_addresses");
    let person: Value = send(client.put(people).bearer_auth(token).json(&json!({"data": {"values": values}}))).await?.json().await?;
    let record_id = person["data"]["id"]["record_id"].as_str().context("Attio didn't return the person")?;
    let note = json!({"data": {
        "parent_object": "people", "parent_record_id": record_id, "title": "From Splee", "format": "plaintext", "content": note,
    }});
    send(client.post(format!("{API}/notes")).bearer_auth(token).json(&note)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::missing_scopes;

    #[test]
    fn read_write_covers_read() {
        assert!(missing_scopes("record_permission:read-write object_configuration:read-write note:read-write").is_empty());
        assert_eq!(missing_scopes("record_permission:read object_configuration:read"), ["record_permission:read-write", "note:read-write"]);
    }
}
