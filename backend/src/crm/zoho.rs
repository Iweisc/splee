//! Zoho CRM: the lead is upserted into Leads, matched by email, and gets a note. It signs in through crm::signin, so the token
//! is a Session, whose site is the API domain of the account's data centre.
//!
//! Docs:
//! https://www.zoho.com/crm/developer/docs/api/v8/upsert-records.html
//! https://www.zoho.com/crm/developer/docs/api/v8/create-notes.html
//! https://www.zoho.com/crm/developer/docs/api/v8/get-records.html
//! https://www.zoho.com/crm/developer/docs/api/v8/status-codes.html

use anyhow::Context;
use reqwest::header::AUTHORIZATION;
use serde_json::{Value, json};

use super::signin::Session;
use super::{client, send, split_name};
use crate::models::Lead;

pub const VERSION: &str = "v8";

fn api(s: &Session, path: &str) -> String {
    format!("{}/crm/{VERSION}/{path}", s.site)
}

/// Zoho wants its own word in front of the token, not Bearer.
fn auth(s: &Session) -> String {
    format!("Zoho-oauthtoken {}", s.access)
}

/// Checks the session can read Leads. No leads at all is a 204, which is fine.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let s = Session::parse(token)?;
    send(client()?.get(api(&s, "Leads?fields=Email&per_page=1")).header(AUTHORIZATION, auth(&s))).await?;
    Ok(())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let s = Session::parse(token)?;
    let client = client()?;
    let upsert = json!({"data": [record(&email.to_lowercase(), lead)], "duplicate_check_fields": ["Email"]});
    let upserted: Value = send(client.post(api(&s, "Leads/upsert")).header(AUTHORIZATION, auth(&s)).json(&upsert)).await?.json().await?;
    let id = saved(&upserted, "the lead")?;
    let note = json!({"data": [{"Parent_Id": {"module": {"api_name": "Leads"}, "id": id}, "Note_Content": note}]});
    let added: Value = send(client.post(api(&s, "Notes")).header(AUTHORIZATION, auth(&s)).json(&note)).await?.json().await?;
    saved(&added, "the note")?;
    Ok(())
}

/// Zoho answers with a status per record, and a 207 when some failed, so a 2xx alone doesn't mean it saved.
fn saved(answer: &Value, what: &str) -> anyhow::Result<String> {
    let first = &answer["data"][0];
    if first["status"] != "success" {
        anyhow::bail!("{}", first["message"].as_str().map_or_else(|| format!("Zoho didn't save {what}"), str::to_string));
    }
    first["details"]["id"].as_str().map(str::to_string).with_context(|| format!("Zoho didn't return {what}"))
}

/// The lead's fields. Blank ones are left out so they don't wipe what Zoho has, except Last_Name and Company, which a new lead
/// needs, so the address stands in for what Splee doesn't know.
fn record(email: &str, lead: &Lead) -> Value {
    let (first, last) = split_name(&lead.full_name);
    let (first, last) = if last.is_empty() { ("", first) } else { (first, last) };
    let (local, domain) = email.split_once('@').unwrap_or((email, email));
    let company = if lead.company_name.trim().is_empty() { lead.company_domain.as_deref().unwrap_or(domain) } else { lead.company_name.trim() };
    let fields = [
        ("Email", email),
        ("First_Name", first),
        ("Last_Name", if last.is_empty() { local } else { last }),
        ("Designation", lead.title.trim()),
        ("Company", company),
        ("Website", lead.company_domain.as_deref().unwrap_or_default()),
    ];
    fields.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), Value::String(v.to_string()))).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::twilio::tests::test_lead;

    #[test]
    fn a_failed_record_says_why() {
        let failed = json!({"data": [{"code": "MANDATORY_NOT_FOUND", "status": "error", "message": "required field not found"}]});
        assert_eq!(saved(&failed, "the lead").unwrap_err().to_string(), "required field not found");
        assert_eq!(saved(&json!({"data": [{"status": "success", "details": {"id": "41"}}]}), "the lead").unwrap(), "41");
        let lead = Lead { full_name: String::new(), ..test_lead() };
        assert_eq!(record("ana@acme.test", &lead), json!({"Email": "ana@acme.test", "Last_Name": "ana", "Company": "acme.test"}));
    }
}
