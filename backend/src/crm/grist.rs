//! Grist: the lead becomes a row in the table a document's page shows, found by its Email column or added, and the note is
//! added to its Notes column, since an API key can't add a comment. The saved token is the page's link and an API key, a
//! line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::plain_markdown;
use crate::grist::Ids;
use crate::hooks;
use crate::models::Lead;

/// Checks the key can read the table and that it has an Email column and a Notes column, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    table.find("check@example.com").await.map(|_| ())
}

/// The table, with the ids of its Email, Notes and, when there is one, Name columns, since every table names its own.
struct Table {
    api: Api,
    id: String,
    columns: Columns,
}

#[derive(Debug, PartialEq)]
struct Columns {
    email: String,
    notes: String,
    /// A Markdown cell reads the note as markdown.
    rich_notes: bool,
    name: Option<String>,
}

struct Api {
    client: reqwest::Client,
    key: String,
    doc: Url,
}

impl Table {
    async fn open(token: &str) -> anyhow::Result<Table> {
        let (link, key) = token.split_once('\n').context("reconnect Grist with the page's link")?;
        let ids = Ids::parse(link).context("the page link should look like https://docs.getgrist.com/abc123/Leads/p/2")?;
        let doc = ids.api.join(&format!("docs/{}/", ids.doc))?;
        let api = Api { client: hooks::public_client(&ids.api).await?, key: key.trim().to_string(), doc };
        let found = api.call(Method::POST, "sql", Some(json!({"sql": ids.table_query()}))).await?;
        let id = found["records"][0]["fields"]["tableId"].as_str().context("that page has no table on it, so paste the link of a page with one")?;
        let columns = columns(&api.call(Method::GET, &format!("tables/{id}/columns"), None).await?)?;
        Ok(Table { api, id: id.to_string(), columns })
    }

    /// The row whose email is `email`, whatever its case. Table and column ids are Grist identifiers, letters, digits and
    /// underscores, and the address goes in as an argument.
    async fn find(&self, email: &str) -> anyhow::Result<Option<Value>> {
        let (table, Columns { email: column, notes, .. }) = (&self.id, &self.columns);
        let sql = format!("select id, \"{notes}\" as notes from \"{table}\" where lower(trim(\"{column}\")) = ? order by id limit 1");
        let found = self.api.call(Method::POST, "sql", Some(json!({"sql": sql, "args": [email]}))).await?;
        Ok(found["records"].as_array().and_then(|rows| rows.first()).map(|row| row["fields"].clone()))
    }

    fn records(&self) -> String {
        format!("tables/{}/records", self.id)
    }
}

impl Api {
    async fn call(&self, method: Method, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let url = self.doc.join(path)?;
        let host = url.host_str().unwrap_or_default().to_string();
        let mut request = self.client.request(method, url).bearer_auth(&self.key);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let res = match request.send().await {
            Err(e) if e.is_connect() => anyhow::bail!("couldn't reach {host}, so check the page link"),
            res => res.map_err(reqwest::Error::without_url)?,
        };
        let status = res.status();
        anyhow::ensure!(!status.is_redirection(), "{host} redirected, so paste the page link as your browser shows it, with https://");
        let body: Value = res.json().await.unwrap_or_default();
        match status.as_u16() {
            200..=299 => Ok(body),
            401 => anyhow::bail!("the API key was rejected"),
            403 => anyhow::bail!("the key's user can't edit this document, so give them Editor access"),
            404 => anyhow::bail!("{host} has no such document, or it isn't a Grist server"),
            _ => anyhow::bail!("{}", body["error"].as_str().map_or_else(|| status.to_string(), str::to_string)),
        }
    }
}

/// The Email, Notes and Name (or Full name) columns, by label or id, that hold text and aren't formulas.
fn columns(found: &Value) -> anyhow::Result<Columns> {
    let text: Vec<&Value> = found["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| c["fields"]["type"] == "Text" && c["fields"]["isFormula"] != true)
        .collect();
    let called = |names: &[&str]| {
        text.iter().copied().find(|c| {
            [c["fields"]["label"].as_str(), c["id"].as_str()].into_iter().flatten().any(|n| names.iter().any(|name| n.trim().eq_ignore_ascii_case(name)))
        })
    };
    let id = |c: &Value| c["id"].as_str().map(str::to_string);
    let notes = called(&["notes"]);
    // A Text column's widget options are a JSON string.
    let rich = |c: &Value| {
        let options = c["fields"]["widgetOptions"].as_str().and_then(|o| serde_json::from_str::<Value>(o).ok());
        options.is_some_and(|o| o["widget"] == "Markdown")
    };
    Ok(Columns {
        email: called(&["email"]).and_then(id).context("the table needs a text column called Email for the lead's address")?,
        notes: notes.and_then(id).context("the table needs a text column called Notes for what the lead said")?,
        rich_notes: notes.is_some_and(rich),
        name: called(&["name", "full name", "full_name"]).and_then(id),
    })
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    let email = email.to_lowercase();
    let note = if table.columns.rich_notes { plain_markdown(note) } else { note.to_string() };
    let Columns { email: email_column, notes, name, .. } = &table.columns;
    match table.find(&email).await? {
        Some(row) => {
            let before = row["notes"].as_str().unwrap_or_default().trim_end();
            let after = if before.is_empty() { note } else { format!("{before}\n\n{note}") };
            let fields = serde_json::Map::from_iter([(notes.clone(), json!(after))]);
            table.api.call(Method::PATCH, &table.records(), Some(json!({"records": [{"id": row["id"], "fields": fields}]}))).await?;
        }
        None => {
            let mut fields = serde_json::Map::new();
            if let Some(name) = name {
                let full_name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
                fields.insert(name.clone(), json!(full_name));
            }
            fields.insert(email_column.clone(), json!(email));
            fields.insert(notes.clone(), json!(note));
            table.api.call(Method::POST, &table.records(), Some(json!({"records": [{"fields": fields}]}))).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Columns, columns};

    #[test]
    fn the_email_notes_and_name_columns_are_found_by_label_or_id() {
        let found = json!({"columns": [
            {"id": "Full_name", "fields": {"label": "Full name", "type": "Text", "isFormula": false}},
            {"id": "E_mail", "fields": {"label": "Email", "type": "Text", "isFormula": false}},
            {"id": "Notes", "fields": {"label": "", "type": "Text", "isFormula": false, "widgetOptions": "{\"widget\":\"Markdown\"}"}},
        ]});
        let expected = Columns { email: "E_mail".into(), notes: "Notes".into(), rich_notes: true, name: Some("Full_name".into()) };
        assert_eq!(columns(&found).unwrap(), expected);
        let formula = json!({"columns": [
            {"id": "Email", "fields": {"label": "Email", "type": "Text", "isFormula": false}},
            {"id": "Notes", "fields": {"label": "Notes", "type": "Text", "isFormula": true}},
        ]});
        assert!(columns(&formula).unwrap_err().to_string().contains("Notes"));
    }
}
