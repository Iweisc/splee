//! Insightly: the lead becomes a contact, found by exact email or created, and gets a note. A key works only on its
//! account's pod, so the saved token is the API URL Insightly shows and the key, a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, note_html, split_name};
use crate::models::Lead;

/// Checks the URL and key reach an Insightly user, before they're saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, "/Users/Me", &[], None).await.map(|_| ())
}

/// The API's root for the API URL as Insightly shows it, like https://api.na1.insightly.com/v3.1/, or just its pod.
/// Only Insightly hosts, so the key is never sent anywhere else.
fn api_root(url: &str) -> Option<String> {
    let url = url.trim().trim_start_matches("https://").trim_start_matches("http://");
    let host = url.split('/').next()?.to_ascii_lowercase();
    let pod = if host.contains('.') { host.strip_prefix("api.")?.strip_suffix(".insightly.com")?.to_string() } else { host };
    (!pod.is_empty() && pod.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| format!("https://api.{pod}.insightly.com/v3.1"))
}

/// A failure is `{Message}`. A pod that doesn't exist has no address, so that says to check the URL.
async fn call(token: &str, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> anyhow::Result<Value> {
    let (url, key) = token.split_once('\n').context("reconnect Insightly with its API URL")?;
    let root = api_root(url).context("the API URL should look like https://api.na1.insightly.com/v3.1/")?;
    let mut url = Url::parse(&format!("{root}{path}"))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut request = client()?.request(method, url).basic_auth(key, Some(""));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = match request.send().await {
        Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {root}, so check the API URL"),
        res => res.map_err(reqwest::Error::without_url)?,
    };
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["Message"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    let found = call(token, Method::GET, "/Contacts/Search", &[("field_name", "EMAIL_ADDRESS"), ("field_value", &email)], None).await?;
    let contact_id = match matching_contact(&found, &email) {
        Some(id) => id,
        None => {
            // Insightly needs a first name, so a lead without a name is named after their address.
            let (first, last) = split_name(&lead.full_name);
            let first = if first.is_empty() { email.split('@').next().unwrap_or(&email) } else { first };
            let contact = json!({"FIRST_NAME": first, "LAST_NAME": last, "EMAIL_ADDRESS": email});
            let created = call(token, Method::POST, "/Contacts", &[], Some(contact)).await?;
            created["CONTACT_ID"].as_i64().context("Insightly didn't return the contact")?
        }
    };
    // A note needs a title, and the note's first line says what happened.
    let title: String = note.lines().next().unwrap_or_default().chars().take(200).collect();
    let note = json!({"TITLE": title, "BODY": note_html(note)});
    call(token, Method::POST, &format!("/Contacts/{contact_id}/Notes"), &[], Some(note)).await?;
    Ok(())
}

/// Only a contact whose address is exactly this one, whatever the search matched.
fn matching_contact(found: &Value, email: &str) -> Option<i64> {
    found.as_array()?.iter().find(|c| c["EMAIL_ADDRESS"].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email)))?["CONTACT_ID"].as_i64()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{api_root, matching_contact};

    #[test]
    fn the_api_url_is_read_however_it_was_pasted() {
        let root = Some("https://api.na1.insightly.com/v3.1".to_string());
        assert_eq!(api_root("https://api.na1.insightly.com/v3.1/"), root);
        assert_eq!(api_root(" API.NA1.insightly.com "), root);
        assert_eq!(api_root("na1"), root);
        assert_eq!(api_root("api.na1.insightly.com.evil.com"), None);
        assert_eq!(api_root("crm.na1.insightly.com"), None);
        assert_eq!(api_root("api.a.b.insightly.com"), None);
    }

    #[test]
    fn only_a_contact_with_the_exact_address_matches() {
        let found = json!([{"CONTACT_ID": 1, "EMAIL_ADDRESS": "sarah@acme.com.au"}, {"CONTACT_ID": 2, "EMAIL_ADDRESS": "Sarah@Acme.com"}]);
        assert_eq!(matching_contact(&found, "sarah@acme.com"), Some(2));
        assert_eq!(matching_contact(&found, "nobody@acme.com"), None);
    }
}
