//! Reads a Grist table with an API key, so leads import straight from it like a NocoDB table. Grist runs at getgrist.com
//! or on the customer's own server, and its API is at the link's origin either way, so requests only go to a public
//! address (see hooks::public_client). The key is used for this request only and never saved.

use chrono::DateTime;
use reqwest::Url;
use serde_json::Value;

use super::util_airtable::Table;
use crate::error::{AppError, AppResult};
use crate::hooks;

/// Grist sends every row at once. 10,000 is more than an import should take in one go.
const MAX_ROWS: usize = 10_000;

pub async fn fetch(link: &str, key: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let ids = Ids::parse(link).ok_or_else(|| bad("Paste the link to a page of a Grist document, like https://docs.getgrist.com/abc123/Leads/p/2"))?;
    let key = key.trim();
    if key.is_empty() {
        return Err(bad("Paste a Grist API key"));
    }
    let client = hooks::public_client(&ids.api).await.map_err(|e| AppError::BadRequest(format!("Couldn't reach Grist: {e:#}")))?;
    let doc = ids.api.join(&format!("docs/{}/", ids.doc)).map_err(internal)?;
    let url = |path: &str| doc.join(path).map_err(internal);
    // The page in the link is a view, and the table is the one its first widget shows. Without a page, it's the first page.
    // The page is a number, so it's safe in the query.
    let query = match ids.page {
        Some(page) => {
            format!("select t.tableId from _grist_Views_section s join _grist_Tables t on s.tableRef = t.id where s.parentId = {page} order by s.id limit 1")
        }
        None => "select t.tableId from _grist_Pages p join _grist_Views_section s on s.parentId = p.viewRef join _grist_Tables t on s.tableRef = t.id \
                 order by p.pagePos, s.id limit 1"
            .to_string(),
    };
    let mut sql = url("sql")?;
    sql.query_pairs_mut().append_pair("q", &query);
    let found = get(&client, sql, key).await?;
    let table = found["records"][0]["fields"]["tableId"].as_str();
    let table = table.ok_or_else(|| bad("That page has no table on it. Open a page with a table and paste its link."))?;
    let columns = columns(&get(&client, url(&format!("tables/{table}/columns"))?, key).await?);
    let mut records = url(&format!("tables/{table}/records"))?;
    records.query_pairs_mut().append_pair("limit", &(MAX_ROWS + 1).to_string());
    let records = get(&client, records, key).await?;
    let records = records["records"].as_array().cloned().unwrap_or_default();
    if records.len() > MAX_ROWS {
        return Err(bad("The table has over 10,000 rows. Split it, or filter it into a new table, and paste that page's link."));
    }
    let name = get(&client, ids.api.join(&format!("docs/{}", ids.doc)).map_err(internal)?, key).await?["name"].as_str().map(str::to_string);
    let headers = columns.iter().map(|c| c.label.clone()).collect();
    let rows = records.iter().enumerate().map(|(i, r)| (i + 2, columns.iter().map(|c| cell(&r["fields"][&c.id], &c.kind)).collect())).collect();
    let name = name.map_or_else(|| table.to_string(), |doc| format!("{doc}: {table}"));
    Ok(Table { name, headers, rows })
}

fn internal(e: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(e.into())
}

struct Column {
    id: String,
    label: String,
    kind: String,
}

/// The table's columns in its order, by their labels. A reference or an attachment is only another row's id, so it's
/// left out.
fn columns(found: &Value) -> Vec<Column> {
    let all = found["columns"].as_array().into_iter().flatten();
    all.filter_map(|c| {
        let kind = c["fields"]["type"].as_str().unwrap_or_default();
        if kind.starts_with("Ref:") || kind.starts_with("RefList:") || kind == "Attachments" {
            return None;
        }
        let id = c["id"].as_str()?.to_string();
        let label = c["fields"]["label"].as_str().filter(|l| !l.trim().is_empty()).map_or_else(|| id.clone(), str::to_string);
        Some(Column { id, label, kind: kind.to_string() })
    })
    .collect()
}

struct Ids {
    /// The API's root, like https://docs.getgrist.com/api/, or https://grist.acme.com/o/sales/api/ for a team on a
    /// self-hosted server.
    api: Url,
    doc: String,
    page: Option<u32>,
}

impl Ids {
    /// https://docs.getgrist.com/{doc}/{name}/p/{page}, as the address bar shows a document's page, with /o/{team} before
    /// it on some servers and /doc/ on older links.
    fn parse(link: &str) -> Option<Ids> {
        let link = link.trim();
        let url = Url::parse(if link.contains("://") { link.to_string() } else { format!("https://{link}") }.as_str()).ok()?;
        let mut segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
        let mut api = url.origin().ascii_serialization();
        if segments.first() == Some(&"o") && segments.len() > 1 {
            api = format!("{api}/o/{}", segments[1]);
            segments.drain(..2);
        }
        if segments.first() == Some(&"doc") {
            segments.remove(0);
        }
        let doc = segments.first().filter(|s| s.len() >= 12 && s.bytes().all(|b| b.is_ascii_alphanumeric()))?.to_string();
        let page = segments.iter().position(|s| *s == "p").and_then(|i| segments.get(i + 1)?.parse().ok());
        Some(Ids { api: Url::parse(&format!("{api}/api/")).ok()?, doc, page })
    }
}

/// A failure is `{error}`: a wrong key is a 401, and a document or table the key's user can't open a 403 or 404.
async fn get(client: &reqwest::Client, url: Url, key: &str) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let res = client.get(url).bearer_auth(key).send().await;
    let res = res.map_err(|_| bad("Couldn't reach Grist. Check the link, and try again in a moment.".into()))?;
    let status = res.status();
    if status.is_redirection() {
        return Err(bad("The server redirected, so paste the link as your browser shows it, with https://".into()));
    }
    let body: Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(body),
        401 => Err(bad("Grist didn't accept that API key. Copy it again from Profile settings → API.".into())),
        403 | 404 => Err(bad("That key can't open this document. Check the link, and that the key's user can see the document.".into())),
        _ => Err(bad(format!("Grist: {}", body["error"].as_str().unwrap_or(status.as_str())))),
    }
}

/// A cell as text. Grist sends a list as ["L", …], an error as ["E", …], dates as seconds since 1970 and a checked
/// box as true.
fn cell(value: &Value, kind: &str) -> String {
    match value {
        Value::Number(n) if kind == "Date" || kind.starts_with("DateTime") => {
            let format = if kind == "Date" { "%Y-%m-%d" } else { "%Y-%m-%d %H:%M" };
            n.as_f64().and_then(|s| DateTime::from_timestamp(s as i64, 0)).map(|d| d.format(format).to_string()).unwrap_or_default()
        }
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "" }.to_string(),
        Value::Array(items) if items.first() == Some(&Value::from("L")) => {
            items[1..].iter().map(|v| cell(v, "")).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
        }
        Value::Array(_) | Value::Object(_) | Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{Ids, cell, columns};

    #[test]
    fn a_page_link_on_getgrist_or_a_server_is_read() {
        let ids = Ids::parse("https://docs.getgrist.com/d1oetuJtF651/Sales/p/3#a1.s8.r2.c3").unwrap();
        assert_eq!((ids.api.as_str(), ids.doc.as_str(), ids.page), ("https://docs.getgrist.com/api/", "d1oetuJtF651", Some(3)));
        let team = Ids::parse("grist.acme.com/o/sales/d1oetuJtF651/Sales").unwrap();
        assert_eq!((team.api.as_str(), team.doc.as_str(), team.page), ("https://grist.acme.com/o/sales/api/", "d1oetuJtF651", None));
        let old = Ids::parse("https://acme.getgrist.com/doc/d1oetuJtF651AoLQVQVvkC/p/acl").unwrap();
        assert_eq!((old.api.as_str(), old.doc.as_str(), old.page), ("https://acme.getgrist.com/api/", "d1oetuJtF651AoLQVQVvkC", None));
        assert!(Ids::parse("https://docs.getgrist.com/").is_none());
        assert!(Ids::parse("https://docs.getgrist.com/o/docs/").is_none());
    }

    #[test]
    fn references_and_attachments_are_left_out() {
        let found = json!({"columns": [
            {"id": "Full_name", "fields": {"label": "Full name", "type": "Text"}},
            {"id": "Company", "fields": {"label": "Company", "type": "Ref:Companies"}},
            {"id": "Files", "fields": {"label": "Files", "type": "Attachments"}},
            {"id": "Email", "fields": {"label": "", "type": "Text"}},
        ]});
        let labels: Vec<String> = columns(&found).into_iter().map(|c| c.label).collect();
        assert_eq!(labels, ["Full name", "Email"]);
    }

    #[test]
    fn cells_read_as_text() {
        assert_eq!(cell(&json!(" Sarah "), "Text"), "Sarah");
        assert_eq!(cell(&json!(42), "Numeric"), "42");
        assert_eq!(cell(&json!(true), "Bool"), "Yes");
        assert_eq!(cell(&json!(1767225600), "Date"), "2026-01-01");
        assert_eq!(cell(&json!(1767229200), "DateTime:UTC"), "2026-01-01 01:00");
        assert_eq!(cell(&json!(["L", "SaaS", "Fintech"]), "ChoiceList"), "SaaS, Fintech");
        assert_eq!(cell(&json!(["E", "TypeError"]), "Any"), "");
        assert_eq!(cell(&json!(null), "Text"), "");
    }
}
