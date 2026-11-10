//! Airtable: the lead becomes a record in a table, found by its email field or created, and the note goes on it as a
//! comment. The saved token is the table's link and a personal access token, a line apart (see Crm::account).

use anyhow::Context;
use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::client;
use crate::airtable::{API, Ids};
use crate::models::Lead;

/// Checks the token can read the table's fields and that one holds an email, before it's saved.
pub async fn check(token: &str) -> anyhow::Result<()> {
    Table::open(token).await.map(|_| ())
}

/// The table, with the names of its email field and, when it takes text, its primary field, which every table names its
/// own way.
struct Table<'a> {
    client: reqwest::Client,
    key: &'a str,
    ids: Ids,
    email: String,
    name: Option<String>,
}

impl<'a> Table<'a> {
    async fn open(token: &'a str) -> anyhow::Result<Table<'a>> {
        let (link, key) = token.split_once('\n').context("reconnect Airtable with the table's link")?;
        let ids = Ids::parse(link).context("the table link should look like https://airtable.com/app…/tbl…")?;
        let client = client()?;
        let schema = call(&client, key, Method::GET, Url::parse(&format!("{API}/meta/bases/{}/tables", ids.base))?, None).await?;
        let table = schema["tables"].as_array().into_iter().flatten().find(|t| t["id"] == ids.table.as_str()).context("the token can't see that table")?;
        let (email, name) = fields(table);
        let email = email.context("the table needs an Email field for the lead's address")?;
        Ok(Table { client, key, ids, email, name })
    }

    async fn call(&self, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
        call(&self.client, self.key, method, url, body).await
    }

    fn url(&self, path: &str) -> anyhow::Result<Url> {
        Ok(Url::parse(&format!("{API}/{}/{}{path}", self.ids.base, self.ids.table))?)
    }
}

/// The email field, the one called Email if there are several, or else a text field called Email, and the primary field
/// when it takes text, since a formula or a number can't be written.
fn fields(table: &Value) -> (Option<String>, Option<String>) {
    let fields = table["fields"].as_array().map(Vec::as_slice).unwrap_or_default();
    let emails: Vec<&Value> = fields.iter().filter(|f| f["type"] == "email").collect();
    let text = ["singleLineText", "multilineText", "richText"];
    let called_email = |f: &&Value| f["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case("email"));
    let text_email = fields.iter().filter(|f| f["type"].as_str().is_some_and(|t| text.contains(&t))).find(called_email);
    let email = emails.iter().copied().find(called_email).or(emails.first().copied()).or(text_email);
    let primary = fields.iter().find(|f| f["id"] == table["primaryFieldId"]);
    let name = primary.filter(|f| f["type"].as_str().is_some_and(|t| text.contains(&t)));
    let name_of = |f: &Value| f["name"].as_str().map(str::to_string);
    (email.and_then(name_of), name.and_then(name_of))
}

/// A failure is `{error: {type, message}}`, or `{error: "NOT_FOUND"}` for a table the token can't reach.
async fn call(client: &reqwest::Client, key: &str, method: Method, url: Url, body: Option<Value>) -> anyhow::Result<Value> {
    let mut request = client.request(method, url).bearer_auth(key);
    if let Some(body) = body {
        request = request.json(&body);
    }
    let res = request.send().await.map_err(reqwest::Error::without_url)?;
    let status = res.status();
    anyhow::ensure!(status != reqwest::StatusCode::UNAUTHORIZED, "the token was rejected");
    let body: Value = res.json().await.unwrap_or_default();
    let reachable = !matches!(status.as_u16(), 403 | 404);
    anyhow::ensure!(
        reachable,
        "the token can't reach the table, so give it the base and the data.records:read, data.records:write, \
         data.recordComments:write and schema.bases:read scopes"
    );
    anyhow::ensure!(status.is_success(), "{}", body["error"]["message"].as_str().map_or_else(|| status.to_string(), str::to_string));
    Ok(body)
}

pub async fn push(token: &str, email: &str, lead: &Lead, note: &str) -> anyhow::Result<()> {
    let table = Table::open(token).await?;
    let email = email.to_lowercase();
    let mut search = table.url("")?;
    search.query_pairs_mut().append_pair("filterByFormula", &formula(&table.email, &email)).append_pair("maxRecords", "1");
    let found = table.call(Method::GET, search, None).await?;
    let record_id = match found["records"][0]["id"].as_str() {
        Some(id) => id.to_string(),
        None => {
            let mut fields = serde_json::Map::new();
            // When the primary field is the Email field, the address goes in last and wins.
            if let Some(name) = &table.name {
                let full_name = if lead.full_name.trim().is_empty() { email.as_str() } else { lead.full_name.trim() };
                fields.insert(name.clone(), json!(full_name));
            }
            fields.insert(table.email.clone(), json!(email));
            let created = table.call(Method::POST, table.url("")?, Some(json!({"records": [{"fields": fields}]}))).await?;
            created["records"][0]["id"].as_str().context("Airtable didn't return the record")?.to_string()
        }
    };
    table.call(Method::POST, table.url(&format!("/{record_id}/comments"))?, Some(json!({"text": note}))).await?;
    Ok(())
}

/// Matches the address whatever its case in the table. The address goes in a string, so its quotes and backslashes are
/// escaped.
fn formula(field: &str, email: &str) -> String {
    format!("LOWER({{{field}}}) = '{}'", email.replace('\\', "\\\\").replace('\'', "\\'"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{fields, formula};

    #[test]
    fn the_email_and_a_writable_primary_field_are_found() {
        let table = json!({"primaryFieldId": "fld1", "fields": [
            {"id": "fld1", "name": "Lead", "type": "singleLineText"},
            {"id": "fld2", "name": "Assistant", "type": "email"},
            {"id": "fld3", "name": "Email", "type": "email"},
        ]});
        assert_eq!(fields(&table), (Some("Email".into()), Some("Lead".into())));
        let computed = json!({"primaryFieldId": "fld1", "fields": [
            {"id": "fld1", "name": "Label", "type": "formula"},
            {"id": "fld2", "name": "Work email", "type": "email"},
        ]});
        assert_eq!(fields(&computed), (Some("Work email".into()), None));
        let plain = json!({"primaryFieldId": "fld1", "fields": [{"id": "fld1", "name": "Email", "type": "singleLineText"}]});
        assert_eq!(fields(&plain), (Some("Email".into()), Some("Email".into())));
        assert_eq!(fields(&json!({"fields": [{"id": "fld1", "name": "Name", "type": "singleLineText"}]})).0, None);
    }

    #[test]
    fn the_address_is_escaped_in_the_formula() {
        assert_eq!(formula("Email", "o'neil@x.com"), "LOWER({Email}) = 'o\\'neil@x.com'");
    }
}
