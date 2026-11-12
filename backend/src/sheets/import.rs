//! Leads from a tab. Its first row names the columns, mapped to lead fields like a file import's, and every row under it is a
//! lead. Each row read is kept by its email, or by a hash of the row when it has no valid email, so reading the tab again only
//! adds new rows: a sheet another tool fills keeps feeding the campaign. New rows go through the same checks as any import,
//! so suppressed addresses and leads the project already has are skipped, and each read that found new rows shows on the Import
//! page.

use std::collections::{BTreeMap, HashMap, HashSet};

use uuid::Uuid;

use super::{Connection, SheetError, authed, range};
use crate::crypto::sha256_hex;
use crate::models::Import;
use crate::routes::imports::suggest_field;
use crate::routes::util_input;
use crate::routes::util_lead_import::{self, NewLead};
use crate::state::AppState;

/// More than any import should take. Sheets sends the whole tab in one answer.
const MAX_ROWS: usize = 50_000;

/// How often "Keep importing" reads the tab.
pub const EVERY_MINUTES: i32 = 15;

/// The lead field each column most likely holds, from its header, as a file import guesses it. The first column wins a field.
pub fn guess(headers: &[String]) -> BTreeMap<&'static str, String> {
    let mut mapping = BTreeMap::new();
    for header in headers {
        if let Some(field) = suggest_field(header) {
            mapping.entry(field).or_insert_with(|| header.clone());
        }
    }
    mapping
}

/// What tells a row apart from the others: its email, whatever its case, or when it has none that's valid, everything in it.
/// A row whose email is filled in later becomes a new row then.
pub fn row_key(email: &str, cells: &[String]) -> String {
    match util_input::email(email) {
        Some(email) => format!("email:{email}"),
        None => {
            let cells: Vec<&str> = cells.iter().map(|c| c.trim()).collect();
            let used = cells.iter().rposition(|c| !c.is_empty()).map_or(0, |i| i + 1);
            format!("row:{}", sha256_hex(&cells[..used].join("\u{1f}")))
        }
    }
}

/// Which column each mapped field reads, by the header the mapping names, checked in the fields' order so the error names the
/// same column every time. A header that's gone is an error, since quietly importing without it would fill the campaign with
/// half-empty leads.
pub fn columns(mapping: &HashMap<String, String>, headers: &[String]) -> Result<HashMap<String, usize>, String> {
    let fields = crate::routes::imports::FIELDS;
    if let Some(field) = mapping.keys().filter(|f| !fields.contains(&f.as_str())).min() {
        return Err(format!("Splee has no lead field called {field}."));
    }
    let mut columns = HashMap::new();
    for field in fields {
        let Some(header) = mapping.get(*field).filter(|h| !h.trim().is_empty()) else { continue };
        let index = headers.iter().position(|h| h.trim() == header.trim()).ok_or_else(|| format!("The tab has no column called \"{header}\" anymore. Map its columns again."))?;
        columns.insert(field.to_string(), index);
    }
    if !columns.contains_key("email") {
        return Err("Pick the column that holds email addresses.".into());
    }
    Ok(columns)
}

/// Reads the tab and adds its new rows as leads, and keeps how it went on the connection. The Import it made, or None when
/// there were no new rows.
pub async fn run(state: &AppState, id: Uuid) -> Result<Option<Import>, SheetError> {
    let result = read_and_add(state, id).await;
    super::record(&state.db, id, &result).await?;
    result
}

async fn read_and_add(state: &AppState, id: Uuid) -> Result<Option<Import>, SheetError> {
    // Under a lock on the connection, held until the rows are kept, so "Import now" and the worker never both add the same rows.
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('sheet-import:' || $1::text, 0))").bind(id).execute(&mut *tx).await?;
    let conn: Option<Connection> = sqlx::query_as(&format!("select {} from sheet_connections where id = $1 and kind = 'import'", super::CONNECTION_COLUMNS))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let conn = conn.ok_or_else(|| SheetError::Failed("That sheet isn't connected anymore.".into()))?;
    let org_id: Uuid = sqlx::query_scalar("select org_id from projects where id = $1").bind(conn.project_id).fetch_one(&mut *tx).await?;

    let (spreadsheet, whole) = (conn.spreadsheet_id.clone(), range(&conn.tab, ""));
    let rows = authed(state, conn.project_id, |c| {
        let (spreadsheet, whole) = (spreadsheet.clone(), whole.clone());
        async move { c.rows(&spreadsheet, &whole).await }
    })
    .await?;
    let mut rows = rows.into_iter();
    let headers = rows.next().unwrap_or_default();
    if headers.iter().all(|h| h.trim().is_empty()) {
        return Err(SheetError::Failed("The tab is empty. Splee reads its first row as the column names.".into()));
    }
    if rows.len() > MAX_ROWS {
        return Err(SheetError::Failed("The tab has over 50,000 rows. Move older rows to another tab.".into()));
    }
    let mapping: HashMap<String, String> = serde_json::from_value(conn.mapping.clone()).unwrap_or_default();
    let columns = columns(&mapping, &headers).map_err(SheetError::Failed)?;

    let seen: HashSet<String> = sqlx::query_scalar::<_, String>("select key from sheet_import_rows where connection_id = $1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?
        .into_iter()
        .collect();
    let mut keys = HashSet::new();
    let mut new = Vec::new();
    for (i, cells) in rows.enumerate() {
        if cells.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let get = |field: &str| columns.get(field).and_then(|&i| cells.get(i)).map(|c| c.trim().to_string()).unwrap_or_default();
        let key = row_key(&get("email"), &cells);
        if seen.contains(&key) || !keys.insert(key) {
            continue;
        }
        let lead = NewLead {
            email: get("email"),
            full_name: get("full_name"),
            first_name: get("first_name"),
            last_name: get("last_name"),
            title: get("title"),
            company_name: get("company_name"),
            company_domain: get("company_domain"),
            linkedin_url: get("linkedin_url"),
            city: get("city"),
            country_code: get("country_code"),
            phone: get("phone"),
            segment_id: conn.segment_id,
            ..NewLead::default()
        };
        // The sheet's own row number, under the header.
        new.push((i + 2, lead));
    }
    if new.is_empty() {
        tx.commit().await?;
        return Ok(None);
    }

    let total = new.len() as i32;
    let outcome = util_lead_import::insert(&state.db, org_id, conn.project_id, "import", new).await.map_err(|e| SheetError::Failed(e.public_message()))?;
    let keys: Vec<String> = keys.into_iter().collect();
    sqlx::query("insert into sheet_import_rows (connection_id, key) select $1, unnest($2::text[]) on conflict do nothing")
        .bind(id)
        .bind(&keys)
        .execute(&mut *tx)
        .await?;
    let name = format!("Google Sheets: {} › {}", conn.spreadsheet_name, conn.tab);
    let errors = serde_json::to_value(&outcome.errors).unwrap_or_default();
    let import = sqlx::query_as::<_, Import>(
        "insert into imports (project_id, segment_id, filename, total, imported, skipped, errors) values ($1, $2, $3, $4, $5, $6, $7) returning *",
    )
    .bind(conn.project_id)
    .bind(conn.segment_id)
    .bind(name)
    .bind(total)
    .bind(outcome.imported as i32)
    .bind(outcome.skipped as i32)
    .bind(errors)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("update sheet_connections set rows_added = rows_added + $2 where id = $1").bind(id).bind(outcome.imported).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Some(import))
}

/// Reads every "Keep importing" tab that's due, and sets its next read 15 minutes on first, so two workers don't both take it.
/// Projects whose Google account is signed out or held wait.
pub async fn run_due(state: &AppState) -> sqlx::Result<()> {
    let due: Vec<Uuid> = sqlx::query_scalar(
        "update sheet_connections set next_import_at = now() + make_interval(mins => $1) where id in (
           select c.id from sheet_connections c join sheet_accounts a on a.project_id = c.project_id
           where c.kind = 'import' and c.keep_importing and c.next_import_at <= now()
             and not a.needs_reconnect and (a.held_until is null or a.held_until <= now())
           order by c.next_import_at limit 50 for update of c skip locked)
         returning id",
    )
    .bind(EVERY_MINUTES)
    .fetch_all(&state.db)
    .await?;
    for id in due {
        if let Err(e) = run(state, id).await {
            tracing::info!(connection = %id, error = e.message(), "google sheets: import failed");
        }
    }
    Ok(())
}
