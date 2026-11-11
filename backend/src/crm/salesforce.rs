//! Salesforce: the lead updates the Contact or the unconverted Lead with their email, or becomes a new Lead, and gets a Task
//! holding the note. A booked lead's Lead gets the status the project picked from the org's Lead Status picklist. It signs in
//! through crm::signin, so the token is a Session.
//!
//! Docs:
//! https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_query.htm
//! https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_sobject_basic_info.htm
//! https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_sobject_retrieve.htm
//! https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/resources_sobject_describe.htm
//! https://developer.salesforce.com/docs/atlas.en-us.api_rest.meta/api_rest/errorcodes.htm
//! https://developer.salesforce.com/docs/atlas.en-us.soql_sosl.meta/soql_sosl/sforce_api_calls_soql_select_quotedstringescapes.htm
//! https://developer.salesforce.com/docs/atlas.en-us.object_reference.meta/object_reference/sforce_api_objects_lead.htm
//! https://developer.salesforce.com/docs/atlas.en-us.object_reference.meta/object_reference/sforce_api_objects_task.htm

use anyhow::Context;
use reqwest::Url;
use serde_json::{Map, Value, json};

use super::signin::Session;
use super::{client, first_line, send, split_name};
use crate::models::Lead;

/// Spring '26. Every org has it, and a newer one changes nothing Splee uses.
pub const VERSION: &str = "v66.0";

fn api(s: &Session, path: &str) -> String {
    format!("{}/services/data/{VERSION}/{path}", s.site)
}

pub async fn check(token: &str) -> anyhow::Result<()> {
    statuses(token).await.map(|_| ())
}

/// The org's active Lead Status values, as [{value, label}].
pub async fn statuses(token: &str) -> anyhow::Result<Value> {
    let s = Session::parse(token)?;
    let described: Value = send(client()?.get(api(&s, "sobjects/Lead/describe")).bearer_auth(&s.access)).await?.json().await?;
    let fields = described["fields"].as_array().context("Salesforce didn't describe Leads")?;
    let status = fields.iter().find(|f| f["name"] == "Status").context("Leads have no Status field")?;
    let values = status["picklistValues"].as_array().map(Vec::as_slice).unwrap_or_default();
    Ok(values.iter().filter(|v| v["active"] != false).map(|v| json!({"value": v["value"], "label": v["label"]})).collect())
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let s = Session::parse(token)?;
    let client = client()?;
    let email = email.to_lowercase();
    let booked = (lead.status == "meeting_booked").then_some(s.booked_status.as_str()).filter(|status| !status.is_empty());
    let record = |object: &str, id: &str| api(&s, &format!("sobjects/{object}/{id}"));
    let who = if let Some(id) = find(&client, &s, "Contact", &email).await? {
        send(client.patch(record("Contact", &id)).bearer_auth(&s.access).json(&fields(lead, false))).await?;
        id
    } else if let Some(id) = find(&client, &s, "Lead", &email).await? {
        let mut update = fields(lead, true);
        if let Some(status) = booked {
            update.insert("Status".into(), status.into());
        }
        send(client.patch(record("Lead", &id)).bearer_auth(&s.access).json(&update)).await?;
        id
    } else {
        let created: Value = send(client.post(api(&s, "sobjects/Lead")).bearer_auth(&s.access).json(&new_lead(&email, lead, booked))).await?.json().await?;
        created["id"].as_str().context("Salesforce didn't return the lead")?.to_string()
    };
    send(client.post(api(&s, "sobjects/Task")).bearer_auth(&s.access).json(&task(&who, note))).await?;
    Ok(())
}

/// The newest Contact or unconverted Lead with the address, since a converted Lead lives on as a Contact. Over REST, SOQL takes
/// no bound values, so the address is escaped. Salesforce compares text without case.
async fn find(client: &reqwest::Client, s: &Session, object: &str, email: &str) -> anyhow::Result<Option<String>> {
    let converted = if object == "Lead" { " and IsConverted = false" } else { "" };
    let soql = format!("select Id from {object} where Email = '{}'{converted} order by LastModifiedDate desc limit 1", soql_string(email));
    let url = Url::parse_with_params(&api(s, "query"), [("q", soql)])?;
    let found: Value = send(client.get(url).bearer_auth(&s.access)).await?.json().await?;
    Ok(found["records"][0]["Id"].as_str().map(str::to_string))
}

/// The inside of a quoted SOQL string: quotes and backslashes escaped, and line breaks, which a quoted string can't hold.
fn soql_string(value: &str) -> String {
    value.chars().fold(String::new(), |mut out, c| {
        match c {
            '\\' | '\'' | '"' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
        out
    })
}

/// The lead's fields. Blank ones are left out so they don't wipe what Salesforce has. Contacts have no Company or Website.
fn fields(lead: &Lead, is_lead: bool) -> Map<String, Value> {
    let (first, last) = split_name(&lead.full_name);
    let (first, last) = if last.is_empty() { ("", first) } else { (first, last) };
    let mut fields = vec![("FirstName", first.to_string()), ("LastName", last.to_string()), ("Title", lead.title.clone())];
    if is_lead {
        fields.extend([("Company", lead.company_name.clone()), ("Website", lead.company_domain.clone().unwrap_or_default())]);
    }
    fields.into_iter().filter(|(_, v)| !v.trim().is_empty()).map(|(k, v)| (k.to_string(), Value::String(v.trim().to_string()))).collect()
}

/// A new Lead needs a last name and a company, so the address stands in for what Splee doesn't know.
fn new_lead(email: &str, lead: &Lead, booked: Option<&str>) -> Map<String, Value> {
    let mut new = fields(lead, true);
    let (local, domain) = email.split_once('@').unwrap_or((email, email));
    new.entry("LastName").or_insert_with(|| local.into());
    new.entry("Company").or_insert_with(|| lead.company_domain.clone().filter(|d| !d.trim().is_empty()).unwrap_or_else(|| domain.into()).into());
    new.insert("Email".into(), email.into());
    if let Some(status) = booked {
        new.insert("Status".into(), status.into());
    }
    new
}

/// The note as a Task on today's date. Its status is the org's default, so it shows up as a follow-up.
fn task(who: &str, note: &str) -> Value {
    let description: String = note.chars().take(32_000).collect();
    json!({"WhoId": who, "Subject": first_line(note, 255), "Description": description, "ActivityDate": chrono::Utc::now().date_naive().to_string()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::twilio::tests::test_lead;

    #[test]
    fn awkward_addresses_stay_inside_the_soql_string() {
        assert_eq!(soql_string("o'brien+crm@acme.test"), "o\\'brien+crm@acme.test");
        assert_eq!(soql_string("a\\' or Id != '@x.test"), "a\\\\\\' or Id != \\'@x.test");
        assert_eq!(soql_string("\"q\"\n@x.test"), "\\\"q\\\"\\n@x.test");
    }

    #[test]
    fn a_new_lead_has_what_salesforce_requires() {
        let lead = Lead { full_name: "Cher".into(), ..test_lead() };
        let new = new_lead("cher@acme.test", &lead, Some("Working - Contacted"));
        assert_eq!(Value::Object(new), json!({"LastName": "Cher", "Company": "acme.test", "Email": "cher@acme.test", "Status": "Working - Contacted"}));
        let lead = Lead { full_name: "Sarah van Dijk".into(), title: "CTO".into(), company_name: "Acme".into(), company_domain: Some("acme.test".into()), ..test_lead() };
        let contact = Value::Object(fields(&lead, false));
        assert_eq!(contact, json!({"FirstName": "Sarah", "LastName": "van Dijk", "Title": "CTO"}));
        assert_eq!(Value::Object(new_lead("sarah@acme.test", &lead, None))["Website"], "acme.test");
    }
}
