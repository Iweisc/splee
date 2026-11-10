//! NocoDB: the lead becomes a row in a table, found by its email column or created, and the note is added to its Notes
//! column, since NocoDB rows have no comments an API token can add. The saved token is the table's link and an API token,
//! a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::plain_markdown;
use crate::hooks;
use crate::models::Lead;
use crate::nocodb::{Ids, truthy};

/// Checks the token can read the table and that it has an email column and a Notes column, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    table.find("check@example.com").await.map(|_| ())
}

/// The table, with the titles of its key, email, Notes and, when it takes text, display columns, since every table names
/// its own. Rows are read and written by column title.
struct Table {
    api: Api,
    columns: Columns,
}

#[derive(Debug, PartialEq)]
struct Columns {
    key: String,
    email: String,
    notes: String,
    /// Rich text reads the note as markdown.
    rich_notes: bool,
    name: Option<String>,
}

struct Api {
    client: reqwest::Client,
    key: String,
    ids: Ids,
}

impl Table {
    async fn open(token: &str) -> anyhow::Result<Table> {
        let (link, key) = token.split_once('\n').context("reconnect NocoDB with the table's link")?;
        let ids = Ids::parse(link).context("the table link should look like https://app.nocodb.com/#/w…/p…/m…")?;
        let api = Api { client: hooks::public_client(&ids.origin).await?, key: key.trim().to_string(), ids };
        let meta = api.call(Method::GET, api.url(&format!("api/v2/meta/tables/{}", api.ids.table))?, None).await?;
        Ok(Table { columns: columns(&meta)?, api })
    }

    fn records(&self) -> anyhow::Result<Url> {
        self.api.url(&format!("api/v2/tables/{}/records", self.api.ids.table))
    }

    /// The row whose email is `email`, whatever its case. NocoDB's eq minds case and its like doesn't, so this asks for
    /// rows like the address and keeps the one that matches it.
    async fn find(&self, email: &str) -> anyhow::Result<Option<Value>> {
        let mut url = self.records()?;
        url.query_pairs_mut().append_pair("where", &format!("({},like,{email})", self.columns.email)).append_pair("limit", "20");
        let found = self.api.call(Method::GET, url, None).await?;
        let same = |row: &&Value| row[&self.columns.email].as_str().is_some_and(|e| e.trim().eq_ignore_ascii_case(email));
        Ok(found["list"].as_array().into_iter().flatten().find(same).cloned())
    }
}

impl Api {
    fn url(&self, path: &str) -> anyhow::Result<Url> {
        Ok(self.ids.origin.join(path)?)
    }

    async fn call(&self, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
        let host = url.host_str().unwrap_or_default().to_string();
        let mut request = self.client.request(method, url).header("xc-token", &self.key);
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
            401 => anyhow::bail!("the API token was rejected"),
            403 => anyhow::bail!("the token's user can't edit this table, so give them Editor access to its base"),
            404 => anyhow::bail!("{host} has no table {}, or it isn't a NocoDB server", self.ids.table),
            _ => anyhow::bail!("{}", body["message"].as_str().or(body["msg"].as_str()).map_or_else(|| status.to_string(), str::to_string)),
        }
    }
}

/// The email column, the one called Email if there are several, or else a text column called Email. Notes is a long text
/// column called Notes. The display column is only written when it takes text, since a formula or a number can't be.
fn columns(meta: &Value) -> anyhow::Result<Columns> {
    let all = meta["columns"].as_array().map(Vec::as_slice).unwrap_or_default();
    let called = |name: &'static str| move |c: &&Value| c["title"].as_str().is_some_and(|t| t.trim().eq_ignore_ascii_case(name));
    let of_type = |types: &'static [&'static str]| move |c: &&Value| c["uidt"].as_str().is_some_and(|t| types.contains(&t));
    let emails: Vec<&Value> = all.iter().filter(of_type(&["Email"])).collect();
    let text_email = all.iter().filter(of_type(&["SingleLineText", "LongText"])).find(called("email"));
    let email = emails.iter().copied().find(called("email")).or(emails.first().copied()).or(text_email);
    let notes = all.iter().filter(of_type(&["LongText"])).find(called("notes"));
    let name = all.iter().filter(|c| truthy(&c["pv"])).find(of_type(&["SingleLineText", "LongText"]));
    let key = all.iter().find(|c| truthy(&c["pk"]));
    let title = |c: &Value| c["title"].as_str().map(str::to_string);
    // Some databases behind NocoDB keep a column's meta as a JSON string.
    let rich = |c: &Value| match &c["meta"] {
        Value::String(s) => serde_json::from_str::<Value>(s).is_ok_and(|m| truthy(&m["richMode"])),
        m => truthy(&m["richMode"]),
    };
    Ok(Columns {
        key: key.and_then(title).context("the table needs a primary key to update rows by")?,
        email: email.and_then(title).context("the table needs an Email column for the lead's address")?,
        notes: notes.and_then(title).context("the table needs a long text column called Notes for what the lead said")?,
        rich_notes: notes.is_some_and(rich),
        name: name.and_then(title),
    })
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    let email = email.to_lowercase();
    let note = if table.columns.rich_notes { plain_markdown(note) } else { note.to_string() };
    let Columns { key, email: email_column, notes, name, .. } = &table.columns;
    match table.find(&email).await? {
        Some(row) => {
            let before = row[notes].as_str().unwrap_or_default().trim_end();
            let after = if before.is_empty() { note } else { format!("{before}\n\n{note}") };
            let row = serde_json::Map::from_iter([(key.clone(), row[key].clone()), (notes.clone(), json!(after))]);
            table.api.call(Method::PATCH, table.records()?, Some(json!([row]))).await?;
        }
        None => {
            let mut row = serde_json::Map::new();
            // When the display column is the email column, the address goes in last and wins.
            if let Some(name) = name {
                let full_name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
                row.insert(name.clone(), json!(full_name));
            }
            row.insert(email_column.clone(), json!(email));
            row.insert(notes.clone(), json!(note));
            table.api.call(Method::POST, table.records()?, Some(Value::Object(row))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Columns, columns};

    #[test]
    fn the_key_email_notes_and_a_writable_display_column_are_found() {
        let meta = json!({"columns": [
            {"title": "Id", "uidt": "ID", "pk": 1},
            {"title": "Lead", "uidt": "SingleLineText", "pv": true},
            {"title": "Assistant", "uidt": "Email"},
            {"title": "Email", "uidt": "Email"},
            {"title": "notes", "uidt": "LongText", "meta": "{\"richMode\":true}"},
        ]});
        let found = Columns { key: "Id".into(), email: "Email".into(), notes: "notes".into(), rich_notes: true, name: Some("Lead".into()) };
        assert_eq!(columns(&meta).unwrap(), found);
        let plain = json!({"columns": [
            {"title": "Id", "uidt": "ID", "pk": true},
            {"title": "Label", "uidt": "Formula", "pv": 1},
            {"title": "Email", "uidt": "SingleLineText"},
            {"title": "Notes", "uidt": "LongText", "meta": {"richMode": false}},
        ]});
        let found = Columns { key: "Id".into(), email: "Email".into(), notes: "Notes".into(), rich_notes: false, name: None };
        assert_eq!(columns(&plain).unwrap(), found);
        let no_notes = json!({"columns": [{"title": "Id", "uidt": "ID", "pk": 1}, {"title": "Email", "uidt": "Email"}, {"title": "Notes", "uidt": "SingleLineText"}]});
        assert!(columns(&no_notes).unwrap_err().to_string().contains("Notes"));
    }
}
