//! Twenty: the lead becomes a person, found by their primary email or created, and gets a note linked to them.

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::{client, plain_markdown, split_name};
use crate::models::Lead;

const API: &str = "https://api.twenty.com/rest";

/// Checks the key reads people, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, Method::GET, "/people", &[("limit", "1")], None).await.map(|_| ())
}

/// A failure is `{statusCode, error, messages}`, and its messages say what was wrong, where `error` only names the exception.
async fn call(token: &str, method: Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> anyhow::Result<Value> {
    let mut url = Url::parse(&format!("{API}{path}"))?;
    if !query.is_empty() {
        url.query_pairs_mut().extend_pairs(query);
    }
    let mut request = client()?.request(method, url).bearer_auth(token);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the API key or token is wrong or was revoked");
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["messages"][0].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    // ilike finds the address whatever its case in Twenty. It reads _ and % as wildcards, so only an exact match is used.
    let filter = format!("emails.primaryEmail[ilike]:\"{email}\"");
    let found = call(token, Method::GET, "/people", &[("filter", &filter), ("limit", "20")], None).await?;
    let person_id = match matching_person(&found, &email) {
        Some(id) => id,
        None => {
            let (first, last) = split_name(&lead.full_name);
            let person = json!({"name": {"firstName": first, "lastName": last}, "emails": {"primaryEmail": email}, "jobTitle": lead.title.trim()});
            let created = call(token, Method::POST, "/people", &[], Some(person)).await?;
            created["data"]["createPerson"]["id"].as_str().context("Twenty didn't return the person")?.to_string()
        }
    };
    let title: String = note.lines().next().unwrap_or_default().chars().take(200).collect();
    let created = call(token, Method::POST, "/notes", &[], Some(json!({"title": title, "bodyV2": {"markdown": plain_markdown(note)}}))).await?;
    let note_id = created["data"]["createNote"]["id"].as_str().context("Twenty didn't return the note")?;
    call(token, Method::POST, "/noteTargets", &[], Some(json!({"noteId": note_id, "targetPersonId": person_id}))).await?;
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
