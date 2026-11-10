//! Reads a Baserow table with a database token, so leads import straight from it like an Airtable table. The token is
//! used for this request only and never saved.

use reqwest::Url;
use serde_json::Value;

use super::util_airtable::Table;
use crate::baserow::Ids;
use crate::error::{AppError, AppResult};
use crate::hooks;

/// Baserow sends up to 200 rows a page. 50 pages is 10,000 leads, more than an import should take in one go.
const PAGE_SIZE: usize = 200;
const MAX_PAGES: usize = 50;

pub async fn fetch(link: &str, token: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let ids = Ids::parse(link).ok_or_else(|| bad("Paste the link to a Baserow table, like https://baserow.io/database/1/table/2"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(bad("Paste a Baserow database token"));
    }
    let client = hooks::public_client(&ids.api).await.map_err(|e| AppError::BadRequest(format!("Couldn't reach Baserow: {e:#}")))?;
    let fields = get(&client, ids.api.join(&format!("database/fields/table/{}/", ids.table)).map_err(internal)?, token).await?;
    let mut rows = Vec::new();
    for page in 1..=MAX_PAGES {
        let mut url = ids.api.join(&format!("database/rows/table/{}/", ids.table)).map_err(internal)?;
        url.query_pairs_mut().append_pair("user_field_names", "true").append_pair("size", &PAGE_SIZE.to_string()).append_pair("page", &page.to_string());
        if let Some(view) = &ids.view {
            url.query_pairs_mut().append_pair("view_id", view);
        }
        let body = get(&client, url, token).await?;
        rows.extend(body["results"].as_array().into_iter().flatten().cloned());
        if body["next"].is_null() {
            let headers = columns(&fields);
            let rows = rows.iter().enumerate().map(|(i, row)| (i + 2, headers.iter().map(|h| cell(&row[h])).collect())).collect();
            return Ok(Table { name: "Baserow table".into(), headers, rows });
        }
    }
    Err(bad("The table has over 10,000 rows. Filter a view down and paste that view's link."))
}

fn internal(e: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(e.into())
}

/// The fields in the table's own order, its primary field first.
fn columns(fields: &Value) -> Vec<String> {
    let mut fields: Vec<&Value> = fields.as_array().into_iter().flatten().collect();
    fields.sort_by_key(|f| (f["primary"] != true, f["order"].as_i64().unwrap_or(i64::MAX), f["id"].as_i64().unwrap_or(0)));
    fields.into_iter().filter_map(|f| f["name"].as_str().map(str::to_string)).collect()
}

/// A failure is `{error, detail}`. A wrong token is a 401, and a table the token can't read a 401 or 404.
async fn get(client: &reqwest::Client, url: Url, token: &str) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let res = client.get(url).header("Authorization", format!("Token {token}")).send().await;
    let res = res.map_err(|_| bad("Couldn't reach Baserow. Check the link, and try again in a moment.".into()))?;
    let status = res.status();
    if status.is_redirection() {
        return Err(bad("The server redirected, so paste the link as your browser shows it, with https://".into()));
    }
    let body: Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(body),
        401 if body["error"] == "ERROR_TOKEN_DOES_NOT_EXIST" => Err(bad("Baserow didn't accept that token. Copy it again from Settings → Database tokens.".into())),
        401 | 404 => Err(bad("That token can't read this table. Give it read access to the table's database.".into())),
        _ => Err(bad(format!("Baserow: {}", body["detail"].as_str().or(body["error"].as_str()).unwrap_or(status.as_str())))),
    }
}

/// A cell as text. A choice or a link to another table is its value, a list of them is joined with commas, a person is
/// their name, a file its name as uploaded, and a checked box is Yes.
fn cell(value: &Value) -> String {
    match value {
        Value::String(s) => s.trim().to_string(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "" }.to_string(),
        Value::Array(items) => items.iter().map(cell).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        Value::Object(o) => ["value", "visible_name", "name"].iter().find_map(|k| o.get(*k)).map(cell).unwrap_or_default(),
        Value::Null => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cell, columns};

    #[test]
    fn the_primary_field_comes_first_then_the_tables_order() {
        let fields = json!([
            {"id": 3, "name": "Email", "order": 1, "primary": false},
            {"id": 1, "name": "Name", "order": 0, "primary": true},
            {"id": 2, "name": "Company", "order": 0, "primary": false},
        ]);
        assert_eq!(columns(&fields), ["Name", "Company", "Email"]);
    }

    #[test]
    fn cells_read_as_text() {
        assert_eq!(cell(&json!(" Sarah ")), "Sarah");
        assert_eq!(cell(&json!("42.00")), "42.00");
        assert_eq!(cell(&json!(true)), "Yes");
        assert_eq!(cell(&json!({"id": 1, "value": "SaaS", "color": "blue"})), "SaaS");
        assert_eq!(cell(&json!([{"id": 1, "value": "SaaS"}, {"id": 2, "value": "Fintech"}])), "SaaS, Fintech");
        assert_eq!(cell(&json!([{"id": 4, "name": "Ana"}])), "Ana");
        assert_eq!(cell(&json!([{"name": "x7f.pdf", "visible_name": "deck.pdf", "url": "https://f/x7f.pdf"}])), "deck.pdf");
        assert_eq!(cell(&json!(null)), "");
    }
}
