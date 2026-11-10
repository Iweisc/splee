//! Reads a Notion database with an internal integration's secret, so leads import straight from it like an Airtable table.
//! Only the database id is taken from the link, so the request always goes to Notion, never a host someone typed. The
//! secret is used for this request only and never saved.

use std::time::Duration;

use reqwest::{Method, Url};
use serde_json::{Value, json};

use super::util_airtable::Table;
use crate::error::{AppError, AppResult};

const API: &str = "https://api.notion.com/v1";
const VERSION: &str = "2026-03-11";

/// Notion sends 100 pages at a time and stops a query at 10,000, more than an import should take in one go anyway.
const MAX_PAGES: usize = 100;

pub async fn fetch(link: &str, token: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let id = database_id(link).ok_or_else(|| bad("Paste the link to a Notion database, like https://www.notion.so/Leads-1a2b…"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(bad("Paste the secret of a Notion integration that the database is shared with"));
    }
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
    let database = call(&client, Method::GET, &format!("databases/{id}"), token, None).await?;
    let name = plain_text(&database["title"]);
    let name = if name.is_empty() { "Notion database".to_string() } else { name };
    // A database usually has one source. One merged from several is read from its first.
    let source = database["data_sources"][0]["id"].as_str().ok_or_else(|| bad("That Notion database has no table to read"))?;
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    for page in 1..=MAX_PAGES {
        let mut query = json!({"page_size": 100});
        if let Some(cursor) = &cursor {
            query["start_cursor"] = json!(cursor);
        }
        let body = call(&client, Method::POST, &format!("data_sources/{source}/query"), token, Some(query)).await?;
        pages.extend(body["results"].as_array().into_iter().flatten().map(|p| p["properties"].clone()));
        if body["request_status"]["type"] == "incomplete" || (page == MAX_PAGES && body["has_more"] == true) {
            return Err(bad("The database has over 10,000 rows. Filter it into a smaller database and paste that one's link."));
        }
        match body["next_cursor"].as_str() {
            Some(next) if body["has_more"] == true => cursor = Some(next.to_string()),
            _ => break,
        }
    }
    let headers = columns(pages.first());
    let rows = pages.iter().enumerate().map(|(i, props)| (i + 2, headers.iter().map(|h| cell(&props[h])).collect())).collect();
    Ok(Table { name, headers, rows })
}

/// The id at the end of a database's link, with or without dashes: https://www.notion.so/acme/Leads-{32 hex}?v=…, as the
/// address bar shows it, or the same on notion.com or a notion.site page.
fn database_id(link: &str) -> Option<String> {
    let url = Url::parse(link.trim()).ok()?;
    let host = url.host_str()?;
    let notion = ["notion.so", "notion.com", "notion.site"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}")));
    if !notion || url.scheme() != "https" {
        return None;
    }
    let last: String = url.path_segments()?.next_back()?.chars().filter(|c| *c != '-').collect();
    let id = last.get(last.len().checked_sub(32)?..)?;
    id.bytes().all(|b| b.is_ascii_hexdigit()).then(|| id.to_lowercase())
}

/// Every page has every property, so the first page names the columns: its title first, then the rest by name.
fn columns(first: Option<&Value>) -> Vec<String> {
    let Some(props) = first.and_then(Value::as_object) else { return Vec::new() };
    let mut names: Vec<&String> = props.iter().filter(|(_, p)| p["type"] != "button").map(|(name, _)| name).collect();
    names.sort_by_key(|name| props[*name]["type"] != "title");
    names.into_iter().cloned().collect()
}

/// A failure is `{code, message}`. A wrong secret is a 401, and a database the integration wasn't added to is a 404. A
/// 429 waits as long as Notion asks, up to 10 seconds, a few times over.
async fn call(client: &reqwest::Client, method: Method, path: &str, token: &str, body: Option<Value>) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let mut attempt = 0;
    loop {
        attempt += 1;
        let mut request = client.request(method.clone(), format!("{API}/{path}")).bearer_auth(token).header("Notion-Version", VERSION);
        if let Some(body) = &body {
            request = request.json(body);
        }
        let res = request.send().await.map_err(|_| bad("Couldn't reach Notion. Try again in a moment.".into()))?;
        let status = res.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS && attempt <= 3 {
            let wait = res.headers().get("retry-after").and_then(|v| v.to_str().ok()?.parse().ok()).unwrap_or(1u64).min(10);
            tokio::time::sleep(Duration::from_secs(wait)).await;
            continue;
        }
        let body: Value = res.json().await.unwrap_or_default();
        return match status.as_u16() {
            200..=299 => Ok(body),
            401 => Err(bad("Notion didn't accept that secret. Copy it again from your integration's Configuration tab.".into())),
            403 | 404 => Err(bad("That integration can't see this database. In Notion, open the database, then ••• → Connections → add it.".into())),
            _ => Err(bad(format!("Notion: {}", body["message"].as_str().unwrap_or(status.as_str())))),
        };
    }
}

/// Rich text as plain text, like a title.
fn plain_text(items: &Value) -> String {
    items.as_array().into_iter().flatten().filter_map(|t| t["plain_text"].as_str()).collect::<String>().trim().to_string()
}

/// A property as text. Choices and lists join with commas, a person is their name or email, a file its name, a date its
/// start, and a checked box is Yes. A formula or rollup is read by the type of its result.
fn cell(prop: &Value) -> String {
    let kind = prop["type"].as_str().unwrap_or_default();
    let value = &prop[kind];
    let list = |items: &Value, each: &dyn Fn(&Value) -> String| {
        items.as_array().into_iter().flatten().map(each).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ")
    };
    let name = |v: &Value| v["name"].as_str().or(v["person"]["email"].as_str()).unwrap_or_default().trim().to_string();
    match kind {
        "title" | "rich_text" => plain_text(value),
        "select" | "status" | "place" => name(value),
        "multi_select" | "people" | "files" => list(value, &name),
        "date" => value["start"].as_str().unwrap_or_default().to_string(),
        "checkbox" => if value == true { "Yes" } else { "" }.to_string(),
        "unique_id" => match (value["prefix"].as_str(), value["number"].as_i64()) {
            (Some(prefix), Some(n)) => format!("{prefix}-{n}"),
            (None, Some(n)) => n.to_string(),
            _ => String::new(),
        },
        "rollup" if value["type"] == "array" => list(&value["array"], &cell),
        "formula" | "rollup" => cell(value),
        _ => match value {
            Value::String(s) => s.trim().to_string(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => if *b { "Yes" } else { "" }.to_string(),
            _ => String::new(),
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cell, columns, database_id};

    #[test]
    fn only_a_notion_database_link_is_read() {
        let id = "1a2b3c4d5e6f40718293a4b5c6d7e8f9";
        assert_eq!(database_id(&format!("https://www.notion.so/acme/Leads-{id}?v=0123456789abcdef0123456789abcdef")).as_deref(), Some(id));
        assert_eq!(database_id(&format!("https://www.notion.so/{}", id.to_uppercase())).as_deref(), Some(id));
        assert_eq!(database_id("https://acme.notion.site/1a2b3c4d-5e6f-4071-8293-a4b5c6d7e8f9").as_deref(), Some(id));
        assert_eq!(database_id(&format!("https://www.notion.com/acme/{id}")).as_deref(), Some(id));
        assert!(database_id(&format!("https://evil.example/{id}")).is_none());
        assert!(database_id(&format!("https://notion.so.evil.example/{id}")).is_none());
        assert!(database_id("https://www.notion.so/acme/Leads-1a2b3c").is_none());
    }

    #[test]
    fn the_title_column_comes_first() {
        let page = json!({"Email": {"type": "email"}, "Name": {"type": "title"}, "Company": {"type": "rich_text"}, "Go": {"type": "button"}});
        assert_eq!(columns(Some(&page)), ["Name", "Company", "Email"]);
        assert!(columns(None).is_empty());
    }

    #[test]
    fn properties_read_as_text() {
        let text = |s: &str| json!([{"plain_text": s}]);
        assert_eq!(cell(&json!({"type": "title", "title": [{"plain_text": "Sarah "}, {"plain_text": "Chen"}]})), "Sarah Chen");
        assert_eq!(cell(&json!({"type": "rich_text", "rich_text": text("Brightline")})), "Brightline");
        assert_eq!(cell(&json!({"type": "email", "email": "sarah@brightline.example"})), "sarah@brightline.example");
        assert_eq!(cell(&json!({"type": "email", "email": null})), "");
        assert_eq!(cell(&json!({"type": "number", "number": 42})), "42");
        assert_eq!(cell(&json!({"type": "checkbox", "checkbox": true})), "Yes");
        assert_eq!(cell(&json!({"type": "select", "select": {"name": "SaaS"}})), "SaaS");
        assert_eq!(cell(&json!({"type": "select", "select": null})), "");
        assert_eq!(cell(&json!({"type": "multi_select", "multi_select": [{"name": "SaaS"}, {"name": "Fintech"}]})), "SaaS, Fintech");
        assert_eq!(cell(&json!({"type": "people", "people": [{"id": "u1"}, {"id": "u2", "person": {"email": "a@x.com"}}]})), "a@x.com");
        assert_eq!(cell(&json!({"type": "date", "date": {"start": "2026-09-15", "end": null}})), "2026-09-15");
        assert_eq!(cell(&json!({"type": "formula", "formula": {"type": "string", "string": "Head of RevOps"}})), "Head of RevOps");
        assert_eq!(cell(&json!({"type": "rollup", "rollup": {"type": "array", "array": [{"type": "title", "title": text("Delta")}]}})), "Delta");
        assert_eq!(cell(&json!({"type": "unique_id", "unique_id": {"prefix": "LEAD", "number": 7}})), "LEAD-7");
        assert_eq!(cell(&json!({"type": "relation", "relation": [{"id": "p1"}]})), "");
    }
}
