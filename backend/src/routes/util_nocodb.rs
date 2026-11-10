//! Reads a NocoDB table with an API token, so leads import straight from it like a Baserow table. The token is used for
//! this request only and never saved.

use reqwest::Url;
use serde_json::Value;

use super::util_airtable::Table;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::nocodb::{Ids, truthy};

/// NocoDB sends up to 1,000 rows a page, or fewer when the server is set to. 10,000 is more than an import should take in
/// one go.
const PAGE_SIZE: usize = 1000;
const MAX_ROWS: usize = 10_000;

pub async fn fetch(link: &str, token: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let ids = Ids::parse(link).ok_or_else(|| bad("Paste the link to a NocoDB table, like https://app.nocodb.com/#/w…/p…/m…"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(bad("Paste a NocoDB API token"));
    }
    let client = hooks::public_client(&ids.origin).await.map_err(|e| AppError::BadRequest(format!("Couldn't reach NocoDB: {e:#}")))?;
    let meta = get(&client, ids.origin.join(&format!("api/v2/meta/tables/{}", ids.table)).map_err(internal)?, token).await?;
    let columns = columns(&meta);
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let mut url = ids.origin.join(&format!("api/v2/tables/{}/records", ids.table)).map_err(internal)?;
        url.query_pairs_mut().append_pair("limit", &PAGE_SIZE.to_string()).append_pair("offset", &rows.len().to_string());
        if let Some(view) = &ids.view {
            url.query_pairs_mut().append_pair("viewId", view);
        }
        let body = get(&client, url, token).await?;
        let page = body["list"].as_array().cloned().unwrap_or_default();
        let last = page.is_empty() || body["pageInfo"]["isLastPage"] != false;
        rows.extend(page);
        if rows.len() > MAX_ROWS {
            return Err(bad("The table has over 10,000 rows. Filter a view down and paste that view's link."));
        }
        if last {
            break;
        }
    }
    let headers: Vec<String> = columns.iter().map(|c| c.name.clone()).collect();
    let rows = rows.iter().enumerate().map(|(i, row)| (i + 2, columns.iter().map(|c| cell(&row[&c.name], c.checkbox)).collect())).collect();
    let name = meta["title"].as_str().filter(|t| !t.trim().is_empty()).unwrap_or("NocoDB table").to_string();
    Ok(Table { name, headers, rows })
}

fn internal(e: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(e.into())
}

struct Column {
    name: String,
    /// A checkbox comes back as 1 or 0, or true or false.
    checkbox: bool,
}

/// The table's own columns in its order, its display column first. NocoDB's system columns, like the row id and created
/// time, and buttons are left out.
fn columns(meta: &Value) -> Vec<Column> {
    let mut columns: Vec<&Value> = meta["columns"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|c| !truthy(&c["system"]) && !matches!(c["uidt"].as_str(), Some("ID" | "Button")))
        .collect();
    columns.sort_by_key(|c| (!truthy(&c["pv"]), c["order"].as_f64().map_or(i64::MAX, |o| (o * 1000.0) as i64)));
    columns.into_iter().filter_map(|c| Some(Column { name: c["title"].as_str()?.to_string(), checkbox: c["uidt"] == "Checkbox" })).collect()
}

/// A failure is `{error, message}`: a wrong token is a 401, and a table or view that isn't there, or that the token can't
/// see, a 404 or 403.
async fn get(client: &reqwest::Client, url: Url, token: &str) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let res = client.get(url).header("xc-token", token).send().await;
    let res = res.map_err(|_| bad("Couldn't reach NocoDB. Check the link, and try again in a moment.".into()))?;
    let status = res.status();
    if status.is_redirection() {
        return Err(bad("The server redirected, so paste the link as your browser shows it, with https://".into()));
    }
    let body: Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(body),
        401 => Err(bad("NocoDB didn't accept that token. Copy it again from Account settings → Tokens.".into())),
        403 | 404 => Err(bad("That token can't see this table. Check the link, and that the token's user can open the table.".into())),
        _ => Err(bad(format!("NocoDB: {}", body["message"].as_str().or(body["msg"].as_str()).unwrap_or(status.as_str())))),
    }
}

/// A cell as text. A choice list is already "a,b", a person is their name or email, a file its title, a link to another
/// table its display value, and a checked box is Yes.
fn cell(value: &Value, checkbox: bool) -> String {
    if checkbox {
        return if truthy(value) { "Yes" } else { "" }.to_string();
    }
    match value {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "" }.to_string(),
        Value::Array(items) => items.iter().map(|v| cell(v, false)).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        Value::Object(o) => ["display_name", "title", "email", "value", "name"].iter().find_map(|k| o.get(*k)).map(|v| cell(v, false)).unwrap_or_default(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cell, columns};

    #[test]
    fn the_display_column_comes_first_and_system_ones_are_left_out() {
        let meta = json!({"columns": [
            {"title": "Id", "uidt": "ID", "order": 1},
            {"title": "CreatedAt", "uidt": "CreatedTime", "system": 1, "order": 2},
            {"title": "Email", "uidt": "Email", "order": 9},
            {"title": "Name", "uidt": "SingleLineText", "pv": true, "order": 8.5},
            {"title": "VIP", "uidt": "Checkbox", "order": 3},
            {"title": "Go", "uidt": "Button", "order": 4},
        ]});
        let names: Vec<String> = columns(&meta).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["Name", "VIP", "Email"]);
    }

    #[test]
    fn cells_read_as_text() {
        assert_eq!(cell(&json!(" Sarah "), false), "Sarah");
        assert_eq!(cell(&json!(42), false), "42");
        assert_eq!(cell(&json!(1), true), "Yes");
        assert_eq!(cell(&json!(0), true), "");
        assert_eq!(cell(&json!("SaaS,Fintech"), false), "SaaS,Fintech");
        assert_eq!(cell(&json!([{"email": "ana@acme.com", "display_name": "Ana"}]), false), "Ana");
        assert_eq!(cell(&json!([{"title": "deck.pdf", "url": "https://f/x.pdf"}]), false), "deck.pdf");
        assert_eq!(cell(&json!(null), false), "");
    }
}
