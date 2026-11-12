//! The Sheets API as its docs describe it, on a local port: spreadsheets.get, values.get and values.append, over spreadsheets
//! held in memory. Tokens come from mail::oauth's FakeProvider, so any access-{n} or access-from-code is taken unless a test put
//! it in World::dead. Tests read the requests back and the tabs' rows.

use std::collections::{BTreeMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct Seen {
    pub method: &'static str,
    /// The spreadsheet id, then the range as Sheets got it, decoded.
    pub path: String,
    pub token: String,
}

#[derive(Clone, Debug, Default)]
pub struct FakeTab {
    pub id: i64,
    pub title: String,
    pub rows: Vec<Vec<String>>,
}

#[derive(Default)]
pub struct World {
    pub requests: Vec<Seen>,
    /// By spreadsheet id: its title and tabs.
    pub spreadsheets: BTreeMap<String, (String, Vec<FakeTab>)>,
    /// Access tokens answered with 401.
    pub dead: HashSet<String>,
    /// The next this many requests are told to slow down, with Retry-After: 1.
    pub busy: usize,
    /// Appends wait this long, so a test can start a second tick while one is writing.
    pub append_delay: Duration,
    /// How the next appends answer after writing their rows, as Google can when something breaks on its side.
    pub lost: VecDeque<Lost>,
}

#[derive(Clone, Copy, Debug)]
pub enum Lost {
    /// This status instead of 200.
    Status(StatusCode),
    /// 200, but only after the client gave up waiting.
    Late,
}

#[derive(Clone)]
pub struct Fake {
    pub base: String,
    pub world: Arc<Mutex<World>>,
}

impl Fake {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fake = Fake { base: format!("http://{}", listener.local_addr().unwrap()), world: Arc::default() };
        let app = Router::new()
            .route("/v4/spreadsheets/{id}", get(spreadsheet))
            .route("/v4/spreadsheets/{id}/values/{range}", get(values).post(append))
            .with_state(fake.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        fake
    }

    /// A spreadsheet with these tabs, each with its rows.
    pub fn sheet(&self, id: &str, title: &str, tabs: &[(&str, Vec<Vec<&str>>)]) {
        let tabs = tabs
            .iter()
            .enumerate()
            .map(|(i, (name, rows))| FakeTab { id: i as i64 * 100, title: name.to_string(), rows: rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect() })
            .collect();
        self.world.lock().unwrap().spreadsheets.insert(id.into(), (title.into(), tabs));
    }

    pub fn rows(&self, id: &str, tab: &str) -> Vec<Vec<String>> {
        let world = self.world.lock().unwrap();
        world.spreadsheets[id].1.iter().find(|t| t.title == tab).unwrap().rows.clone()
    }

    pub fn set_rows(&self, id: &str, tab: &str, rows: Vec<Vec<&str>>) {
        let mut world = self.world.lock().unwrap();
        let tab = world.spreadsheets.get_mut(id).unwrap().1.iter_mut().find(|t| t.title == tab).unwrap();
        tab.rows = rows.iter().map(|r| r.iter().map(|c| c.to_string()).collect()).collect();
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.world.lock().unwrap().requests.clone()
    }

    pub fn appends(&self) -> usize {
        self.requests().iter().filter(|r| r.method == "POST").count()
    }
}

/// Notes the request, and answers for the token and for World::busy before the endpoint does.
fn gate(fake: &Fake, method: &'static str, path: String, headers: &HeaderMap) -> Option<Response> {
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or_default().to_string();
    let mut world = fake.world.lock().unwrap();
    world.requests.push(Seen { method, path, token: token.clone() });
    if world.busy > 0 {
        world.busy -= 1;
        let body = json!({ "error": { "code": 429, "message": "Quota exceeded for quota metric 'Read requests'", "status": "RESOURCE_EXHAUSTED" } });
        return Some((StatusCode::TOO_MANY_REQUESTS, [("retry-after", "1")], axum::Json(body)).into_response());
    }
    if !token.starts_with("access-") || world.dead.contains(&token) {
        let body = json!({ "error": { "code": 401, "message": "Request had invalid authentication credentials.", "status": "UNAUTHENTICATED" } });
        return Some((StatusCode::UNAUTHORIZED, axum::Json(body)).into_response());
    }
    None
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, axum::Json(json!({ "error": { "code": 404, "message": "Requested entity was not found.", "status": "NOT_FOUND" } }))).into_response()
}

/// 'Tab''s name'!1:6 is the tab and the rows from 1 to 6, counting from 1. Without rows, every row.
fn parse_range(range: &str) -> Option<(String, Option<(usize, usize)>)> {
    let rest = range.strip_prefix('\'')?;
    let mut title = String::new();
    let mut chars = rest.char_indices().peekable();
    let mut end = None;
    while let Some((i, c)) = chars.next() {
        if c == '\'' {
            if chars.peek().map(|(_, n)| *n) == Some('\'') {
                chars.next();
                title.push('\'');
                continue;
            }
            end = Some(i + 1);
            break;
        }
        title.push(c);
    }
    let cells = rest[end?..].strip_prefix('!');
    let rows = cells.and_then(|c| {
        let (from, to) = c.split_once(':')?;
        Some((from.parse().ok()?, to.parse().ok()?))
    });
    Some((title, rows))
}

async fn spreadsheet(State(fake): State<Fake>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if let Some(res) = gate(&fake, "GET", id.clone(), &headers) {
        return res;
    }
    let world = fake.world.lock().unwrap();
    let Some((title, tabs)) = world.spreadsheets.get(&id) else { return not_found() };
    let sheets: Vec<Value> = tabs.iter().map(|t| json!({ "properties": { "sheetId": t.id, "title": t.title, "sheetType": "GRID" } })).collect();
    axum::Json(json!({ "properties": { "title": title }, "sheets": sheets })).into_response()
}

async fn values(State(fake): State<Fake>, Path((id, range)): Path<(String, String)>, headers: HeaderMap) -> Response {
    if let Some(res) = gate(&fake, "GET", format!("{id}/{range}"), &headers) {
        return res;
    }
    let world = fake.world.lock().unwrap();
    let Some((_, tabs)) = world.spreadsheets.get(&id) else { return not_found() };
    let found = parse_range(&range).and_then(|(title, rows)| Some((tabs.iter().find(|t| t.title == title)?, rows)));
    let Some((tab, rows)) = found else {
        let body = json!({ "error": { "code": 400, "message": format!("Unable to parse range: {range}"), "status": "INVALID_ARGUMENT" } });
        return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
    };
    let (from, to) = rows.unwrap_or((1, usize::MAX));
    let picked: Vec<&Vec<String>> = tab.rows.iter().skip(from - 1).take(to.saturating_add(1) - from).collect();
    // Sheets leaves out the values key for an empty range.
    if picked.is_empty() {
        return axum::Json(json!({ "range": range, "majorDimension": "ROWS" })).into_response();
    }
    axum::Json(json!({ "range": range, "majorDimension": "ROWS", "values": picked })).into_response()
}

async fn append(State(fake): State<Fake>, Path((id, range)): Path<(String, String)>, headers: HeaderMap, body: axum::Json<Value>) -> Response {
    let Some(range) = range.strip_suffix(":append").map(str::to_string) else { return not_found() };
    if let Some(res) = gate(&fake, "POST", format!("{id}/{range}"), &headers) {
        return res;
    }
    let delay = fake.world.lock().unwrap().append_delay;
    tokio::time::sleep(delay).await;
    // In a block, so the lock is let go before the answer waits.
    let (added, lost) = {
        let mut world = fake.world.lock().unwrap();
        let Some((_, tabs)) = world.spreadsheets.get_mut(&id) else { return not_found() };
        let Some(tab) = parse_range(&range).and_then(|(title, _)| tabs.iter_mut().find(|t| t.title == title)) else { return not_found() };
        let rows: Vec<Vec<String>> = serde_json::from_value(body["values"].clone()).unwrap_or_default();
        let added = rows.len();
        tab.rows.extend(rows);
        (added, world.lost.pop_front())
    };
    match lost {
        Some(Lost::Status(status)) => {
            (status, axum::Json(json!({ "error": { "code": status.as_u16(), "message": "The service is currently unavailable.", "status": "UNAVAILABLE" } }))).into_response()
        }
        Some(Lost::Late) => {
            tokio::time::sleep(super::TIMEOUT * 2).await;
            axum::Json(json!({ "spreadsheetId": id, "updates": { "updatedRows": added } })).into_response()
        }
        None => axum::Json(json!({ "spreadsheetId": id, "updates": { "updatedRows": added } })).into_response(),
    }
}

#[test]
fn ranges_read_like_sheets_reads_them() {
    assert_eq!(parse_range("'Leads'"), Some(("Leads".into(), None)));
    assert_eq!(parse_range("'Bob''s list'!1:6"), Some(("Bob's list".into(), Some((1, 6)))));
    assert_eq!(parse_range("'Hot'!A1"), Some(("Hot".into(), None)));
    assert_eq!(parse_range("Leads"), None);
}
