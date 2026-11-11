//! Bringing campaigns over from Instantly, Smartlead or lemlist (outreach/). The key is checked by listing the campaigns, then
//! sent again to start the import, which keeps it encrypted until the worker is done with it.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{OutreachImport, Segment, SegmentStep};
use crate::outreach::{Campaign, Client, Picked, Tool};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/outreach/campaigns", post(campaigns))
        .route("/projects/{id}/outreach/imports", get(list).post(start))
        .route("/segments/{id}/steps", get(steps))
}

/// Columns of outreach_imports but the key.
const COLUMNS: &str = "id, project_id, tool, status, campaigns, include_blocklist, blocklist_added, error, created_at, finished_at";

/// At most this many campaigns in one import, which keeps one import from holding the worker for hours.
const MAX_CAMPAIGNS: usize = 50;

#[derive(Deserialize)]
struct KeyBody {
    tool: String,
    api_key: String,
}

impl KeyBody {
    fn client(&self, state: &AppState) -> AppResult<Client> {
        let tool = Tool::parse(&self.tool).ok_or_else(|| AppError::BadRequest("Pick Instantly, Smartlead or lemlist".into()))?;
        if self.api_key.trim().is_empty() {
            return Err(AppError::BadRequest(format!("Paste your {} API key", tool.label())));
        }
        Client::new(tool, &state.cfg.outreach_api_url, &self.api_key)
    }
}

#[derive(Serialize)]
struct Campaigns {
    campaigns: Vec<Campaign>,
}

/// Checks the key by listing the tool's campaigns, with their lead counts when the tool tells them.
async fn campaigns(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<KeyBody>) -> AppResult<Json<Campaigns>> {
    auth.project(&state.db, id).await?;
    Ok(Json(Campaigns { campaigns: body.client(&state)?.campaigns().await? }))
}

#[derive(Deserialize)]
struct StartBody {
    #[serde(flatten)]
    key: KeyBody,
    campaigns: Vec<PickedBody>,
    #[serde(default = "yes")]
    blocklist: bool,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
struct PickedBody {
    id: String,
    name: String,
    leads: Option<i64>,
}

async fn start(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<StartBody>) -> AppResult<Json<OutreachImport>> {
    auth.project(&state.db, id).await?;
    let client = body.key.client(&state)?;
    if body.campaigns.is_empty() {
        return Err(AppError::BadRequest("Pick at least one campaign".into()));
    }
    if body.campaigns.len() > MAX_CAMPAIGNS {
        return Err(AppError::BadRequest(format!("Pick at most {MAX_CAMPAIGNS} campaigns at a time")));
    }
    let mut picked: Vec<Picked> = Vec::new();
    for c in body.campaigns {
        let id = c.id.trim().to_string();
        // Ids go into the tool's URLs, so only the characters the tools use for them are let through.
        if id.is_empty() || id.len() > 100 || !id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
            return Err(AppError::BadRequest("That campaign id isn't one the tool gave".into()));
        }
        if !picked.iter().any(|p| p.id == id) {
            picked.push(Picked { id, name: c.name.trim().chars().take(200).collect(), leads: c.leads, status: "queued".into(), ..Picked::default() });
        }
    }
    let import = sqlx::query_as::<_, OutreachImport>(&format!(
        "insert into outreach_imports (org_id, project_id, tool, api_key_enc, campaigns, include_blocklist, created_by)
         values ($1, $2, $3, $4, $5, $6, $7) returning {COLUMNS}"
    ))
    .bind(auth.org_id)
    .bind(id)
    .bind(client.tool.name())
    .bind(crate::crypto::encrypt(&state.cfg.app_secret, body.key.api_key.trim()))
    .bind(json!(picked))
    .bind(body.blocklist)
    .bind(auth.user_id)
    .fetch_one(&state.db)
    .await
    .map_err(|e| match AppError::from(e) {
        AppError::Conflict(_) => AppError::Conflict("An import is already running for this project. Wait for it to finish.".into()),
        other => other,
    })?;
    Ok(Json(import))
}

/// The project's latest imports, newest first, which the Import page polls while one runs.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<OutreachImport>>> {
    auth.project(&state.db, id).await?;
    let imports = sqlx::query_as::<_, OutreachImport>(&format!(
        "select {COLUMNS} from outreach_imports where project_id = $1 order by created_at desc limit 5"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(imports))
}

#[derive(Serialize)]
struct Steps {
    /// The tool the campaign came from, or null for a campaign made in Splee.
    imported_from: Option<String>,
    steps: Vec<SegmentStep>,
}

async fn steps(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Steps>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let imported_from: Option<String> = sqlx::query_scalar("select imported_from from segments where id = $1").bind(segment.id).fetch_one(&state.db).await?;
    let steps = sqlx::query_as::<_, SegmentStep>("select position, delay_days, subject, body from segment_steps where segment_id = $1 order by position")
        .bind(segment.id)
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Steps { imported_from: imported_from.and_then(|t| Tool::parse(&t)).map(|t| t.label().to_string()), steps }))
}
