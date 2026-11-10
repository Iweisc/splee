//! Teable: the lead becomes a record in a table, found by its Email field or created, and the note is added to its Notes
//! field, since a personal access token can't add a comment. The saved token is the table's link and a personal access
//! token, a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use crate::hooks;
use crate::models::Lead;
use crate::teable::Ids;

/// Checks the token can read the table and that it has an Email field and a Notes field, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    table.find("check@example.com").await.map(|_| ())
}

/// The table, with the ids of its Email, Notes and, when it takes text, primary fields, since every table names its own.
/// Records are read and written by field id, so a renamed field doesn't break a sync halfway.
struct Table {
    api: Api,
    fields: Fields,
}

#[derive(Debug, PartialEq)]
struct Fields {
    email: String,
    notes: String,
    name: Option<String>,
}

struct Api {
    client: reqwest::Client,
    key: String,
    ids: Ids,
}

impl Table {
    async fn open(token: &str) -> anyhow::Result<Table> {
        let (link, key) = token.split_once('\n').context("reconnect Teable with the table's link")?;
        let ids = Ids::parse(link).context("the table link should look like https://app.teable.ai/base/bse…/table/tbl…")?;
        let api = Api { client: hooks::public_client(&ids.origin).await?, key: key.trim().to_string(), ids };
        let found = api.call(Method::GET, api.url("field")?, None).await?;
        Ok(Table { fields: fields(&found)?, api })
    }

    /// The record whose email is `email`, whatever its case. Teable's "is" minds case and its "contains" doesn't, so this
    /// asks for records containing the address and keeps the one that matches it.
    async fn find(&self, email: &str) -> anyhow::Result<Option<Value>> {
        let filter = json!({"conjunction": "and", "filterSet": [{"fieldId": self.fields.email, "operator": "contains", "value": email}]});
        let mut url = self.api.url("record")?;
        url.query_pairs_mut().append_pair("fieldKeyType", "id").append_pair("take", "20").append_pair("filter", &filter.to_string());
        let found = self.api.call(Method::GET, url, None).await?;
        let same = |record: &&Value| record["fields"][&self.fields.email].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email));
        Ok(found["records"].as_array().into_iter().flatten().find(same).cloned())
    }
}

impl Api {
    /// A path under the table, like `record` or `record/{id}`.
    fn url(&self, path: &str) -> anyhow::Result<Url> {
        Ok(self.ids.origin.join(&format!("api/table/{}/{path}", self.ids.table))?)
    }

    async fn call(&self, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut request = self.client.request(method, url).bearer_auth(&self.key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let res = match request.send().await {
            Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {host}, so check the table link"),
            res => res.map_err(reqwest::Error::without_url)?,
        };
        let status = res.status();
        anyhow::ensure!(!status.is_redirection(), "{host} redirected, so paste the table link as your browser shows it, with https://");
        let body: Value = res.json().await.unwrap_or_default();
        match status.as_u16() {
            200..=299 => Ok(body),
            401 => anyhow::bail!("the personal access token was rejected or has expired"),
            403 => anyhow::bail!("the token can't edit this table, so give it record read, create and update and field read, with access to its base"),
            404 => anyhow::bail!("{host} has no table {}, or it isn't a Teable server", self.ids.table),
            _ => anyhow::bail!("{}", body["message"].as_str().map_or_else(|| status.to_string(), str::to_string)),
        }
    }
}

/// Email is a text field called Email and Notes a long text field called Notes. The primary field is only written when it
/// takes text, since a formula or a number can't be.
fn fields(found: &Value) -> anyhow::Result<Fields> {
    let all = found.as_array().map(Vec::as_slice).unwrap_or_default();
    let text = |f: &&Value| matches!(f["type"].as_str(), Some("singleLineText" | "longText")) && f["isComputed"] != true;
    let called = |name: &str| all.iter().filter(text).find(|f| f["name"].as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(name)));
    let id = |f: &Value| f["id"].as_str().map(str::to_string);
    let notes = called("notes").filter(|f| f["type"] == "longText");
    Ok(Fields {
        email: called("email").and_then(id).context("the table needs a text field called Email for the lead's address")?,
        notes: notes.and_then(id).context("the table needs a long text field called Notes for what the lead said")?,
        name: all.iter().filter(text).find(|f| f["isPrimary"] == true).and_then(id),
    })
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    let email = email.to_lowercase();
    let Fields { email: email_field, notes, name } = &table.fields;
    match table.find(&email).await? {
        Some(record) => {
            let before = record["fields"][notes].as_str().unwrap_or_default().trim_end();
            let after = if before.is_empty() { note.to_string() } else { format!("{before}\n\n{note}") };
            let id = record["id"].as_str().context("Teable sent a record without an id")?;
            let fields = serde_json::Map::from_iter([(notes.clone(), json!(after))]);
            let body = json!({"fieldKeyType": "id", "record": {"fields": fields}});
            table.api.call(Method::PATCH, table.api.url(&format!("record/{id}"))?, Some(body)).await?;
        }
        None => {
            let mut fields = serde_json::Map::new();
            // When the primary field is the Email field, the address goes in last and wins.
            if let Some(name) = name {
                let full_name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
                fields.insert(name.clone(), json!(full_name));
            }
            fields.insert(email_field.clone(), json!(email));
            fields.insert(notes.clone(), json!(note));
            let body = json!({"fieldKeyType": "id", "records": [{"fields": fields}]});
            table.api.call(Method::POST, table.api.url("record")?, Some(body)).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Fields, fields};

    #[test]
    fn the_email_notes_and_primary_fields_are_found_by_name() {
        let found = json!([
            {"id": "fldName", "name": "Name", "type": "singleLineText", "isPrimary": true},
            {"id": "fldMail", "name": " email ", "type": "singleLineText"},
            {"id": "fldNote", "name": "Notes", "type": "longText"},
        ]);
        let expected = Fields { email: "fldMail".into(), notes: "fldNote".into(), name: Some("fldName".into()) };
        assert_eq!(fields(&found).unwrap(), expected);
        let computed = json!([
            {"id": "fldName", "name": "Name", "type": "formula", "isPrimary": true, "isComputed": true},
            {"id": "fldMail", "name": "Email", "type": "singleLineText"},
            {"id": "fldNote", "name": "Notes", "type": "singleLineText"},
        ]);
        assert!(fields(&computed).unwrap_err().to_string().contains("long text"));
    }
}
