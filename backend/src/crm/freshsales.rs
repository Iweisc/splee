//! Freshsales: the lead becomes a contact, found by exact email or created, and gets a note. Freshsales needs the
//! account's bundle alias as well as the API key, so the saved token is the two a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

/// Checks the alias and key reach a Freshsales account, before they're saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, "/selector/owners", &[], None).await.map(|_| ())
}

/// The API's root for a bundle alias as Freshsales shows it, with or without https:// and the path, or just its name.
/// Only Freshworks hosts, so the key is never sent anywhere else.
fn api_root(alias: &str) -> Option<String> {
    let alias = alias.trim().trim_start_matches("https://").trim_start_matches("http://");
    let host = alias.split('/').next()?.to_ascii_lowercase();
    let host = if host.contains('.') { host } else { format!("{host}.myfreshworks.com") };
    let (name, domain) = host.split_once('.')?;
    let valid = !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
    match domain {
        "myfreshworks.com" | "freshworks.com" if valid => Some(format!("https://{host}/crm/sales/api")),
        // Freshsales Classic accounts.
        "freshsales.io" if valid => Some(format!("https://{host}/api")),
        _ => None,
    }
}

/// A failure is `{errors: {code, message}}`, or `{message}` from older endpoints.
async fn call(token: &str, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> anyhow::Result<Value> {
    let (alias, key) = token.split_once('\n').context("reconnect Freshsales with its bundle alias")?;
    let root = api_root(alias).context("the bundle alias should look like acme.myfreshworks.com")?;
    let mut url = Url::parse(&format!("{root}{path}"))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut request = client()?.request(method, url).header("Authorization", format!("Token token={key}"));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    let said = body["errors"]["message"].as_str().or(body["message"].as_str());
    // A bundle alias no account has gets an empty 404.
    anyhow::ensure!(status != reqwest::StatusCode::NOT_FOUND || said.is_some(), "no account has that bundle alias");
    anyhow::ensure!(status.is_success(), "{}", said.map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    // Lookup matches the whole address only.
    let query = [("q", email.as_str()), ("f", "email"), ("entities", "contact")];
    let found = call(token, Method::GET, "/lookup", &query, None).await?;
    let contact_id = match found["contacts"]["contacts"][0]["id"].as_i64() {
        Some(id) => id,
        None => {
            // Upsert, because create doesn't document how it takes an address and upsert does. Nobody has it, so this creates.
            let (first, last) = split_name(&lead.full_name);
            let body = json!({"unique_identifier": {"emails": email}, "contact": {"first_name": first, "last_name": last}});
            let created = call(token, Method::POST, "/contacts/upsert", &[], Some(body)).await?;
            created["contact"]["id"].as_i64().context("Freshsales didn't return the contact")?
        }
    };
    let note = json!({"note": {"description": note, "targetable_type": "Contact", "targetable_id": contact_id}});
    call(token, Method::POST, "/notes", &[], Some(note)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::api_root;

    #[test]
    fn a_bundle_alias_is_read_however_it_was_pasted() {
        let root = Some("https://acme.myfreshworks.com/crm/sales/api".to_string());
        assert_eq!(api_root("acme"), root);
        assert_eq!(api_root(" https://Acme.myfreshworks.com/crm/sales/ "), root);
        assert_eq!(api_root("acme.freshsales.io"), Some("https://acme.freshsales.io/api".to_string()));
        assert_eq!(api_root("acme.example.com"), None);
        assert_eq!(api_root("evil.com/.myfreshworks.com"), None);
        assert_eq!(api_root("a.b.myfreshworks.com"), None);
    }
}
