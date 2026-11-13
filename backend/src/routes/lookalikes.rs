//! "Like your customers" on the Companies page: the seeds, the profile with its toggles, the ranked matches and adding them
//! to a campaign. API v1 and the MCP server use the same helpers. See lookalikes.rs.

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::imports::check_segment;
use super::util_input;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::lookalikes::{self, Lookalikes, Match, Settings};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/lookalikes", get(show))
        .route("/projects/{id}/lookalikes/settings", put(save))
        .route("/projects/{id}/lookalikes/add", post(add))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Lookalikes>> {
    auth.project(&state.db, id).await?;
    Ok(Json(lookalikes::find(&state.db, id, None).await?))
}

/// Replaces the hand-picked seeds and the parts switched off, and answers with the new profile and matches.
async fn save(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Settings>) -> AppResult<Json<Lookalikes>> {
    auth.project(&state.db, id).await?;
    let settings = checked(body)?;
    lookalikes::save_settings(&state.db, id, &settings).await?;
    Ok(Json(lookalikes::find(&state.db, id, None).await?))
}

fn checked(body: Settings) -> AppResult<Settings> {
    let added = domains(&body.added)?;
    if added.len() > lookalikes::MAX_ADDED_SEEDS {
        return Err(AppError::BadRequest(format!("Add at most {} customers by hand", lookalikes::MAX_ADDED_SEEDS)));
    }
    if let Some(key) = body.off.iter().find(|k| !lookalikes::PARTS.iter().any(|p| p.key == k.as_str())) {
        return Err(AppError::BadRequest(format!("{key} isn't a part of the profile")));
    }
    // A domain added again is no longer removed.
    let removed = domains(&body.removed)?.into_iter().filter(|d| !added.contains(d)).collect();
    Ok(Settings { added, removed, off: body.off })
}

/// The domains, each once, or a 400 naming the first that isn't one.
pub(super) fn domains(input: &[String]) -> AppResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for raw in input.iter().filter(|d| !d.trim().is_empty()) {
        let domain = util_input::domain(raw).ok_or_else(|| AppError::BadRequest(format!("{} isn't a website domain", raw.trim())))?;
        if !out.contains(&domain) {
            out.push(domain);
        }
    }
    Ok(out)
}

#[derive(Deserialize)]
pub(super) struct AddBody {
    pub segment_id: Uuid,
    /// Domains from the matches. Without them, the `top` best are added.
    #[serde(default)]
    pub domains: Vec<String>,
    pub top: Option<usize>,
    /// The seeds to match, like the API's seeds parameter. The saved ones without it.
    pub seeds: Option<Vec<String>>,
}

#[derive(Serialize)]
pub(super) struct Added {
    added: u64,
    skipped: u64,
}

async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Added>> {
    auth.project(&state.db, id).await?;
    Ok(Json(add_matches(&state, auth.org_id, id, body).await?))
}

/// Ranks again and adds the picked matches, so each gets its score and reasons. Picked domains that aren't among the
/// matches any more, like ones added since, are skipped. The caller checks the project.
pub(super) async fn add_matches(state: &AppState, org_id: Uuid, project_id: Uuid, body: AddBody) -> AppResult<Added> {
    check_segment(state, project_id, Some(body.segment_id)).await?;
    let db = &state.db;
    let seeds = body.seeds.as_deref().map(domains).transpose()?;
    let picked = domains(&body.domains)?;
    let top = body.top.unwrap_or(0).min(lookalikes::LIMIT as usize);
    if picked.is_empty() && top == 0 {
        return Err(AppError::BadRequest("Pick at least one company, or pass top".into()));
    }
    let found = lookalikes::find(db, project_id, seeds.as_deref()).await?;
    let matches: Vec<&Match> = match picked.is_empty() {
        true => found.results.iter().take(top).collect(),
        false => found.results.iter().filter(|m| picked.contains(&m.domain)).collect(),
    };
    let asked = if picked.is_empty() { matches.len() } else { picked.len() };
    let added = if matches.is_empty() { 0 } else { lookalikes::add(db, org_id, project_id, body.segment_id, &matches).await? };
    Ok(Added { added, skipped: asked as u64 - added })
}
