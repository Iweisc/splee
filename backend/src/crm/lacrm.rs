//! Less Annoying CRM: the lead becomes a contact, found by exact email or created and assigned to the token's user, and
//! gets a note. Every call is a POST of `{Function, Parameters}` to one endpoint, with the API key as the whole
//! Authorization header.

use anyhow::Context;
use serde_json::{Value, json};

use super::client;
use crate::models::Lead;

const API: &str = "https://api.lessannoyingcrm.com/v2/";

/// Checks the key belongs to a Less Annoying CRM user, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    call(token, "GetUser", json!({})).await.map(|_| ())
}

/// A failure comes back as a 400 with `ErrorDescription`, like "Invalid credentials. Please make sure your authentication
/// is correct."
async fn call(token: &str, function: &str, parameters: Value) -> anyhow::Result<Value> {
    let request = client()?.post(API).header("Authorization", token).json(&json!({"Function": function, "Parameters": parameters}));
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    anyhow::ensure!(status.is_success(), "{}", body["ErrorDescription"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let email = email.to_lowercase();
    let found = call(token, "GetContacts", json!({"SearchTerms": email})).await?;
    let contact_id = match matching_contact(&found, &email) {
        Some(id) => id.to_string(),
        None => {
            // A new contact must be assigned to a user, and the key's own user is the one connecting Splee.
            let user = call(token, "GetUser", json!({})).await?;
            let user_id = user["UserId"].as_str().context("Less Annoying CRM didn't return the user")?;
            let created = call(token, "CreateContact", contact(user_id, &email, lead)).await?;
            created["ContactId"].as_str().context("Less Annoying CRM didn't return the contact")?.to_string()
        }
    };
    call(token, "CreateNote", json!({"ContactId": contact_id, "Note": note})).await?;
    Ok(())
}

/// The search matches any field, so a result counts only when it's a person with exactly this address. A company can
/// have the same email, and the note belongs on the person.
fn matching_contact<'a>(found: &'a Value, email: &str) -> Option<&'a str> {
    let results = found["Results"].as_array()?;
    let has_email = |c: &Value| c["Email"].as_array().is_some_and(|e| e.iter().any(|e| e["Text"].as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case(email))));
    results.iter().find(|c| c["IsCompany"] != true && has_email(c)).and_then(|c| c["ContactId"].as_str())
}

/// A new person, named after the lead or their address. The CRM creates the company when none has that name.
fn contact(user_id: &str, email: &str, lead: &Lead) -> Value {
    let name = lead.full_name.trim();
    let mut new = json!({"IsCompany": false, "AssignedTo": user_id, "Name": if name.is_empty() { email } else { name }, "Email": email});
    if !lead.company_name.trim().is_empty() {
        new["Company Name"] = json!(lead.company_name.trim());
    }
    new
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::matching_contact;

    #[test]
    fn only_a_person_with_the_exact_address_matches() {
        let found = json!({"HasMoreResults": false, "Results": [
            {"ContactId": "1", "IsCompany": true, "Email": [{"Text": "sarah@acme.com", "Type": "Work"}]},
            {"ContactId": "2", "IsCompany": false, "Email": [{"Text": "sarah@acme.com.au", "Type": "Work"}]},
            {"ContactId": "3", "IsCompany": false, "Email": [{"Text": "other@acme.com"}, {"Text": "Sarah@Acme.com", "Type": "Work"}]},
        ]});
        assert_eq!(matching_contact(&found, "sarah@acme.com"), Some("3"));
        assert_eq!(matching_contact(&found, "nobody@acme.com"), None);
        assert_eq!(matching_contact(&json!({}), "sarah@acme.com"), None);
    }
}
