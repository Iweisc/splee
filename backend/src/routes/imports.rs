use std::collections::HashMap;
use axum::extract::{DefaultBodyLimit, Multipart, Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
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
use super::util_table::{Csv, parse_csv, parse_spreadsheet};
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

pub(crate) const FIELDS: &[&str] = &[
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
    "language",
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

pub(crate) fn suggest_field(header: &str) -> Option<&'static str> {
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
        "language" | "lang" | "locale" => "language",
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
                language: get("language"),
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
        sqlx::query("select 1 from segments where id = $1 and project_id = $2 and archived_at is null")
            .bind(segment_id)
            .bind(project_id)
            .fetch_optional(&state.db)
            .await?
            .ok_or_else(|| AppError::BadRequest("That campaign isn't part of this project, or it's archived".into()))?;
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
    use std::sync::Arc;

    use axum::body::Body;
    use axum::extract::{FromRequest, Request};
    use serde_json::json;
    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;

    struct Seeded {
        state: AppState,
        org: Uuid,
        project: Uuid,
        segment: Uuid,
        other_org: Uuid,
        other_project: Uuid,
    }

    async fn seed(db: PgPool) -> Seeded {
        let mut ids = Vec::new();
        for name in ["Acme", "Other"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
            ids.push((org, project));
        }
        let [(org, project), (other_org, other_project)] = ids[..] else { unreachable!() };
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Founders') returning id").bind(project).fetch_one(&db).await.unwrap();
        let state = AppState { db, cfg: Arc::new(Config::test()), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() };
        Seeded { state, org, project, segment, other_org, other_project }
    }

    fn owner(org_id: Uuid) -> Auth {
        Auth { user_id: Uuid::new_v4(), org_id, role: "owner".into(), token_hash: String::new() }
    }

    /// A form part: its name, the file name when it's a file, and its content.
    type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

    /// The multipart body a browser sends for `parts`.
    async fn form(parts: &[Part<'_>]) -> Multipart {
        let mut body = Vec::new();
        for (name, filename, content) in parts {
            let filename = filename.map(|f| format!("; filename=\"{f}\"")).unwrap_or_default();
            body.extend(format!("--XBOUNDARY\r\nContent-Disposition: form-data; name=\"{name}\"{filename}\r\n\r\n").as_bytes());
            body.extend(*content);
            body.extend(b"\r\n");
        }
        body.extend(b"--XBOUNDARY--\r\n");
        let request =
            Request::builder().method("POST").header("content-type", "multipart/form-data; boundary=XBOUNDARY").body(Body::from(body)).unwrap();
        Multipart::from_request(request, &()).await.unwrap()
    }

    async fn upload(s: &Seeded, org: Uuid, project: Uuid, parts: &[Part<'_>]) -> AppResult<Import> {
        import(State(s.state.clone()), owner(org), Path(project), form(parts).await).await.map(|Json(i)| i)
    }

    async fn count(s: &Seeded, table: &str) -> i64 {
        sqlx::query_scalar(&format!("select count(*) from {table}")).fetch_one(&s.state.db).await.unwrap()
    }

    const CSV: &[u8] = b"\xEF\xBB\xBFE-mail,First Name,Last name,Name,Company,Website,Mobile,Country,Email address
ada@acme.example,Ada,Lovelace,,Acme,https://www.acme.example/about,020 7946 0018,GB,
ADA@acme.example,Ada,Again,,Acme,,,,

not-an-email,Nope,,,,,,,
,Blank,,,,,,,
bob@blocked.example,Bob,,,Blocked,,,,
sam@acme.example,,,Sam Lee,Acme,,,,
";

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_preview_suggests_a_mapping_and_shows_the_first_rows(db: PgPool) {
        let s = seed(db).await;
        let preview_of = |org: Uuid, parts: Vec<Part<'static>>| {
            let state = s.state.clone();
            let project = s.project;
            async move { preview(State(state), owner(org), Path(project), form(&parts).await).await.map(|Json(p)| p) }
        };

        let shown = preview_of(s.org, vec![("file", Some("leads.csv"), CSV)]).await.unwrap();
        assert_eq!(shown.filename, "leads.csv");
        assert_eq!(shown.headers[0], "E-mail", "the byte order mark is dropped");
        let firsts: Vec<_> = shown.rows.iter().map(|r| r[0].as_str()).collect();
        assert_eq!(firsts, ["ada@acme.example", "ADA@acme.example", "not-an-email", "", "bob@blocked.example"], "the first 5 rows in order, blank lines left out");
        assert_eq!(shown.rows[0][..2], ["ada@acme.example", "Ada"]);
        let mut suggested: Vec<_> = shown.suggested_mapping.iter().map(|(f, h)| (*f, h.as_str())).collect();
        suggested.sort();
        assert_eq!(suggested, [
            ("company_domain", "Website"),
            ("company_name", "Company"),
            ("country_code", "Country"),
            ("email", "E-mail"),
            ("first_name", "First Name"),
            ("full_name", "Name"),
            ("last_name", "Last name"),
            ("phone", "Mobile"),
        ]);

        let refused = |r: AppResult<Preview>| match r {
            Err(AppError::BadRequest(m)) => m,
            other => panic!("{:?}", other.map(|p| p.filename)),
        };
        assert_eq!(refused(preview_of(s.org, vec![("segment_id", None, b"".as_slice())]).await), "Attach a CSV or Excel file");
        // A header row of empty cells.
        assert_eq!(refused(preview_of(s.org, vec![("file", Some("leads.csv"), b",,\nada@acme.example,,\n".as_slice())]).await), "The CSV has no header row");
        assert!(matches!(preview_of(s.other_org, vec![("file", Some("leads.csv"), CSV)]).await, Err(AppError::NotFound)));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_import_needs_a_file_an_email_column_and_a_campaign_of_its_project(db: PgPool) {
        let s = seed(db).await;
        let archived: Uuid = sqlx::query_scalar("insert into segments (project_id, name, archived_at) values ($1, 'Old', now()) returning id")
            .bind(s.project)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        let theirs: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Theirs') returning id")
            .bind(s.other_project)
            .fetch_one(&s.state.db)
            .await
            .unwrap();
        let (archived, theirs) = (archived.to_string(), theirs.to_string());
        let file: Part = ("file", Some("leads.csv"), CSV);
        let email: Part = ("mapping", None, br#"{"email": "E-mail"}"#.as_slice());
        let campaign = "That campaign isn't part of this project, or it's archived";
        let cases: Vec<(Vec<Part>, &str)> = vec![
            (vec![email], "Attach a CSV or Excel file"),
            (vec![file], "Map your columns first"),
            (vec![file, ("mapping", None, b"[\"email\"]".as_slice())], "mapping must be a JSON object of field to column"),
            (vec![file, ("mapping", None, br#"{"email": "E-mail", "nickname": "Name"}"#.as_slice())], "Unknown field: nickname"),
            (vec![file, ("mapping", None, br#"{"email": "Mail"}"#.as_slice())], "Column \"Mail\" is not in the file"),
            (vec![file, ("mapping", None, br#"{"first_name": "First Name"}"#.as_slice())], "Pick the column that holds email addresses"),
            (vec![file, ("mapping", None, br#"{"email": "", "first_name": "First Name"}"#.as_slice())], "Pick the column that holds email addresses"),
            (vec![file, email, ("segment_id", None, b"not-an-id".as_slice())], "segment_id is not a valid id"),
            (vec![file, email, ("segment_id", None, archived.as_bytes())], campaign),
            (vec![file, email, ("segment_id", None, theirs.as_bytes())], campaign),
        ];
        for (parts, want) in cases {
            match upload(&s, s.org, s.project, &parts).await {
                Err(AppError::BadRequest(m)) => assert_eq!(m, want),
                other => panic!("{want}: {:?}", other.map(|i| i.filename)),
            }
        }
        assert!(matches!(upload(&s, s.other_org, s.project, &[file, email]).await, Err(AppError::NotFound)));
        assert_eq!((count(&s, "leads").await, count(&s, "imports").await), (0, 0));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_import_saves_the_mapped_leads_and_records_what_it_skipped(db: PgPool) {
        let s = seed(db).await;
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.example', 'domain', 'manual')")
            .bind(s.org)
            .execute(&s.state.db)
            .await
            .unwrap();
        let segment = s.segment.to_string();
        let mapping = br#"{"email": "E-mail", "first_name": "First Name", "last_name": "Last name", "full_name": "Name", "company_name": "Company",
                           "company_domain": "Website", "country_code": "Country", "phone": "", "title": ""}"#.as_slice();
        let done = upload(&s, s.org, s.project, &[("file", Some("leads.csv"), CSV), ("mapping", None, mapping), ("segment_id", None, segment.as_bytes())])
            .await
            .unwrap();
        assert_eq!((done.filename.as_str(), done.total, done.imported, done.skipped, done.segment_id), ("leads.csv", 6, 2, 4, Some(s.segment)));
        // Rows are the file's line numbers, so the blank line 4 counts.
        assert_eq!(
            done.errors,
            json!([
                { "row": 3, "error": "Duplicate: ada@acme.example" },
                { "row": 5, "error": "Invalid email: not-an-email" },
                { "row": 6, "error": "Missing email" },
                { "row": 7, "error": "Suppressed: bob@blocked.example" },
            ])
        );
        let leads: Value = sqlx::query_scalar(
            "select json_agg(json_build_object('email', email, 'full_name', full_name, 'first_name', first_name, 'last_name', last_name,
                    'company_name', company_name, 'company_domain', company_domain, 'country_code', country_code, 'phone', phone,
                    'segment_id', segment_id, 'source', source) order by email)
             from leads where project_id = $1",
        )
        .bind(s.project)
        .fetch_one(&s.state.db)
        .await
        .unwrap();
        let lead = |email: &str, full: &str, first: &str, last: &str, domain: Option<&str>, country: &str| {
            json!({ "email": email, "full_name": full, "first_name": first, "last_name": last, "company_name": "Acme", "company_domain": domain,
                    "country_code": country, "phone": "", "segment_id": s.segment, "source": "import" })
        };
        assert_eq!(
            leads,
            json!([lead("ada@acme.example", "Ada Lovelace", "Ada", "Lovelace", Some("acme.example"), "GB"), lead("sam@acme.example", "Sam Lee", "Sam", "Lee", None, "")]),
            "a column mapped to nothing, like phone here, isn't read"
        );

        // An Excel file is read as a spreadsheet whatever the case of its extension. Both its people are new.
        let xlsx = include_bytes!("testdata/leads.xlsx").as_slice();
        let sheet = upload(&s, s.org, s.project, &[("file", Some("Leads.XLSX"), xlsx), ("mapping", None, br#"{"email": "Email", "first_name": "First name"}"#.as_slice())])
            .await
            .unwrap();
        assert_eq!((sheet.filename.as_str(), sheet.total, sheet.imported, sheet.skipped, sheet.segment_id), ("Leads.XLSX", 2, 2, 0, None));
        assert_eq!(sheet.errors, json!([]));

        // The project's imports, newest first, only for its own org.
        sqlx::query("update imports set created_at = now() - interval '1 hour' where id = $1").bind(sheet.id).execute(&s.state.db).await.unwrap();
        let Json(past) = list(State(s.state.clone()), owner(s.org), Path(s.project)).await.unwrap();
        assert_eq!(past.iter().map(|i| i.id).collect::<Vec<_>>(), [done.id, sheet.id]);
        assert!(matches!(list(State(s.state.clone()), owner(s.other_org), Path(s.project)).await, Err(AppError::NotFound)));
        let Json(none) = list(State(s.state.clone()), owner(s.other_org), Path(s.other_project)).await.unwrap();
        assert!(none.is_empty());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_hunter_import_needs_a_website_and_a_hunter_key(db: PgPool) {
        let s = seed(db).await;
        let hunter = |org: Uuid, project: Uuid, domain: &str, segment_id: Option<Uuid>| {
            let state = s.state.clone();
            let body = HunterBody { domain: domain.into(), segment_id };
            async move { import_hunter(State(state), owner(org), Path(project), Json(body)).await.map(|Json(i)| i) }
        };
        let refused = |r: AppResult<Import>| match r {
            Err(AppError::BadRequest(m)) => m,
            other => panic!("{:?}", other.map(|i| i.filename)),
        };
        assert_eq!(refused(hunter(s.org, s.project, "not a website", None).await), "Enter a website like acme.com");
        assert!(refused(hunter(s.org, s.project, "acme.example", Some(Uuid::new_v4())).await).starts_with("That campaign isn't part of this project"));
        assert_eq!(refused(hunter(s.org, s.project, "https://www.acme.example/team", Some(s.segment)).await), "Connect Hunter under Integrations → Email finder first");
        assert!(matches!(hunter(s.other_org, s.project, "acme.example", None).await, Err(AppError::NotFound)));
        assert_eq!(count(&s, "imports").await, 0);
    }

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
