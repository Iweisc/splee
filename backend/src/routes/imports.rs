use std::collections::HashMap;

use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use uuid::Uuid;

use super::util_lead_import::{self, NewLead};
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::Import;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/import", post(import))
        .route("/projects/{id}/imports", get(list))
        .route("/projects/{id}/import/preview", post(preview))
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
        _ => return None,
    })
}

#[derive(Serialize)]
struct Preview {
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    suggested_mapping: HashMap<&'static str, String>,
}

async fn preview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, multipart: Multipart) -> AppResult<Json<Preview>> {
    auth.project(&state.db, id).await?;
    let upload = read_upload(multipart).await?;
    let csv = parse_csv(&upload.file.ok_or_else(|| AppError::BadRequest("Attach a CSV file".into()))?)?;
    let mut suggested_mapping = HashMap::new();
    for header in &csv.headers {
        if let Some(field) = suggest_field(header) {
            suggested_mapping.entry(field).or_insert_with(|| header.clone());
        }
    }
    let rows = csv.rows.into_iter().take(5).map(|(_, cells)| cells).collect();
    Ok(Json(Preview { headers: csv.headers, rows, suggested_mapping }))
}

async fn import(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, multipart: Multipart) -> AppResult<Json<Import>> {
    auth.project(&state.db, id).await?;
    let upload = read_upload(multipart).await?;
    let csv = parse_csv(&upload.file.ok_or_else(|| AppError::BadRequest("Attach a CSV file".into()))?)?;
    let mapping = upload.mapping.ok_or_else(|| AppError::BadRequest("Map your columns first".into()))?;
    if let Some(segment_id) = upload.segment_id {
        sqlx::query("select 1 from segments where id = $1 and project_id = $2")
            .bind(segment_id)
            .bind(id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("That campaign isn't part of this project".into()))?;
    }

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
                segment_id: upload.segment_id,
            };
            (line, lead)
        })
        .collect();
    let total = rows.len() as i32;
    let outcome = util_lead_import::insert(&state.db, auth.org_id, id, "import", rows).await?;

    let import = sqlx::query_as::<_, Import>(
        "insert into imports (project_id, segment_id, filename, total, imported, skipped, errors)
         values ($1, $2, $3, $4, $5, $6, $7) returning *",
    )
    .bind(id)
    .bind(upload.segment_id)
    .bind(&upload.filename)
    .bind(total)
    .bind(outcome.imported as i32)
    .bind(outcome.skipped as i32)
    .bind(serde_json::to_value(&outcome.errors).map_err(anyhow::Error::from)?)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(import))
}
