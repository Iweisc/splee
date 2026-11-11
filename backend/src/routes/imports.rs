use std::collections::HashMap;
use std::io::Cursor;

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use calamine::Reader;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use super::util_airtable as airtable;
use super::util_baserow as baserow;
use super::util_grist as grist;
use super::util_teable as teable;
use super::util_nocodb as nocodb;
use super::util_input;
use super::util_sheet as sheet;
use super::util_lead_import::{self, NewLead};
use super::util_notion as notion;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Import;
use crate::state::AppState;
use crate::verify;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/import", post(import))
        .route("/projects/{id}/imports", get(list))
        .route("/projects/{id}/import/preview", post(preview))
        .route("/projects/{id}/import/hunter", post(import_hunter))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
}

const FIELDS: &[&str] = &[
    "email",
    "full_name",
    "first_name",
    "last_name",
    "title",
    "company_name",
    "company_domain",
    "linkedin_url",
    "city",
    "country_code",
    "phone",
];

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<Import>>> {
    auth.project(&state.db, id).await?;
    let imports = sqlx::query_as::<_, Import>("select * from imports where project_id = $1 order by created_at desc")
        .bind(id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(imports))
}

#[derive(Default)]
struct Upload {
    filename: String,
    file: Option<Vec<u8>>,
    /// A Google Sheet to read instead of a file.
    sheet_url: Option<String>,
    /// An Airtable table to read instead, with a token that's used for this request only.
    airtable_url: Option<String>,
    airtable_token: Option<String>,
    /// A Notion database to read instead, with an integration's secret that's used for this request only.
    notion_url: Option<String>,
    notion_token: Option<String>,
    /// A Baserow table to read instead, with a database token that's used for this request only.
    baserow_url: Option<String>,
    baserow_token: Option<String>,
    /// A NocoDB table to read instead, with an API token that's used for this request only.
    nocodb_url: Option<String>,
    nocodb_token: Option<String>,
    /// A Grist table to read instead, with an API key that's used for this request only.
    grist_url: Option<String>,
    grist_token: Option<String>,
    /// A Teable table to read instead, with a personal access token that's used for this request only.
    teable_url: Option<String>,
    teable_token: Option<String>,
    segment_id: Option<Uuid>,
    mapping: Option<HashMap<String, String>>,
}

async fn read_upload(mut multipart: Multipart) -> AppResult<Upload> {
    let bad = |m: &str| AppError::BadRequest(m.into());
    let mut upload = Upload::default();
    while let Some(field) = multipart.next_field().await.map_err(|e| AppError::BadRequest(e.body_text()))? {
        match field.name() {
            Some("file") => {
                upload.filename = field.file_name().unwrap_or("import.csv").to_string();
                upload.file = Some(field.bytes().await.map_err(|e| AppError::BadRequest(e.body_text()))?.to_vec());
            }
            Some("sheet_url") => {
                let text = field.text().await.map_err(|e| AppError::BadRequest(e.body_text()))?;
                upload.sheet_url = Some(text.trim().to_string()).filter(|t| !t.is_empty());
            }
            Some(
                name @ ("airtable_url" | "airtable_token" | "notion_url" | "notion_token" | "baserow_url" | "baserow_token"
                | "nocodb_url" | "nocodb_token" | "grist_url" | "grist_token" | "teable_url" | "teable_token"),
            ) => {
                let slot = match name {
                    "airtable_url" => &mut upload.airtable_url,
                    "airtable_token" => &mut upload.airtable_token,
                    "notion_url" => &mut upload.notion_url,
                    "notion_token" => &mut upload.notion_token,
                    "baserow_url" => &mut upload.baserow_url,
                    "baserow_token" => &mut upload.baserow_token,
                    "nocodb_url" => &mut upload.nocodb_url,
                    "nocodb_token" => &mut upload.nocodb_token,
                    "grist_url" => &mut upload.grist_url,
                    "grist_token" => &mut upload.grist_token,
                    "teable_url" => &mut upload.teable_url,
                    _ => &mut upload.teable_token,
                };
                *slot = Some(field.text().await.map_err(|e| AppError::BadRequest(e.body_text()))?);
            }
            Some("segment_id") => {
                let text = field.text().await.map_err(|e| AppError::BadRequest(e.body_text()))?;
                if !text.trim().is_empty() {
                    upload.segment_id = Some(Uuid::parse_str(text.trim()).map_err(|_| bad("segment_id is not a valid id"))?);
                }
            }
            Some("mapping") => {
                let text = field.text().await.map_err(|e| AppError::BadRequest(e.body_text()))?;
                upload.mapping = Some(serde_json::from_str(&text).map_err(|_| bad("mapping must be a JSON object of field to column"))?);
            }
            _ => {}
        }
    }
    Ok(upload)
}

impl Upload {
    /// The uploaded file, the Google Sheet's tab read as CSV, the Airtable, Baserow, NocoDB, Grist or Teable table or the Notion
    /// database, and its name.
    async fn csv(&mut self) -> AppResult<(String, Csv)> {
        if let Some(link) = &self.teable_url {
            let table = teable::fetch(link, self.teable_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.grist_url {
            let table = grist::fetch(link, self.grist_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.nocodb_url {
            let table = nocodb::fetch(link, self.nocodb_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.baserow_url {
            let table = baserow::fetch(link, self.baserow_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.notion_url {
            let table = notion::fetch(link, self.notion_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.airtable_url {
            let table = airtable::fetch(link, self.airtable_token.as_deref().unwrap_or_default()).await?;
            return Ok((table.name, Csv { headers: table.headers, rows: table.rows }));
        }
        if let Some(link) = &self.sheet_url {
            let (name, bytes) = sheet::fetch(link).await?;
            return Ok((name, parse_csv(&bytes)?));
        }
        let file = self.file.take().ok_or_else(|| AppError::BadRequest("Attach a CSV or Excel file".into()))?;
        let name = std::mem::take(&mut self.filename);
        let lower = name.to_lowercase();
        let spreadsheet = [".xlsx", ".xlsm", ".xls", ".ods"].iter().any(|ext| lower.ends_with(ext));
        Ok((name, if spreadsheet { parse_spreadsheet(file)? } else { parse_csv(&file)? }))
    }
}

struct Csv {
    headers: Vec<String>,
    /// (line number in the file, cells)
    rows: Vec<(usize, Vec<String>)>,
}

fn parse_csv(bytes: &[u8]) -> AppResult<Csv> {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new().flexible(true).trim(csv::Trim::All).from_reader(bytes);
    let to_strings = |r: &csv::ByteRecord| r.iter().map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>();
    let invalid = |e: csv::Error| AppError::BadRequest(format!("Couldn't read the CSV: {e}"));
    let headers = to_strings(reader.byte_headers().map_err(invalid)?);
    if headers.iter().all(String::is_empty) {
        return Err(AppError::BadRequest("The CSV has no header row".into()));
    }
    let mut rows = Vec::new();
    for record in reader.byte_records() {
        let record = record.map_err(invalid)?;
        let line = record.position().map_or(0, |p| p.line() as usize);
        rows.push((line, to_strings(&record)));
    }
    Ok(Csv { headers, rows })
}

/// How much an .xlsx or .ods may unzip to. Its sheet is read into memory whole, and a small file can unzip to gigabytes.
const MAX_UNZIPPED: u64 = 200 * 1024 * 1024;

/// The first sheet of an Excel or OpenDocument file, read like a CSV: its first row is the header and line numbers are
/// the sheet's row numbers. Blank rows are skipped, as the CSV reader skips blank lines.
fn parse_spreadsheet(bytes: Vec<u8>) -> AppResult<Csv> {
    let unreadable = |e: &dyn std::fmt::Display| AppError::BadRequest(format!("Couldn't read the spreadsheet: {e}"));
    // .xls isn't a zip, and only a zip can unzip to more than it is.
    if let Ok(mut zip) = zip::ZipArchive::new(Cursor::new(&bytes)) {
        let unzipped: u64 = (0..zip.len()).filter_map(|i| zip.by_index_raw(i).ok().map(|f| f.size())).sum();
        if unzipped > MAX_UNZIPPED {
            return Err(AppError::BadRequest("The spreadsheet is too big. Save its first sheet as a CSV and import that.".into()));
        }
    }
    let mut book = calamine::open_workbook_auto_from_rs(Cursor::new(bytes)).map_err(|e| unreadable(&e))?;
    let sheet = book.worksheet_range_at(0).ok_or_else(|| AppError::BadRequest("The spreadsheet has no sheets".into()))?;
    let sheet = sheet.map_err(|e| unreadable(&e))?;
    let first_row = sheet.start().map_or(0, |(row, _)| row as usize);
    let mut rows = sheet
        .rows()
        .enumerate()
        .map(|(i, cells)| (first_row + i + 1, cells.iter().map(|c| c.to_string().trim().to_string()).collect::<Vec<_>>()))
        .filter(|(_, cells)| cells.iter().any(|c| !c.is_empty()));
    let (_, headers) = rows.next().ok_or_else(|| AppError::BadRequest("The spreadsheet has no header row".into()))?;
    Ok(Csv { headers, rows: rows.collect() })
}

fn suggest_field(header: &str) -> Option<&'static str> {
    let normalized = header.trim().to_lowercase().replace(['_', '-'], " ");
    Some(match normalized.as_str() {
        "email" | "e mail" | "email address" => "email",
        "first name" | "firstname" => "first_name",
        "last name" | "lastname" | "surname" => "last_name",
        "name" | "full name" => "full_name",
        "title" | "job title" => "title",
        "company" | "company name" => "company_name",
        "website" | "domain" | "company domain" => "company_domain",
        "linkedin" | "linkedin url" => "linkedin_url",
        "city" => "city",
        "country" | "country code" => "country_code",
        "phone" | "phone number" | "mobile" | "mobile phone" | "mobile number" | "cell" | "cell phone" | "telephone" | "tel" | "whatsapp" => "phone",
        _ => return None,
    })
}

#[derive(Serialize)]
struct Preview {
    filename: String,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    suggested_mapping: HashMap<&'static str, String>,
}

async fn preview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, multipart: Multipart) -> AppResult<Json<Preview>> {
    auth.project(&state.db, id).await?;
    let (filename, csv) = read_upload(multipart).await?.csv().await?;
    let mut suggested_mapping = HashMap::new();
    for header in &csv.headers {
        if let Some(field) = suggest_field(header) {
            suggested_mapping.entry(field).or_insert_with(|| header.clone());
        }
    }
    let rows = csv.rows.into_iter().take(5).map(|(_, cells)| cells).collect();
    Ok(Json(Preview { filename, headers: csv.headers, rows, suggested_mapping }))
}

async fn import(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, multipart: Multipart) -> AppResult<Json<Import>> {
    auth.project(&state.db, id).await?;
    let mut upload = read_upload(multipart).await?;
    let (filename, csv) = upload.csv().await?;
    let mapping = upload.mapping.ok_or_else(|| AppError::BadRequest("Map your columns first".into()))?;
    check_segment(&state, id, upload.segment_id).await?;

    let mut columns: HashMap<&str, usize> = HashMap::new();
    for (field, header) in &mapping {
        if header.is_empty() {
            continue;
        }
        let field = FIELDS.iter().find(|f| *f == field).ok_or_else(|| AppError::BadRequest(format!("Unknown field: {field}")))?;
        let index = csv
            .headers
            .iter()
            .position(|h| h == header)
            .ok_or_else(|| AppError::BadRequest(format!("Column \"{header}\" is not in the file")))?;
        columns.insert(field, index);
    }
    if !columns.contains_key("email") {
        return Err(AppError::BadRequest("Pick the column that holds email addresses".into()));
    }

    let rows: Vec<(usize, NewLead)> = csv
        .rows
        .into_iter()
        .map(|(line, cells)| {
            let get = |field: &str| columns.get(field).and_then(|&i| cells.get(i)).cloned().unwrap_or_default();
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
                segment_id: upload.segment_id,
                ..NewLead::default()
            };
            (line, lead)
        })
        .collect();
    Ok(Json(record(&state, &auth, id, upload.segment_id, &filename, rows).await?))
}

pub(super) async fn check_segment(state: &AppState, project_id: Uuid, segment_id: Option<Uuid>) -> AppResult<()> {
    if let Some(segment_id) = segment_id {
        sqlx::query("select 1 from segments where id = $1 and project_id = $2")
            .bind(segment_id)
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("That campaign isn't part of this project".into()))?;
    }
    Ok(())
}

/// Inserts the leads and keeps a row in the project's past imports.
async fn record(state: &AppState, auth: &Auth, id: Uuid, segment_id: Option<Uuid>, filename: &str, rows: Vec<(usize, NewLead)>) -> AppResult<Import> {
    let total = rows.len() as i32;
    let outcome = util_lead_import::insert(&state.db, auth.org_id, id, "import", rows).await?;
    let import = sqlx::query_as::<_, Import>(
        "insert into imports (project_id, segment_id, filename, total, imported, skipped, errors)
         values ($1, $2, $3, $4, $5, $6, $7) returning *",
    )
    .bind(id)
    .bind(segment_id)
    .bind(filename)
    .bind(total)
    .bind(outcome.imported as i32)
    .bind(outcome.skipped as i32)
    .bind(serde_json::to_value(&outcome.errors).map_err(anyhow::Error::from)?)
    .fetch_one(&state.db)
    .await?;
    Ok(import)
}

#[derive(Deserialize)]
struct HunterBody {
    domain: String,
    segment_id: Option<Uuid>,
}

/// Imports the people Hunter has found at a company, with the project's Hunter key from Email finder or Email verification.
async fn import_hunter(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<HunterBody>) -> AppResult<Json<Import>> {
    auth.project(&state.db, id).await?;
    let domain = util_input::domain(&body.domain).ok_or_else(|| AppError::BadRequest("Enter a website like acme.com".into()))?;
    check_segment(&state, id, body.segment_id).await?;
    let key = verify::hunter_key(&state, id)
        .await?
        .ok_or_else(|| AppError::BadRequest("Connect Hunter under Integrations → Email finder first".into()))?;
    let url = Url::parse_with_params("https://api.hunter.io/v2/domain-search", [("domain", domain.as_str()), ("type", "personal")])
        .map_err(anyhow::Error::from)?;
    let found = verify::hunter_get(url, &key).await.map_err(|e| AppError::BadRequest(format!("Hunter: {e:#}")))?;
    let rows = hunter_leads(&found, body.segment_id).into_iter().enumerate().map(|(i, lead)| (i + 1, lead)).collect();
    Ok(Json(record(&state, &auth, id, body.segment_id, &format!("Hunter: {domain}"), rows).await?))
}

/// The people in a Hunter domain search, leaving out addresses Hunter isn't fairly sure of (confidence 0 to 100),
/// unless it verified them.
fn hunter_leads(body: &Value, segment_id: Option<Uuid>) -> Vec<NewLead> {
    let data = &body["data"];
    let text = |v: &Value| v.as_str().unwrap_or_default().to_string();
    data["emails"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|e| e["verification"]["status"] == "valid" || e["confidence"].as_i64().unwrap_or(0) >= 80)
        .map(|e| NewLead {
            email: text(&e["value"]),
            first_name: text(&e["first_name"]),
            last_name: text(&e["last_name"]),
            title: text(&e["position"]),
            company_name: text(&data["organization"]),
            company_domain: text(&data["domain"]),
            linkedin_url: text(&e["linkedin"]),
            segment_id,
            ..NewLead::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{hunter_leads, parse_spreadsheet};

    #[test]
    fn reads_the_first_sheet_of_a_spreadsheet() {
        // A blank row 1, the header on row 2, a phone number saved as a number, a blank row 4, and a second sheet.
        let csv = parse_spreadsheet(include_bytes!("testdata/leads.xlsx").to_vec()).unwrap();
        assert_eq!(csv.headers, ["Email", "First name", "Company", "Phone"]);
        let rows: Vec<_> = csv.rows.iter().map(|(line, cells)| (*line, cells.join("|"))).collect();
        assert_eq!(rows, [(3, "ada@acme.com|Ada|Acme|15551234567".to_string()), (5, "bob@acme.com|Bob|Acme|".to_string())]);
        assert!(parse_spreadsheet(b"not a spreadsheet".to_vec()).is_err());
    }

    #[test]
    fn keeps_the_people_hunter_is_sure_of() {
        let body = json!({"data": {"domain": "acme.com", "organization": "Acme", "emails": [
            {"value": "ada@acme.com", "first_name": "Ada", "last_name": "Byron", "position": "CTO", "confidence": 95,
             "linkedin": "https://www.linkedin.com/in/ada", "verification": {"status": null}},
            {"value": "bob@acme.com", "first_name": null, "last_name": null, "position": null, "confidence": 40,
             "verification": {"status": "valid"}},
            {"value": "guess@acme.com", "confidence": 30, "verification": {"status": "accept_all"}},
        ]}});
        let leads = hunter_leads(&body, None);
        assert_eq!(leads.iter().map(|l| l.email.as_str()).collect::<Vec<_>>(), ["ada@acme.com", "bob@acme.com"]);
        assert_eq!((leads[0].title.as_str(), leads[0].company_name.as_str(), leads[0].company_domain.as_str()), ("CTO", "Acme", "acme.com"));
        assert_eq!(leads[1].first_name, "");
        assert!(hunter_leads(&json!({"data": {"emails": []}}), None).is_empty());
    }
}
