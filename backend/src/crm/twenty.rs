//! Twenty: the lead becomes a person, found by their primary email or created, and gets a note linked to them. It's the
//! cloud at api.twenty.com, or a server the customer runs, whose saved token is the site's address and the key, a line apart
//! (see Crm::account). A self-hosted server only gets requests at a public address (see hooks::public_client).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, plain_markdown, site_root, split_name};
use crate::hooks;
use crate::models::Lead;

const CLOUD: &str = "https://api.twenty.com/";

/// Checks the key reads people, before it's saved.
pub async fn check(token: &str, self_hosted: bool) -> anyhow::Result<()> {
    let api = Api::new(token, self_hosted).await?;
    api.call(Method::GET, "people", &[("limit", "1")], None).await.map(|_| ())
}

struct Api<'a> {
    root: Url,
    key: &'a str,
    client: reqwest::Client,
}

impl<'a> Api<'a> {
    async fn new(token: &'a str, self_hosted: bool) -> anyhow::Result<Api<'a>> {
        if !self_hosted {
            return Ok(Api { root: Url::parse(CLOUD)?, key: token, client: client()? });
        }
        let (site, key) = token.split_once('\n').context("reconnect Twenty with its site URL")?;
        let root = site_root(site).context("the site URL should look like https://crm.acme.com")?;
        Ok(Api { client: hooks::public_client(&root).await?, root, key })
    }

    /// A failure is `{statusCode, error, messages}`, and its messages say what was wrong, where `error` only names the
    /// exception.
    async fn call(&self, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> anyhow::Result<Value> {
        let mut url = self.root.join(&format!("rest/{path}"))?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut request = self.client.request(method, url).bearer_auth(self.key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let host = self.root.host_str().unwrap_or_default();
        let res = match request.send().await {
            Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {host}"),
            res => res.map_err(reqwest::Error::without_url)?,
        };
        let status = res.status();
        anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key or token is wrong or was revoked");
        anyhow::ensure!(!status.is_redirection(), "the site redirected, so paste the address Twenty opens at, with https://");
        let body: Value = res.json().await.unwrap_or_default();
        // Twenty answers every path under /rest, so a 404 without its error body is another site.
        anyhow::ensure!(status != reqwest::StatusCode::NOT_FOUND || body["messages"].is_array(), "{host} isn't a Twenty server, so check the site URL");
        anyhow::ensure!(status.is_success(), "{}", body["messages"][0].as_str().map_or_else(|| status.to_string(), str::to_string));
        Ok(body)
    }
}

pub async fn push(token: &str, self_hosted: bool, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let api = Api::new(token, self_hosted).await?;
    let email = email.to_lowercase();
    // ilike finds the address whatever its case in Twenty. It reads _ and % as wildcards, so only an exact match is used.
    let filter = format!("emails.primaryEmail[ilike]:\"{email}\"");
    let found = api.call(Method::GET, "people", &[("filter", &filter), ("limit", "20")], None).await?;
    let person_id = match matching_person(&found, &email) {
        Some(id) => id,
        None => {
            let (first, last) = split_name(&lead.full_name);
            let person = json!({"name": {"firstName": first, "lastName": last}, "emails": {"primaryEmail": email}, "jobTitle": lead.title.trim()});
            let created = api.call(Method::POST, "people", &[], Some(person)).await?;
            created["data"]["createPerson"]["id"].as_str().context("Twenty didn't return the person")?.to_string()
        }
    };
    let title: String = note.lines().next().unwrap_or_default().chars().take(200).collect();
    let created = api.call(Method::POST, "notes", &[], Some(json!({"title": title, "bodyV2": {"markdown": plain_markdown(note)}}))).await?;
    let note_id = created["data"]["createNote"]["id"].as_str().context("Twenty didn't return the note")?;
    api.call(Method::POST, "noteTargets", &[], Some(json!({"noteId": note_id, "targetPersonId": person_id}))).await?;
    Ok(())
}

/// Only a person whose primary address is exactly this one, whatever the search matched.
fn matching_person(found: &Value, email: &str) -> Option<String> {
    let people = found["data"]["people"].as_array()?;
    let person = people.iter().find(|p| p["emails"]["primaryEmail"].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email)))?;
    person["id"].as_str().map(str::to_string)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::matching_person;

    #[test]
    fn only_a_person_with_the_exact_address_matches() {
        let found = json!({"data": {"people": [
            {"id": "a", "emails": {"primaryEmail": "sarah_x@acme.com"}},
            {"id": "b", "emails": {"primaryEmail": "Sarah.X@Acme.com"}},
        ]}});
        assert_eq!(matching_person(&found, "sarah.x@acme.com"), Some("b".to_string()));
        assert_eq!(matching_person(&found, "sarah@acme.com"), None);
    }
}
