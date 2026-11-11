//! Dynamics 365 Sales: the lead updates the Lead with their email, or becomes a new one, and gets a note (an annotation) on it.
//! It signs in through crm::signin with the Microsoft app, so the token is a Session, whose site is the org.
//!
//! Docs:
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/overview
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/query/filter-rows
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/create-entity-web-api
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/update-delete-entities-using-web-api
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/compose-http-requests-handle-errors
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/reference/lead
//! https://learn.microsoft.com/en-us/power-apps/developer/data-platform/webapi/reference/annotation

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::signin::Session;
use super::{client, first_line, send, split_name};
use crate::models::Lead;

pub const VERSION: &str = "v9.2";

fn api(s: &Session, path: &str) -> String {
    format!("{}/api/data/{VERSION}/{path}", s.site)
}

fn request(client: &reqwest::Client, method: Method, url: impl reqwest::IntoUrl, s: &Session) -> reqwest::RequestBuilder {
    client.request(method, url).bearer_auth(&s.access).header("OData-MaxVersion", "4.0").header("OData-Version", "4.0").header("Accept", "application/json")
}

pub async fn check(token: &str) -> anyhow::Result<()> {
    let s = Session::parse(token)?;
    send(request(&client()?, Method::GET, api(&s, "WhoAmI"), &s)).await?;
    Ok(())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let s = Session::parse(token)?;
    let client = client()?;
    let email = email.to_lowercase();
    // Dataverse compares text without case.
    let filter = format!("emailaddress1 eq '{}'", odata_string(&email));
    let search = Url::parse_with_params(&api(&s, "leads"), [("$select", "leadid"), ("$filter", &filter), ("$orderby", "modifiedon desc"), ("$top", "1")])?;
    let found: Value = send(request(&client, Method::GET, search, &s)).await?.json().await?;
    let lead_id = match found["value"][0]["leadid"].as_str() {
        Some(id) => {
            // If-Match keeps a PATCH from creating the lead if it was deleted since.
            send(request(&client, Method::PATCH, api(&s, &format!("leads({id})")), &s).header("If-Match", "*").json(&fields(lead))).await?;
            id.to_string()
        }
        None => {
            let mut new = fields(lead);
            let (local, _) = email.split_once('@').unwrap_or((&email, ""));
            new.entry("lastname").or_insert_with(|| local.into());
            new.insert("emailaddress1".into(), email.as_str().into());
            new.insert("subject".into(), "Lead from Splee".into());
            let create = request(&client, Method::POST, api(&s, "leads?$select=leadid"), &s).header("Prefer", "return=representation").json(&new);
            let created: Value = send(create).await?.json().await?;
            created["leadid"].as_str().context("Dynamics didn't return the lead")?.to_string()
        }
    };
    let annotation = json!({"subject": first_line(note, 500), "notetext": note, "objectid_lead@odata.bind": format!("/leads({lead_id})")});
    send(request(&client, Method::POST, api(&s, "annotations"), &s).json(&annotation)).await?;
    Ok(())
}

/// The inside of a quoted OData string, where a quote is written twice.
fn odata_string(value: &str) -> String {
    value.replace('\'', "''")
}

/// The lead's fields. Blank ones are left out so they don't wipe what Dynamics has.
fn fields(lead: &Lead) -> serde_json::Map<String, Value> {
    let (first, last) = split_name(&lead.full_name);
    let (first, last) = if last.is_empty() { ("", first) } else { (first, last) };
    let fields = [
        ("firstname", first),
        ("lastname", last),
        ("jobtitle", lead.title.trim()),
        ("companyname", lead.company_name.trim()),
        ("websiteurl", lead.company_domain.as_deref().unwrap_or_default()),
    ];
    fields.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect()
}

#[cfg(test)]
mod tests {
    use super::odata_string;

    #[test]
    fn quotes_are_doubled() {
        assert_eq!(odata_string("o'brien+crm@acme.test"), "o''brien+crm@acme.test");
        assert_eq!(odata_string("x' or 1 eq 1 or 'a@b.test"), "x'' or 1 eq 1 or ''a@b.test");
    }
}
