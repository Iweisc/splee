//! Google Sheets, both ways. A project signs in to a Google account on the integrations page (routes/sheets.rs) with the Google
//! app mailboxes sign in with, asking only for Sheets, and keeps that account's tokens apart from any mailbox's. Leads import from
//! a tab once or every 15 minutes (import.rs), and hot leads, booked meetings and won deals add a row to another tab (export.rs).
//!
//! Sheets allows about 60 reads and 60 writes a minute per Google account. Each export tab gets one read and one append a tick,
//! with every row waiting for it. A request Google turns down with 429 is tried again after a short wait, and when Google keeps
//! asking, nothing calls Sheets for the project until the hold on its account passes. An append is never sent twice in a tick,
//! since an error or a timeout can come after Google wrote the rows: export.rs reads the tab on the next tick to find out.
//!
//! Docs:
//! https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets/get
//! https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets.values/get
//! https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets.values/append
//! https://developers.google.com/workspace/sheets/api/limits

pub mod export;
pub mod import;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::Utc;
use reqwest::{RequestBuilder, StatusCode, Url};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::config::Config;
use crate::crypto;
use crate::mail::oauth::{TokenError, token_request};
use crate::models::Ts;
use crate::state::AppState;

pub const API: &str = "https://sheets.googleapis.com";
/// Sheets, and the address to show who signed in. Not include_granted_scopes, so the token can't read a mailbox the same Google
/// account connected, and a mailbox's token can't touch the sheets.
pub const SCOPE: &str = "https://www.googleapis.com/auth/spreadsheets openid email";

/// What a spreadsheet link points at: the spreadsheet, and the tab that was open when there's a gid.
#[derive(Debug, PartialEq)]
pub struct Link {
    pub id: String,
    pub gid: Option<i64>,
}

/// https://docs.google.com/spreadsheets/d/{id}/edit#gid={tab}, as the address bar shows a spreadsheet. Only the id is kept, so a
/// request never goes anywhere but Google.
pub fn link(link: &str) -> Option<Link> {
    let url = Url::parse(link.trim()).ok()?;
    if url.host_str() != Some("docs.google.com") {
        return None;
    }
    let mut path = url.path_segments()?;
    let (Some("spreadsheets"), Some("d"), Some(id)) = (path.next(), path.next(), path.next()) else { return None };
    // A "Publish to web" link (/d/e/…) has another id that the API doesn't take.
    if id == "e" || id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return None;
    }
    let gid = [url.fragment(), url.query()].into_iter().flatten().flat_map(|part| part.split('&')).find_map(|pair| pair.strip_prefix("gid="));
    Some(Link { id: id.to_string(), gid: gid.and_then(|g| g.parse().ok()) })
}

/// A tab in A1 notation, with `cells` after it, like "!1:1". Quotes in the title are doubled, as Sheets reads them.
pub fn range(tab: &str, cells: &str) -> String {
    format!("'{}'{cells}", tab.replace('\'', "''"))
}

#[derive(Debug)]
pub enum SheetError {
    /// Google turned down a token of the sign-in with this `sheet_accounts.version`. Once a fresh one is turned down too, the
    /// project has to sign in again, unless it already did since.
    Unauthorized(i32),
    /// Google asked Splee to slow down, for this many seconds when it said.
    Limited(Option<u64>),
    /// Anything else, as the integrations page shows it.
    Failed(String),
}

pub const SIGNED_OUT: &str = "Google signed Splee out of Sheets, so nothing imports or exports. Reconnect your Google account.";
const LIMITED: &str = "Google Sheets asked Splee to slow down, so it tries again in a few minutes.";
pub const NOT_CONNECTED: &str = "Connect a Google account for Sheets on the integrations page first.";

impl SheetError {
    pub fn message(&self) -> String {
        match self {
            SheetError::Unauthorized(_) => SIGNED_OUT.into(),
            SheetError::Limited(_) => LIMITED.into(),
            SheetError::Failed(m) => m.clone(),
        }
    }
}

impl From<sqlx::Error> for SheetError {
    fn from(e: sqlx::Error) -> Self {
        tracing::error!(error = %e, "google sheets: database error");
        SheetError::Failed("Something went wrong on Splee's side. It tries again in a minute.".into())
    }
}

/// How many times a request is tried when Google says to slow down, the unit Retry-After counts in, and the longest wait.
const TRIES: u32 = 3;
const SECOND: Duration = if cfg!(test) { Duration::from_millis(10) } else { Duration::from_secs(1) };
/// Short in tests, so one can run into it.
const TIMEOUT: Duration = if cfg!(test) { Duration::from_secs(1) } else { Duration::from_secs(60) };
const MAX_WAIT_SECS: u64 = 30;
/// How long a project's account waits when Google kept asking and didn't say for how long. The limit is per minute.
const HOLD_SECS: u64 = 2 * 60;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Tab {
    pub id: i64,
    pub title: String,
}

#[derive(Debug, Serialize)]
pub struct Spreadsheet {
    pub title: String,
    pub tabs: Vec<Tab>,
}

/// Talks to Sheets as one Google account.
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: String,
    /// Who signed in, for messages about sharing.
    email: String,
    /// The sign-in the token belongs to.
    version: i32,
}

impl Client {
    fn new(cfg: &Config, access: &Access) -> Result<Self, SheetError> {
        let http = reqwest::Client::builder().user_agent("Splee").timeout(TIMEOUT).build().map_err(|e| SheetError::Failed(e.to_string()))?;
        let base = cfg.sheets_api_url.trim_end_matches('/').to_string();
        Ok(Self { http, base, token: access.token.clone(), email: access.email.clone(), version: access.version })
    }

    /// {base}/v4/spreadsheets/{id}, then `rest`, each segment escaped, so a tab's title can't change the path.
    fn url(&self, id: &str, rest: &[&str]) -> Result<Url, SheetError> {
        let mut url = Url::parse(&format!("{}/v4/spreadsheets/", self.base)).map_err(|e| SheetError::Failed(e.to_string()))?;
        url.path_segments_mut().map_err(|_| SheetError::Failed("the Sheets API address can't take a path".into()))?.pop_if_empty().push(id).extend(rest);
        Ok(url)
    }

    /// Sends the request and reads the answer, waiting and trying again when Google says to slow down, and when it's briefly
    /// down if the request is `idempotent`.
    async fn send(&self, request: RequestBuilder, idempotent: bool) -> Result<Value, SheetError> {
        let request = request.bearer_auth(&self.token);
        for attempt in 0..TRIES {
            let res = request.try_clone().expect("bodies are bytes").send().await;
            let res = res.map_err(|e| SheetError::Failed(format!("Couldn't reach Google Sheets: {}", e.without_url())))?;
            let status = res.status();
            let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            let body: Value = res.json().await.unwrap_or_default();
            let busy = status == StatusCode::TOO_MANY_REQUESTS || (idempotent && status == StatusCode::SERVICE_UNAVAILABLE);
            if busy && attempt + 1 < TRIES {
                tokio::time::sleep(SECOND * retry_after.unwrap_or(1 << attempt).min(MAX_WAIT_SECS) as u32).await;
                continue;
            }
            let said = body["error"]["message"].as_str().filter(|m| !m.is_empty());
            return match status {
                s if s.is_success() => Ok(body),
                StatusCode::UNAUTHORIZED => Err(SheetError::Unauthorized(self.version)),
                StatusCode::TOO_MANY_REQUESTS => Err(SheetError::Limited(retry_after)),
                StatusCode::FORBIDDEN if body["error"]["status"] == "RESOURCE_EXHAUSTED" => Err(SheetError::Limited(retry_after)),
                StatusCode::FORBIDDEN => Err(SheetError::Failed(format!(
                    "{} can't edit this spreadsheet. Share it with that Google account as an editor, or connect one that can.",
                    self.email
                ))),
                StatusCode::NOT_FOUND => Err(SheetError::Failed("Google has no spreadsheet at that link. Check the link.".into())),
                StatusCode::BAD_REQUEST if said.is_some_and(|m| m.contains("Unable to parse range")) => {
                    Err(SheetError::Failed("The spreadsheet has no tab by that name anymore. Pick the tab again.".into()))
                }
                s => Err(SheetError::Failed(said.map_or_else(|| format!("Google Sheets answered {s}"), |m| format!("Google Sheets said: {m}")))),
            };
        }
        unreachable!("the last try returns")
    }

    /// The spreadsheet's name and its tabs, leaving out charts on a sheet of their own.
    pub async fn spreadsheet(&self, id: &str) -> Result<Spreadsheet, SheetError> {
        let mut url = self.url(id, &[])?;
        url.query_pairs_mut().append_pair("fields", "properties.title,sheets.properties(sheetId,title,sheetType)");
        let found = self.send(self.http.get(url), true).await?;
        let tabs = found["sheets"].as_array().into_iter().flatten().map(|s| &s["properties"]).filter(|p| p["sheetType"].as_str().is_none_or(|t| t == "GRID"));
        let tabs = tabs.filter_map(|p| Some(Tab { id: p["sheetId"].as_i64().unwrap_or(0), title: p["title"].as_str()?.to_string() })).collect();
        Ok(Spreadsheet { title: found["properties"]["title"].as_str().unwrap_or("Untitled spreadsheet").to_string(), tabs })
    }

    /// The cells of a range as the sheet shows them, row by row. Sheets leaves out trailing empty rows and cells.
    pub async fn rows(&self, id: &str, range: &str) -> Result<Vec<Vec<String>>, SheetError> {
        let found = self.send(self.http.get(self.url(id, &["values", range])?), true).await?;
        let cell = |v: &Value| v.as_str().map_or_else(|| if v.is_null() { String::new() } else { v.to_string() }, str::to_string);
        Ok(found["values"].as_array().into_iter().flatten().map(|row| row.as_array().into_iter().flatten().map(cell).collect()).collect())
    }

    /// Adds the rows under the last one in the tab. RAW, so a reply that starts with = stays text instead of becoming a formula.
    /// Only a 429 is tried again, since Google turned that down before writing anything. After a 5xx or a timeout the rows may
    /// be in the sheet, so the caller has to read the tab before sending them again.
    pub async fn append(&self, id: &str, range: &str, rows: &[Vec<String>]) -> Result<(), SheetError> {
        let mut url = self.url(id, &["values", &format!("{range}:append")])?;
        url.query_pairs_mut().append_pair("valueInputOption", "RAW").append_pair("insertDataOption", "INSERT_ROWS");
        self.send(self.http.post(url).json(&json!({ "majorDimension": "ROWS", "values": rows })), false).await.map(drop)
    }
}

/// An access token for the project's account, as `access` gives it.
struct Access {
    token: String,
    /// Fetched just now, so there's no point fetching another when Google turns it down.
    renewed: bool,
    email: String,
    version: i32,
}

/// The stored token while it has a minute left, else a new one. `rejected` is a token Google just turned down: one another task
/// saved since is used instead, and otherwise a new one is fetched. Under a lock on the account, since the worker and a button
/// can ask at once.
async fn access(state: &AppState, project_id: Uuid, rejected: Option<&str>) -> Result<Access, SheetError> {
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('sheet-token:' || $1::text, 0))").bind(project_id).execute(&mut *tx).await?;
    let found: Option<(String, String, String, Option<Ts>, bool, i32)> = sqlx::query_as(
        "select email, refresh_token_enc, access_token_enc, expires_at, needs_reconnect, version from sheet_accounts where project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some((email, refresh_enc, access_enc, expires_at, needs_reconnect, version)) = found else { return Err(SheetError::Failed(NOT_CONNECTED.into())) };
    // A sign-in Google revoked isn't tried again on every tick.
    if needs_reconnect {
        return Err(SheetError::Unauthorized(version));
    }
    let secret = &state.cfg.app_secret;
    let unreadable = |e: anyhow::Error| {
        tracing::error!(error = %e, "a Sheets sign-in couldn't be decrypted");
        SheetError::Unauthorized(version)
    };
    if !access_enc.is_empty() && expires_at.is_some_and(|at| at > Utc::now() + chrono::Duration::seconds(60)) {
        let token = crypto::decrypt(secret, &access_enc).map_err(unreadable)?;
        if rejected != Some(token.as_str()) {
            return Ok(Access { token, renewed: false, email, version });
        }
    }
    let app = state.cfg.google.as_ref().ok_or_else(|| SheetError::Failed("Google sign-in isn't set up on this Splee.".into()))?;
    let refresh = crypto::decrypt(secret, &refresh_enc).map_err(unreadable)?;
    match token_request("Google", app, &[("grant_type", "refresh_token"), ("refresh_token", &refresh)]).await {
        Ok(tokens) => {
            // Matched on the old refresh token, so a reconnect since keeps its own.
            sqlx::query("update sheet_accounts set access_token_enc = $3, expires_at = $4 where project_id = $1 and refresh_token_enc = $2")
                .bind(project_id)
                .bind(&refresh_enc)
                .bind(crypto::encrypt(secret, &tokens.access_token))
                .bind(tokens.expires_at())
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok(Access { token: tokens.access_token, renewed: true, email, version })
        }
        Err(TokenError::Revoked(_)) => Err(SheetError::Unauthorized(version)),
        Err(TokenError::Unavailable(_, e)) => Err(SheetError::Failed(format!("Couldn't reach Google to renew the sign-in: {e}"))),
        Err(TokenError::Refused(_, e)) => Err(SheetError::Failed(format!("Google refused to renew the sign-in: {e}"))),
    }
}

/// Stops imports and exports until someone reconnects, and says so on the integrations page. Only while `version` is still the
/// account's sign-in: a request that started before a reconnect can fail after it, and the new sign-in has to stay.
async fn sign_out(db: &PgPool, project_id: Uuid, version: i32) -> sqlx::Result<()> {
    let done = sqlx::query(
        "update sheet_accounts set needs_reconnect = true, access_token_enc = '', expires_at = null, error = $3 where project_id = $1 and version = $2",
    )
    .bind(project_id)
    .bind(version)
    .bind(SIGNED_OUT)
    .execute(db)
    .await?;
    if done.rows_affected() > 0 {
        tracing::warn!(project = %project_id, "google signed splee out of sheets");
    }
    Ok(())
}

/// Runs `f` with a client for the project's account, and again with a new token when Google turned down one that wasn't new,
/// since a token can stop working before it runs out. One turned down after that signs the project out until it reconnects.
/// When Google keeps asking Splee to slow down, the account is held. `f` has to be safe to run twice.
pub async fn authed<T, F, Fut>(state: &AppState, project_id: Uuid, f: F) -> Result<T, SheetError>
where
    F: Fn(Client) -> Fut,
    Fut: Future<Output = Result<T, SheetError>>,
{
    let held: bool = sqlx::query_scalar("select exists (select 1 from sheet_accounts where project_id = $1 and held_until > now())")
        .bind(project_id)
        .fetch_one(&state.db)
        .await?;
    if held {
        return Err(SheetError::Limited(None));
    }
    let result = match access(state, project_id, None).await {
        Ok(first) => match f(Client::new(&state.cfg, &first)?).await {
            Err(SheetError::Unauthorized(_)) if !first.renewed => match access(state, project_id, Some(&first.token)).await {
                Ok(again) => f(Client::new(&state.cfg, &again)?).await,
                Err(e) => Err(e),
            },
            other => other,
        },
        Err(e) => Err(e),
    };
    match &result {
        Err(SheetError::Unauthorized(version)) => sign_out(&state.db, project_id, *version).await?,
        Err(SheetError::Limited(secs)) => {
            let secs = secs.unwrap_or(HOLD_SECS).clamp(HOLD_SECS, 60 * 60) as f64;
            sqlx::query("update sheet_accounts set held_until = now() + make_interval(secs => $2) where project_id = $1")
                .bind(project_id)
                .bind(secs)
                .execute(&state.db)
                .await?;
        }
        _ => {}
    }
    result
}

/// A tab leads import from or lead events go to, as the pages and the API show it.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Connection {
    pub id: Uuid,
    pub project_id: Uuid,
    /// import or export.
    pub kind: String,
    pub spreadsheet_id: String,
    pub spreadsheet_name: String,
    pub tab: String,
    /// {field: column header}, for an import.
    pub mapping: Value,
    pub segment_id: Option<Uuid>,
    pub keep_importing: bool,
    pub next_import_at: Option<Ts>,
    /// The lead events an export writes.
    pub events: Vec<String>,
    pub last_sync_at: Option<Ts>,
    pub rows_added: i64,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    pub created_at: Ts,
}

pub const CONNECTION_COLUMNS: &str = "id, project_id, kind, spreadsheet_id, spreadsheet_name, tab, mapping, segment_id, keep_importing, next_import_at, \
     events, last_sync_at, rows_added, error, failed_at, created_at";

impl Connection {
    pub async fn list(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<Connection>> {
        sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from sheet_connections where project_id = $1 order by kind desc, created_at"))
            .bind(project_id)
            .fetch_all(db)
            .await
    }

    /// The project's connection with this id, so another project's can't be reached through it.
    pub async fn find(db: &PgPool, project_id: Uuid, id: Uuid) -> sqlx::Result<Option<Connection>> {
        sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from sheet_connections where id = $1 and project_id = $2"))
            .bind(id)
            .bind(project_id)
            .fetch_optional(db)
            .await
    }
}

/// Keeps how the last import or export went for the integrations page. A success clears the error.
async fn record<T>(db: &PgPool, id: Uuid, result: &Result<T, SheetError>) -> sqlx::Result<()> {
    let error = result.as_ref().err().map(SheetError::message);
    sqlx::query(
        "update sheet_connections set error = $2, failed_at = case when $2::text is null then null else now() end,
           last_sync_at = case when $2::text is null then now() else last_sync_at end
         where id = $1",
    )
    .bind(id)
    .bind(error)
    .execute(db)
    .await
    .map(drop)
}

/// Writes waiting export rows, then reads the import tabs that are due.
pub async fn tick(state: &AppState) -> anyhow::Result<()> {
    export::flush_all(state).await?;
    import::run_due(state).await?;
    Ok(())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state).await {
                tracing::error!(error = ?e, "google sheets: tick failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}
