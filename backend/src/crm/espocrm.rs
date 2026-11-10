//! EspoCRM: the lead becomes a contact, found by email or created, and gets a post on its stream. EspoCRM is self-hosted,
//! so the saved token is the site's address and an API user's key, a line apart (see Crm::account), and requests only go
//! to a public address (see hooks::public_client).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{plain_markdown, split_name};
use crate::hooks;
use crate::models::Lead;

/// Checks the site and key reach an API user, before they're saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, "/App/user", &[], None).await.map(|_| ())
}

/// The API's root for the site's address as its users open it, like https://crm.acme.com or crm.acme.com/espo/.
fn api_root(site: &str) -> anyhow::Result<Url> {
    let site = site.trim().trim_end_matches('/');
    let site = if site.contains("://") { site.to_string() } else { format!("https://{site}") };
    let url = Url::parse(&format!("{}/api/v1", site.trim_end_matches("/api/v1"))).ok().filter(|u| u.host_str().is_some());
    url.context("the site URL should look like https://crm.acme.com")
}

/// A failure has an empty body and says why in its X-Status-Reason header.
async fn call(token: &str, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> anyhow::Result<Value> {
    let (site, key) = token.split_once('\n').context("reconnect EspoCRM with its site URL")?;
    let root = api_root(site)?;
    let mut url = Url::parse(&format!("{root}{path}"))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut request = hooks::public_client(&url).await?.request(method, url).header("X-Api-Key", key);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = match request.send().await {
        Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {}, so check the site URL", root.host_str().unwrap_or_default()),
        res => res.map_err(reqwest::Error::without_url)?,
    };
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key was rejected");
    anyhow::ensure!(!status.is_redirection(), "the site redirected, so paste the address EspoCRM opens at, with https://");
    let reason = res.headers().get("x-status-reason").and_then(|v| v.to_str().ok()).map(|r| r.trim_end_matches('.').to_string());
    if status == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("the API user's role doesn't allow it ({}), so give it Contacts access", reason.as_deref().unwrap_or("forbidden"));
    }
    anyhow::ensure!(status.is_success(), "{}", reason.unwrap_or_else(|| status.to_string()));
    res.json().await.context("EspoCRM didn't answer with JSON, so check the site URL")
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    // `equals` on an address is exact and ignores case, so the first contact found is theirs.
    let query = [("where[0][type]", "equals"), ("where[0][attribute]", "emailAddress"), ("where[0][value]", &email), ("select", "id"), ("maxSize", "1")];
    let found = call(token, Method::GET, "/Contact", &query, None).await?;
    let contact_id = match found["list"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let (first, last) = contact_name(&lead.full_name, &email);
            let contact = json!({"firstName": first, "lastName": last, "emailAddress": email});
            let created = call(token, Method::POST, "/Contact", &[], Some(contact)).await?;
            created["id"].as_str().context("EspoCRM didn't return the contact")?.to_string()
        }
    };
    let post = json!({"type": "Post", "parentType": "Contact", "parentId": contact_id, "post": plain_markdown(note)});
    call(token, Method::POST, "/Note", &[], Some(post)).await?;
    Ok(())
}

/// EspoCRM needs a last name, so a one-word name is the last name, and a lead without a name is named after their address.
fn contact_name<'a>(full_name: &'a str, email: &'a str) -> (&'a str, &'a str) {
    match split_name(full_name) {
        ("", _) => ("", email.split('@').next().unwrap_or(email)),
        (one, "") => ("", one),
        both => both,
    }
}

#[cfg(test)]
mod tests {
    use super::{api_root, contact_name};

    #[test]
    fn the_site_url_becomes_the_api_root() {
        let root = |site: &str| api_root(site).map(|u| u.to_string()).unwrap_or_default();
        assert_eq!(root("https://crm.acme.com/"), "https://crm.acme.com/api/v1");
        assert_eq!(root(" crm.acme.com/espo "), "https://crm.acme.com/espo/api/v1");
        assert_eq!(root("https://crm.acme.com/api/v1/"), "https://crm.acme.com/api/v1");
        assert_eq!(root("not a url"), "");
    }

    #[test]
    fn every_contact_has_a_last_name() {
        assert_eq!(contact_name("Sarah van Dijk", "s@x.com"), ("Sarah", "van Dijk"));
        assert_eq!(contact_name("Cher", "c@x.com"), ("", "Cher"));
        assert_eq!(contact_name(" ", "pat@x.com"), ("", "pat"));
    }
}
