//! Baserow: the lead becomes a row in a table, found by its email field or created, and the note is added to its Notes
//! field. Baserow's row comments are a paid feature, so the note goes in a field every plan has. The saved token is the
//! table's link and a database token, a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::plain_markdown;
use crate::baserow::Ids;
use crate::hooks;
use crate::models::Lead;

/// Checks the token can read the table and that it has an email field and a Notes field, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    // Reading the fields doesn't need the token's read permission, and finding the lead does.
    table.find("check@example.com").await.map(|_| ())
}

/// The table, with the ids of its email field, its Notes field and, when it takes text, its primary field, since every
/// table names its own.
struct Table {
    api: Api,
    fields: Fields,
}

struct Api {
    client: reqwest::Client,
    key: String,
    ids: Ids,
}

#[derive(Debug, PartialEq)]
struct Fields {
    email: u64,
    notes: u64,
    /// Rich text reads the note as markdown.
    rich_notes: bool,
    name: Option<u64>,
}

impl Table {
    async fn open(token: &str) -> anyhow::Result<Table> {
        let (link, key) = token.split_once('\n').context("reconnect Baserow with the table's link")?;
        let ids = Ids::parse(link).context("the table link should look like https://baserow.io/database/1/table/2")?;
        let api = Api { client: hooks::public_client(&ids.api).await?, key: key.trim().to_string(), ids };
        let found = api.call(Method::GET, api.url("fields")?, None).await?;
        Ok(Table { fields: fields(&found)?, api })
    }

    /// The row whose email is `email`, whatever its case. Baserow's equal filter minds case and its contains filter doesn't,
    /// so this asks for rows containing the address and keeps the one that matches it.
    async fn find(&self, email: &str) -> anyhow::Result<Option<Value>> {
        let mut url = self.api.url("rows")?;
        url.query_pairs_mut().append_pair(&format!("filter__field_{}__contains", self.fields.email), email).append_pair("size", "20");
        let found = self.api.call(Method::GET, url, None).await?;
        let column = format!("field_{}", self.fields.email);
        let same = |row: &&Value| row[&column].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email));
        Ok(found["results"].as_array().into_iter().flatten().find(same).cloned())
    }
}

impl Api {
    /// The table's `fields` or `rows` endpoint.
    fn url(&self, what: &str) -> anyhow::Result<Url> {
        Ok(self.ids.api.join(&format!("database/{what}/table/{}/", self.ids.table))?)
    }

    async fn call(&self, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut request = self.client.request(method, url).header("Authorization", format!("Token {}", self.key));
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
        match (status.as_u16(), body["error"].as_str().unwrap_or_default()) {
            (200..=299, _) => Ok(body),
            (401, "ERROR_TOKEN_DOES_NOT_EXIST") => anyhow::bail!("the database token was rejected"),
            (401, _) => anyhow::bail!("the token can't do that in this table, so give it create, read and update"),
            (404, _) => anyhow::bail!("{host} has no table {}, or it isn't a Baserow server", self.ids.table),
            _ => anyhow::bail!("{}", body["detail"].as_str().or(body["error"].as_str()).map_or_else(|| status.to_string(), str::to_string)),
        }
    }
}

/// The email field, the one called Email if there are several, or else a text field called Email. The Notes field is a long
/// text field called Notes. The primary field is only written when it takes text, since a formula or a number can't be.
fn fields(found: &Value) -> anyhow::Result<Fields> {
    let all = found.as_array().map(Vec::as_slice).unwrap_or_default();
    let called = |name: &'static str| move |f: &&Value| f["name"].as_str().is_some_and(|n| n.trim().eq_ignore_ascii_case(name));
    let of_type = |types: &'static [&'static str]| move |f: &&Value| f["type"].as_str().is_some_and(|t| types.contains(&t));
    let emails: Vec<&Value> = all.iter().filter(of_type(&["email"])).collect();
    let text_email = all.iter().filter(of_type(&["text", "long_text"])).find(called("email"));
    let email = emails.iter().copied().find(called("email")).or(emails.first().copied()).or(text_email);
    let notes = all.iter().filter(of_type(&["long_text"])).find(called("notes"));
    let name = all.iter().filter(|f| f["primary"] == true).find(of_type(&["text", "long_text"]));
    let id = |f: &Value| f["id"].as_u64();
    Ok(Fields {
        email: email.and_then(id).context("the table needs an Email field for the lead's address")?,
        notes: notes.and_then(id).context("the table needs a long text field called Notes for what the lead said")?,
        rich_notes: notes.is_some_and(|f| f["long_text_enable_rich_text"] == true),
        name: name.and_then(id),
    })
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    let email = email.to_lowercase();
    let note = if table.fields.rich_notes { plain_markdown(note) } else { note.to_string() };
    let notes = format!("field_{}", table.fields.notes);
    match table.find(&email).await? {
        Some(row) => {
            let id = row["id"].as_u64().context("Baserow didn't return the row's id")?;
            let before = row[&notes].as_str().unwrap_or_default().trim_end();
            let after = if before.is_empty() { note } else { format!("{before}\n\n{note}") };
            let row = serde_json::Map::from_iter([(notes, json!(after))]);
            let url = table.api.url("rows")?.join(&format!("{id}/"))?;
            table.api.call(Method::PATCH, url, Some(Value::Object(row))).await?;
        }
        None => {
            let mut row = serde_json::Map::new();
            // When the primary field is the email field, the address goes in last and wins.
            if let Some(name) = table.fields.name {
                let full_name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
                row.insert(format!("field_{name}"), json!(full_name));
            }
            row.insert(format!("field_{}", table.fields.email), json!(email));
            row.insert(notes, json!(note));
            table.api.call(Method::POST, table.api.url("rows")?, Some(Value::Object(row))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Fields, fields};

    #[test]
    fn the_email_notes_and_a_writable_primary_field_are_found() {
        let table = json!([
            {"id": 1, "name": "Lead", "type": "text", "primary": true},
            {"id": 2, "name": "Assistant", "type": "email", "primary": false},
            {"id": 3, "name": "Email", "type": "email", "primary": false},
            {"id": 4, "name": "notes", "type": "long_text", "primary": false, "long_text_enable_rich_text": true},
        ]);
        assert_eq!(fields(&table).unwrap(), Fields { email: 3, notes: 4, rich_notes: true, name: Some(1) });
        let plain = json!([
            {"id": 1, "name": "Label", "type": "formula", "primary": true},
            {"id": 2, "name": "Email", "type": "text", "primary": false},
            {"id": 3, "name": "Notes", "type": "long_text", "primary": false},
        ]);
        assert_eq!(fields(&plain).unwrap(), Fields { email: 2, notes: 3, rich_notes: false, name: None });
        let no_notes = json!([{"id": 1, "name": "Email", "type": "email", "primary": true}, {"id": 2, "name": "Notes", "type": "text"}]);
        assert!(fields(&no_notes).unwrap_err().to_string().contains("Notes"));
    }
}
