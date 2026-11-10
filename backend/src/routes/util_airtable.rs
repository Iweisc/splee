//! Reads an Airtable table with a personal access token, so leads import straight from it like a Google Sheet.
//! Only the base, table and view ids are taken from the link, so the request always goes to Airtable, never a host
//! someone typed. The token is used for this request only and never saved.

use std::time::Duration;

use reqwest::Url;
use serde_json::Value;

use crate::error::{AppError, AppResult};

const API: &str = "https://api.airtable.com/v0";

/// Airtable sends 100 records a page. 100 pages is 10,000 leads, more than an import should take in one go.
const MAX_PAGES: usize = 100;

/// The table's name, its columns and its rows, each with the row number it would have in a CSV export.
pub struct Table {
    pub name: String,
    pub headers: Vec<String>,
    pub rows: Vec<(usize, Vec<String>)>,
}

pub async fn fetch(link: &str, token: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let ids = Ids::parse(link).ok_or_else(|| bad("Paste the link to an Airtable table, like https://airtable.com/app…/tbl…"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(bad("Paste an Airtable personal access token"));
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
    let (name, headers) = schema(&client, &ids, token).await.unwrap_or_else(|| ("Airtable table".into(), Vec::new()));
    let mut records = Vec::new();
    let mut offset: Option<String> = None;
    for page in 0.. {
        if page == MAX_PAGES {
            return Err(bad("The table has over 10,000 rows. Filter a view down and paste that view's link."));
        }
        let mut url = Url::parse(&format!("{API}/{}/{}", ids.base, ids.table)).map_err(|e| AppError::Internal(e.into()))?;
        url.query_pairs_mut().append_pair("pageSize", "100");
        if let Some(view) = &ids.view {
            url.query_pairs_mut().append_pair("view", view);
        }
        if let Some(offset) = &offset {
            url.query_pairs_mut().append_pair("offset", offset);
        }
        let body = get(&client, url, token).await?;
        records.extend(body["records"].as_array().into_iter().flatten().map(|r| r["fields"].clone()));
        match body["offset"].as_str() {
            Some(next) => offset = Some(next.to_string()),
            None => break,
        }
    }
    let headers = columns(headers, &records);
    let rows = records.iter().enumerate().map(|(i, fields)| (i + 2, headers.iter().map(|h| cell(&fields[h])).collect())).collect();
    Ok(Table { name, headers, rows })
}

/// The schema's columns, then any others in name order. Airtable leaves an empty cell out of its record, so the others are
/// gathered from every record.
fn columns(mut headers: Vec<String>, records: &[Value]) -> Vec<String> {
    let others: std::collections::BTreeSet<&String> =
        records.iter().flat_map(|fields| fields.as_object().into_iter().flatten().map(|(k, _)| k)).filter(|k| !headers.contains(k)).collect();
    headers.extend(others.into_iter().cloned());
    headers
}

struct Ids {
    base: String,
    table: String,
    view: Option<String>,
}

impl Ids {
    /// https://airtable.com/{app…}/{tbl…}/{viw…}?blocks=hide, as the address bar shows a table or one of its views.
    fn parse(link: &str) -> Option<Ids> {
        let url = Url::parse(link.trim()).ok()?;
        if !matches!(url.host_str(), Some("airtable.com" | "www.airtable.com")) {
            return None;
        }
        let segments: Vec<&str> = url.path_segments()?.collect();
        let id = |prefix: &str| segments.iter().find(|s| is_id(s, prefix)).map(|s| s.to_string());
        Some(Ids { base: id("app")?, table: id("tbl")?, view: id("viw") })
    }
}

/// Airtable's ids are a three-letter prefix and 14 letters or digits, like appXXXXXXXXXXXXXX.
fn is_id(segment: &str, prefix: &str) -> bool {
    segment.len() == 17 && segment.starts_with(prefix) && segment.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The table's name and columns in its own order. Reading them needs the token's schema.bases:read scope, so without it
/// the import goes on with a plain name.
async fn schema(client: &reqwest::Client, ids: &Ids, token: &str) -> Option<(String, Vec<String>)> {
    let url = Url::parse(&format!("{API}/meta/bases/{}/tables", ids.base)).ok()?;
    let body = get(client, url, token).await.ok()?;
    let table = body["tables"].as_array()?.iter().find(|t| t["id"] == ids.table.as_str())?;
    let fields = table["fields"].as_array()?.iter().filter_map(|f| f["name"].as_str().map(str::to_string)).collect();
    Some((table["name"].as_str()?.to_string(), fields))
}

/// A failure is `{error: {type, message}}`. A wrong token is a 401. A table the token can't reach is a 403 or 404.
async fn get(client: &reqwest::Client, url: Url, token: &str) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let res = client.get(url).bearer_auth(token).send().await.map_err(|_| bad("Couldn't reach Airtable. Try again in a moment.".into()))?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(body),
        401 => Err(bad("Airtable didn't accept that token. Copy it again from Airtable's Builder Hub.".into())),
        403 | 404 => Err(bad("That token can't read this table. Give it the data.records:read scope and access to the base.".into())),
        _ => Err(bad(format!("Airtable: {}", body["error"]["message"].as_str().unwrap_or(status.as_str())))),
    }
}

/// A cell as text. A multiple choice or linked list joins its items with commas, a collaborator is their name or email,
/// an attachment its file name, and a checked box is Yes.
fn cell(value: &Value) -> String {
    match value {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "" }.to_string(),
        Value::Array(items) => items.iter().map(cell).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        Value::Object(o) => ["name", "email", "filename", "text", "value", "url"].iter().find_map(|k| o.get(*k)).map(cell).unwrap_or_default(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Ids, cell, columns};

    #[test]
    fn only_an_airtable_table_link_is_read() {
        let ids = Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn/viwAbCdEfGhIjKlMn?blocks=hide").unwrap();
        assert_eq!((ids.base.as_str(), ids.table.as_str(), ids.view.as_deref()), ("appAbCdEfGhIjKlMn", "tblAbCdEfGhIjKlMn", Some("viwAbCdEfGhIjKlMn")));
        assert!(Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn").unwrap().view.is_none());
        assert!(Ids::parse("https://airtable.com/appAbCdEfGhIjKlMn").is_none());
        assert!(Ids::parse("https://evil.example/appAbCdEfGhIjKlMn/tblAbCdEfGhIjKlMn").is_none());
        assert!(Ids::parse("https://airtable.com/appShort/tblAbCdEfGhIjKlMn").is_none());
    }

    #[test]
    fn columns_the_schema_misses_follow_in_name_order() {
        let records = [json!({"Email": "a@x.com", "Name": "Ana"}), json!({"Company": "Delta", "Email": "b@x.com"})];
        assert_eq!(columns(Vec::new(), &records), ["Company", "Email", "Name"]);
        assert_eq!(columns(vec!["Name".into()], &records), ["Name", "Company", "Email"]);
    }

    #[test]
    fn cells_read_as_text() {
        assert_eq!(cell(&json!(" Sarah ")), "Sarah");
        assert_eq!(cell(&json!(42)), "42");
        assert_eq!(cell(&json!(true)), "Yes");
        assert_eq!(cell(&json!(["SaaS", "Fintech"])), "SaaS, Fintech");
        assert_eq!(cell(&json!({"id": "usr1", "email": "a@x.com", "name": "Ana"})), "Ana");
        assert_eq!(cell(&json!([{"id": "att1", "url": "https://dl/x.pdf", "filename": "deck.pdf"}])), "deck.pdf");
        assert_eq!(cell(&json!(null)), "");
    }
}
