//! The Google Sheets card on the integrations page (sheets/). Signing in goes through the mailbox sign-in's start and callback
//! (routes/mailbox_oauth.rs), so the operator registers no new redirect URI, and comes back to the integrations page with
//! `sheets=connected` or `sheets_error`. Every answer that changes something is the project's Sheets as the card shows it.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::response::Redirect;
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::imports::check_segment;
use super::mailbox_oauth::{self, CallbackQuery, SHEETS, SignedIn};
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::mail::oauth::Provider;
use crate::models::{Import, Ts};
use crate::sheets::{self, Connection, SheetError, export, import};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/sheets", get(project_status).delete(disconnect))
        .route("/projects/{id}/sheets/oauth", post(start))
        .route("/projects/{id}/sheets/open", post(open))
        .route("/projects/{id}/sheets/preview", post(preview))
        .route("/projects/{id}/sheets/connections", post(create))
        .route("/projects/{id}/sheets/connections/{conn}", delete(remove).patch(change))
        .route("/projects/{id}/sheets/connections/{conn}/import", post(import_now))
}

/// The Google account as the card shows it. The tokens never leave the server.
#[derive(Debug, Serialize, FromRow)]
pub struct Account {
    pub email: String,
    pub needs_reconnect: bool,
    pub error: Option<String>,
    /// Only while it's still ahead.
    pub held_until: Option<Ts>,
}

#[derive(Debug, Serialize)]
pub struct Status {
    /// Whether this Splee has a Google app to sign in with.
    pub available: bool,
    pub account: Option<Account>,
    pub connections: Vec<Connection>,
}

async fn status(db: &PgPool, state: &AppState, project_id: Uuid) -> AppResult<Json<Status>> {
    let account = sqlx::query_as(
        "select email, needs_reconnect, error, case when held_until > now() then held_until end as held_until from sheet_accounts where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    let connections = Connection::list(db, project_id).await?;
    Ok(Json(Status { available: state.cfg.google.is_some(), account, connections }))
}

async fn project_status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Status>> {
    auth.project(&state.db, id).await?;
    status(&state.db, &state, id).await
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    let hint: Option<String> = sqlx::query_scalar("select email from sheet_accounts where project_id = $1").bind(id).fetch_optional(&state.db).await?;
    let url = mailbox_oauth::start_sign_in(&state, &auth, Provider::Google, &SHEETS, Some(id), sheets::SCOPE, hint.as_deref()).await?;
    Ok(Json(json!({ "url": url })))
}

/// Saves the project's sign-in and sends the browser back to its integrations page.
pub(super) async fn callback(state: &AppState, auth: Option<Auth>, q: CallbackQuery, project: Uuid) -> Redirect {
    let params = match save(state, auth, q).await {
        Ok(()) => vec![("sheets", "connected".to_string())],
        Err(error) => vec![("sheets_error", error)],
    };
    mailbox_oauth::redirect(&format!("/app/p/{project}/integrations"), &params)
}

async fn save(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<(), String> {
    let SignedIn { project_id, tokens, account, .. } = mailbox_oauth::signed_in(state, auth, q, &SHEETS).await?;
    let project_id = project_id.ok_or_else(|| "This sign-in expired or was already used. Connect Google Sheets again.".to_string())?;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or("Google didn't let Splee stay signed in. Connect Google Sheets again.")?;
    let secret = &state.cfg.app_secret;
    // Signing in again, as the same account or another, starts the account over: no hold and no error.
    sqlx::query(
        "insert into sheet_accounts (project_id, email, refresh_token_enc, access_token_enc, expires_at) values ($1, $2, $3, $4, $5)
         on conflict (project_id) do update set email = excluded.email, refresh_token_enc = excluded.refresh_token_enc,
           access_token_enc = excluded.access_token_enc, expires_at = excluded.expires_at, needs_reconnect = false, error = null, held_until = null,
           version = sheet_accounts.version + 1",
    )
    .bind(project_id)
    .bind(&account.email)
    .bind(crypto::encrypt(secret, refresh))
    .bind(crypto::encrypt(secret, &tokens.access_token))
    .bind(tokens.expires_at())
    .execute(&state.db)
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "saving the Sheets sign-in failed");
        "Something went wrong saving Google Sheets. Try again.".to_string()
    })?;
    Ok(())
}

/// Forgets the Google account. The tabs stay set up and wait until a Google account is connected again.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Status>> {
    auth.project(&state.db, id).await?;
    sqlx::query("delete from sheet_accounts where project_id = $1").bind(id).execute(&state.db).await?;
    status(&state.db, &state, id).await
}

fn failed(e: SheetError) -> AppError {
    match e {
        SheetError::Limited(_) => AppError::TooManyRequests(e.message()),
        e => AppError::BadRequest(e.message()),
    }
}

fn spreadsheet_id(url: &str) -> AppResult<sheets::Link> {
    sheets::link(url).ok_or_else(|| AppError::BadRequest("Paste the link to a Google Sheet, like https://docs.google.com/spreadsheets/d/…".into()))
}

#[derive(Deserialize)]
struct OpenBody {
    url: String,
}

#[derive(Serialize)]
struct Opened {
    spreadsheet_id: String,
    name: String,
    tabs: Vec<sheets::Tab>,
    /// The tab the link had open, else the first.
    tab: Option<String>,
}

/// The spreadsheet's name and tabs, to pick one from.
async fn open(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<OpenBody>) -> AppResult<Json<Opened>> {
    auth.project(&state.db, id).await?;
    let link = spreadsheet_id(&body.url)?;
    let found = sheets::authed(&state, id, |c| {
        let spreadsheet = link.id.clone();
        async move { c.spreadsheet(&spreadsheet).await }
    })
    .await
    .map_err(failed)?;
    let tab = link.gid.and_then(|gid| found.tabs.iter().find(|t| t.id == gid)).or(found.tabs.first()).map(|t| t.title.clone());
    Ok(Json(Opened { spreadsheet_id: link.id, name: found.title, tabs: found.tabs, tab }))
}

#[derive(Deserialize)]
struct PreviewBody {
    url: String,
    tab: String,
}

#[derive(Serialize)]
struct Preview {
    headers: Vec<String>,
    /// The first 5 rows under the header.
    rows: Vec<Vec<String>>,
    suggested_mapping: std::collections::BTreeMap<&'static str, String>,
}

/// The tab's columns and first rows, with a guess at which lead field each column holds, as a file import shows them.
async fn preview(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PreviewBody>) -> AppResult<Json<Preview>> {
    auth.project(&state.db, id).await?;
    let link = spreadsheet_id(&body.url)?;
    // The header and the first 5 rows are all the card shows, so that's all that's read.
    let top = sheets::range(&body.tab, "!1:6");
    let rows = sheets::authed(&state, id, |c| {
        let (spreadsheet, top) = (link.id.clone(), top.clone());
        async move { c.rows(&spreadsheet, &top).await }
    })
    .await
    .map_err(failed)?;
    let mut rows = rows.into_iter();
    let headers: Vec<String> = rows.next().unwrap_or_default().into_iter().map(|h| h.trim().to_string()).collect();
    if headers.iter().all(String::is_empty) {
        return Err(AppError::BadRequest("That tab is empty. Splee reads its first row as the column names.".into()));
    }
    let suggested_mapping = import::guess(&headers);
    Ok(Json(Preview { headers, rows: rows.collect(), suggested_mapping }))
}

#[derive(Deserialize)]
struct CreateBody {
    kind: String,
    url: String,
    tab: String,
    #[serde(default)]
    mapping: HashMap<String, String>,
    segment_id: Option<Uuid>,
    #[serde(default)]
    keep_importing: bool,
    events: Option<Vec<String>>,
}

#[derive(Serialize)]
struct Created {
    #[serde(flatten)]
    status: Status,
    /// The first read of an import tab, when it found rows.
    import: Option<Import>,
    /// Why the first read failed, while the tab stays set up.
    error: Option<String>,
}

fn events(events: Option<Vec<String>>) -> AppResult<Vec<String>> {
    let events = events.unwrap_or_else(|| export::EVENTS.iter().map(|(e, _)| e.to_string()).collect());
    if let Some(bad) = events.iter().find(|e| !export::EVENTS.iter().any(|(known, _)| known == e)) {
        return Err(AppError::BadRequest(format!("{bad} isn't an event a sheet can get. Pick hot_lead, meeting_booked or deal.won.")));
    }
    if events.is_empty() {
        return Err(AppError::BadRequest("Pick at least one event to add a row for.".into()));
    }
    Ok(events)
}

/// Sets up a tab to import from, reading it straight away, or to write lead events to.
async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<CreateBody>) -> AppResult<Json<Created>> {
    auth.project(&state.db, id).await?;
    let link = spreadsheet_id(&body.url)?;
    let tab = body.tab.trim();
    if tab.is_empty() {
        return Err(AppError::BadRequest("Pick a tab".into()));
    }
    if !matches!(body.kind.as_str(), "import" | "export") {
        return Err(AppError::BadRequest("kind is import or export".into()));
    }
    let export = body.kind == "export";
    let events = if export { events(body.events)? } else { Vec::new() };
    if !export {
        check_segment(&state, id, body.segment_id).await?;
        if body.mapping.get("email").is_none_or(|h| h.trim().is_empty()) {
            return Err(AppError::BadRequest("Pick the column that holds email addresses".into()));
        }
    }
    // Opened first, so a wrong link, a tab that isn't there or a spreadsheet the account can't open is caught now.
    let found = sheets::authed(&state, id, |c| {
        let spreadsheet = link.id.clone();
        async move { c.spreadsheet(&spreadsheet).await }
    })
    .await
    .map_err(failed)?;
    if !found.tabs.iter().any(|t| t.title == tab) {
        return Err(AppError::BadRequest(format!("{} has no tab called \"{tab}\".", found.title)));
    }
    let conn: Uuid = sqlx::query_scalar(
        "insert into sheet_connections (project_id, kind, spreadsheet_id, spreadsheet_name, tab, mapping, segment_id, keep_importing, next_import_at, events)
         values ($1, $2, $3, $4, $5, $6, $7, $8, case when $8 then now() + make_interval(mins => $9) end, $10) returning id",
    )
    .bind(id)
    .bind(&body.kind)
    .bind(&link.id)
    .bind(&found.title)
    .bind(tab)
    .bind(json!(if export { HashMap::new() } else { body.mapping }))
    .bind(if export { None } else { body.segment_id })
    .bind(!export && body.keep_importing)
    .bind(import::EVERY_MINUTES)
    .bind(&events)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match e {
        sqlx::Error::Database(db) if db.constraint() == Some("sheet_connections_export_tab") => AppError::Conflict("This tab already gets your lead events.".into()),
        e => e.into(),
    })?;
    let (import, error) = match export {
        true => (None, None),
        false => match import::run(&state, conn).await {
            Ok(import) => (import, None),
            Err(e) => (None, Some(e.message())),
        },
    };
    let Json(status) = status(&state.db, &state, id).await?;
    Ok(Json(Created { status, import, error }))
}

#[derive(Deserialize)]
struct ChangeBody {
    keep_importing: Option<bool>,
    events: Option<Vec<String>>,
}

/// Turns "Keep importing" on or off, or changes which events an export writes.
async fn change(State(state): State<AppState>, auth: Auth, Path((id, conn)): Path<(Uuid, Uuid)>, Json(body): Json<ChangeBody>) -> AppResult<Json<Status>> {
    auth.project(&state.db, id).await?;
    let found = Connection::find(&state.db, id, conn).await?.ok_or(AppError::NotFound)?;
    if let Some(on) = body.keep_importing.filter(|_| found.kind == "import") {
        sqlx::query("update sheet_connections set keep_importing = $2, next_import_at = case when $2 then now() + make_interval(mins => $3) end where id = $1")
            .bind(conn)
            .bind(on)
            .bind(import::EVERY_MINUTES)
            .execute(&state.db)
            .await?;
    }
    if body.events.is_some() && found.kind == "export" {
        sqlx::query("update sheet_connections set events = $2 where id = $1").bind(conn).bind(events(body.events)?).execute(&state.db).await?;
    }
    status(&state.db, &state, id).await
}

async fn remove(State(state): State<AppState>, auth: Auth, Path((id, conn)): Path<(Uuid, Uuid)>) -> AppResult<Json<Status>> {
    auth.project(&state.db, id).await?;
    sqlx::query("delete from sheet_connections where id = $1 and project_id = $2").bind(conn).bind(id).execute(&state.db).await?;
    status(&state.db, &state, id).await
}

#[derive(Serialize)]
pub struct ImportedNow {
    /// None when the tab had no new rows.
    pub import: Option<Import>,
    pub connection: Connection,
}

/// Reads an import tab now and adds its new rows, for the card's button, API v1 and MCP. `org_id` is the caller's, so a key or
/// session from another organization finds nothing.
pub(super) async fn import_for(state: &AppState, org_id: Uuid, project_id: Uuid, conn: Uuid) -> AppResult<ImportedNow> {
    super::api_v1::check_project(&state.db, project_id, org_id).await?;
    let found = Connection::find(&state.db, project_id, conn).await?.ok_or(AppError::NotFound)?;
    if found.kind != "import" {
        return Err(AppError::BadRequest("That sheet gets lead events. Only an import sheet can be read now.".into()));
    }
    let import = import::run(state, conn).await.map_err(failed)?;
    let connection = Connection::find(&state.db, project_id, conn).await?.ok_or(AppError::NotFound)?;
    Ok(ImportedNow { import, connection })
}

async fn import_now(State(state): State<AppState>, auth: Auth, Path((id, conn)): Path<(Uuid, Uuid)>) -> AppResult<Json<ImportedNow>> {
    Ok(Json(import_for(&state, auth.org_id, id, conn).await?))
}
