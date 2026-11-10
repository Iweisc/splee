//! Reads a Teable table with a personal access token, so leads import straight from it like a NocoDB table. The token is
//! used for this request only and never saved.

use reqwest::Url;
use serde_json::Value;

use super::util_airtable::Table;
use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::teable::Ids;

/// Teable Cloud sends at most 100 records a page to newer tokens. 10,000 is more than an import should take in one go.
const PAGE_SIZE: usize = 100;
const MAX_ROWS: usize = 10_000;

pub async fn fetch(link: &str, token: &str) -> AppResult<Table> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let ids = Ids::parse(link).ok_or_else(|| bad("Paste the link to a Teable table, like https://app.teable.ai/base/bse…/table/tbl…"))?;
    let token = token.trim();
    if token.is_empty() {
        return Err(bad("Paste a Teable personal access token"));
    }
    let client = hooks::public_client(&ids.origin).await.map_err(|e| AppError::BadRequest(format!("Couldn't reach Teable: {e:#}")))?;
    let url = |path: &str| ids.origin.join(&format!("api/table/{}/{path}", ids.table)).map_err(internal);
    // With a view, its columns come in its order without the hidden ones, and its filters apply to the records.
    let mut fields = url("field")?;
    let mut records = url("record")?;
    records.query_pairs_mut().append_pair("cellFormat", "text").append_pair("fieldKeyType", "id").append_pair("take", &PAGE_SIZE.to_string());
    if let Some(view) = &ids.view {
        fields.query_pairs_mut().append_pair("viewId", view);
        records.query_pairs_mut().append_pair("viewId", view);
    }
    let columns = columns(&get(&client, fields, token).await?);
    let mut rows: Vec<Value> = Vec::new();
    loop {
        let mut page = records.clone();
        page.query_pairs_mut().append_pair("skip", &rows.len().to_string());
        let found = get(&client, page, token).await?["records"].as_array().cloned().unwrap_or_default();
        let last = found.len() < PAGE_SIZE;
        rows.extend(found);
        if rows.len() > MAX_ROWS {
            return Err(bad("The table has over 10,000 rows. Filter a view down and paste that view's link."));
        }
        if last {
            break;
        }
    }
    let table = ids.origin.join(&format!("api/base/{}/table/{}", ids.base, ids.table)).map_err(internal)?;
    let name = get(&client, table, token).await.ok();
    let headers = columns.iter().map(|c| c.name.clone()).collect();
    let rows = rows.iter().enumerate().map(|(i, r)| (i + 2, columns.iter().map(|c| cell(&r["fields"][&c.id], c.checkbox)).collect())).collect();
    let name = name.and_then(|t| t["name"].as_str().filter(|n| !n.trim().is_empty()).map(str::to_string)).unwrap_or_else(|| "Teable table".into());
    Ok(Table { name, headers, rows })
}

fn internal(e: impl Into<anyhow::Error>) -> AppError {
    AppError::Internal(e.into())
}

struct Column {
    id: String,
    name: String,
    /// A checkbox reads as "true" in text.
    checkbox: bool,
}

/// The fields as Teable lists them, which with a view is its order without the hidden ones. A button has no value.
fn columns(found: &Value) -> Vec<Column> {
    let all = found.as_array().into_iter().flatten().filter(|f| f["type"] != "button");
    all.filter_map(|f| Some(Column { id: f["id"].as_str()?.to_string(), name: f["name"].as_str()?.to_string(), checkbox: f["type"] == "checkbox" })).collect()
}

/// A failure is `{message, status, code}`: a wrong or expired token is a 401, a token without read access to the table a
/// 403, and a table that isn't there a 404.
async fn get(client: &reqwest::Client, url: Url, token: &str) -> AppResult<Value> {
    let bad = |m: String| AppError::BadRequest(m);
    let res = client.get(url).bearer_auth(token).send().await;
    let res = res.map_err(|_| bad("Couldn't reach Teable. Check the link, and try again in a moment.".into()))?;
    let status = res.status();
    if status.is_redirection() {
        return Err(bad("The server redirected, so paste the link as your browser shows it, with https://".into()));
    }
    let body: Value = res.json().await.unwrap_or_default();
    match status.as_u16() {
        200..=299 => Ok(body),
        401 => Err(bad("Teable didn't accept that token. Copy it again from Settings → Personal access tokens.".into())),
        403 => Err(bad("That token can't read this table. Give it the record and field read scopes, with access to its base.".into())),
        404 => Err(bad("Teable has no such table. Check the link.".into())),
        _ => Err(bad(format!("Teable: {}", body["message"].as_str().unwrap_or(status.as_str())))),
    }
}

/// A cell as Teable shows it in text, with a checked box as Yes.
fn cell(value: &Value, checkbox: bool) -> String {
    let text = value.as_str().map(str::trim).unwrap_or_default();
    match checkbox {
        true if text == "true" => "Yes".into(),
        true => String::new(),
        false => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{cell, columns};

    #[test]
    fn buttons_are_left_out_and_checkboxes_read_as_yes() {
        let found = json!([
            {"id": "fldA", "name": "Name", "type": "singleLineText"},
            {"id": "fldB", "name": "Go", "type": "button"},
            {"id": "fldC", "name": "VIP", "type": "checkbox"},
        ]);
        let columns = columns(&found);
        assert_eq!(columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Name", "VIP"]);
        assert!(columns[1].checkbox);
        assert_eq!(cell(&json!("true"), true), "Yes");
        assert_eq!(cell(&json!(null), true), "");
        assert_eq!(cell(&json!(" SaaS, Fintech "), false), "SaaS, Fintech");
        assert_eq!(cell(&json!(null), false), "");
    }
}
