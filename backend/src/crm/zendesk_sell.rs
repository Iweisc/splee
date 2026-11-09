//! Zendesk Sell: the lead becomes a person contact, found by exact email or created, with a note. The token is a
//! personal access token.

use anyhow::Context;
use reqwest::Url;
use serde_json::{Value, json};

use super::{client, split_name};
use crate::models::Lead;

const API: &str = "https://api.getbase.com/v2";

/// Sell turns away requests without a User-Agent, and reqwest sends none by default.
const USER_AGENT: &str = "Splee";

/// Checks the token belongs to a Sell user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(client()?.get(format!("{API}/users/self")).bearer_auth(token)).await.map(|_| ())
}

/// Sell explains a failure in the first of `errors`, with the field it's about when validation failed.
async fn call(request: reqwest::RequestBuilder) -> anyhow::Result<Value> {
    let res = request.header("user-agent", USER_AGENT).send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    let error = &body["errors"][0]["error"];
    let message = error["message"].as_str().unwrap_or(status.as_str());
    let field = error["field"].as_str().map(|f| format!(" ({})", f.trim_start_matches("/data/"))).unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{message}{field}");
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let client = client()?;
    let email = email.to_lowercase();
    // An organization can have the same email, and the note belongs on the person.
    let search = Url::parse_with_params(&format!("{API}/contacts"), [("email", email.as_str()), ("is_organization", "false"), ("per_page", "1")])?;
    let found = call(client.get(search).bearer_auth(token)).await?;
    let contact_id = match found["items"][0]["data"]["id"].as_i64() {
        Some(id) => id,
        None => {
            let created = call(client.post(format!("{API}/contacts")).bearer_auth(token).json(&json!({"data": person(&email, lead)}))).await?;
            created["data"]["id"].as_i64().context("Zendesk Sell didn't return the contact")?
        }
    };
    let entry = json!({"data": {"resource_type": "contact", "resource_id": contact_id, "content": note}});
    call(client.post(format!("{API}/notes")).bearer_auth(token).json(&entry)).await?;
    Ok(())
}

/// A new person. Blank fields are left out.
fn person(email: &str, lead: &Lead) -> Value {
    let (first, last) = names(&lead.full_name, email);
    let mut new = json!({"is_organization": false, "email": email, "last_name": last});
    for (key, value) in [("first_name", first), ("title", lead.title.trim())] {
        if !value.is_empty() {
            new[key] = json!(value);
        }
    }
    new
}

/// Sell needs a last name, so a one-word name goes there, and a lead with no name is filed under their email.
fn names<'a>(full: &'a str, email: &'a str) -> (&'a str, &'a str) {
    match split_name(full) {
        ("", _) => ("", email),
        (only, "") => ("", only),
        both => both,
    }
}

#[cfg(test)]
mod tests {
    use super::names;

    #[test]
    fn every_new_contact_has_a_last_name() {
        assert_eq!(names("Sarah van Dijk", "s@acme.com"), ("Sarah", "van Dijk"));
        assert_eq!(names("Cher", "c@acme.com"), ("", "Cher"));
        assert_eq!(names("  ", "x@acme.com"), ("", "x@acme.com"));
    }
}
