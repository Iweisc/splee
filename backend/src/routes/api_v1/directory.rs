use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::response::Response;
use axum::{Extension, Json};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::{SinceQuery, check_project};
use crate::audit::Who;
use crate::auth::ApiKeyAuth;
use crate::company_filters::{self, Described, Model};
use crate::error::{AppError, AppResult};
use crate::local_businesses::{self, AddOutcome, Results};
use crate::lookalikes::{self, Lookalikes};
use crate::models::{Segment, SendingDomain};
use crate::porkbun::Availability;
use crate::routes::directory::{self, Added, ByDomains, ByDomainsBody, DescribeBody, SearchQuery};
use crate::routes::directory_export::{self, ExportQuery};
use crate::routes::lookalikes::{self as lookalike_routes, AddBody as AddLookalikesBody};
use crate::routes::saved_searches::{self, NewMatches, NewSearch, SearchRow};
use crate::routes::sending_domains::{self, SuggestQuery};
use crate::routes::util_input;
use crate::routes::util_pagination::PageQuery;
use crate::routes::util_scope;
use crate::state::AppState;

pub(in crate::routes) async fn search_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SearchQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<directory::Page>> {
    Ok(Json(directory::page_of(&state.db, auth.org_id, &q, &page).await?))
}

/// The filters a sentence like "dentists in Leeds that list an email" stands for, to pass to GET /v1/companies.
pub(in crate::routes) async fn companies_filters(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Extension(model): Extension<Arc<dyn Model>>,
    Json(body): Json<DescribeBody>,
) -> AppResult<Json<Described>> {
    Ok(Json(company_filters::describe(&state, model.as_ref(), auth.org_id, &body.query).await?))
}

/// The database's company for each of up to 1,000 domains, and the domains it doesn't have.
pub(in crate::routes) async fn companies_by_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<ByDomainsBody>,
) -> AppResult<Json<ByDomains>> {
    Ok(Json(directory::by_domains(&state.db, auth.org_id, body).await?))
}

/// The organization's sending domains as the Domains page lists them, with their DNS and how many mailboxes use each.
pub(in crate::routes) async fn list_sending_domains(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<SendingDomain>>> {
    Ok(Json(crate::domains::list(&state.db, auth.org_id).await?))
}

/// The Domains page's suggestions for the company's domain, checked with the organization's Porkbun account.
pub(in crate::routes) async fn suggest_sending_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SuggestQuery>,
) -> AppResult<Json<Vec<Availability>>> {
    Ok(Json(sending_domains::suggest(&state, auth.org_id, &q).await?))
}

#[derive(Deserialize)]
pub(in crate::routes) struct AvailabilityQuery {
    /// Separated by commas.
    domains: String,
}

/// The most names one availability check takes.
const MAX_AVAILABILITY: usize = 20;

/// Whether the organization's Porkbun account can buy each name and at what price. Buying stays in the app.
pub(in crate::routes) async fn check_sending_domains(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<AvailabilityQuery>,
) -> AppResult<Json<Vec<Availability>>> {
    let mut names: Vec<String> = vec![];
    for raw in q.domains.split(',').map(str::trim).filter(|d| !d.is_empty()) {
        let name = util_input::domain(raw).ok_or_else(|| AppError::BadRequest(format!("{raw:?} isn't a domain")))?;
        if !names.contains(&name) {
            names.push(name);
        }
    }
    if names.is_empty() || names.len() > MAX_AVAILABILITY {
        return Err(AppError::BadRequest(format!("Pass 1 to {MAX_AVAILABILITY} domains, separated by commas")));
    }
    Ok(Json(sending_domains::availability(&state, auth.org_id, &names).await?))
}

/// The search as a file, up to 50,000 rows a page. See routes/directory_export.rs.
pub(in crate::routes) async fn export_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SearchQuery>,
    Query(x): Query<ExportQuery>,
) -> AppResult<Response> {
    directory_export::export(&state, Who::from(&auth), &q, &x).await
}

#[derive(Deserialize)]
pub(in crate::routes) struct AddCompaniesBody {
    #[serde(default)]
    domains: Vec<String>,
}

pub(in crate::routes) async fn add_companies(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddCompaniesBody>,
) -> AppResult<Json<Added>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(directory::add_domains(&state.db, auth.org_id, segment.project_id, segment.id, &body.domains).await?))
}

pub(in crate::routes) async fn list_saved_searches(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Vec<SearchRow>>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::list_of(&state.db, id).await?))
}

/// Without a member behind the key, nobody owns the search, so only its chat alerts and GET .../new say what it finds.
pub(in crate::routes) async fn save_search(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<NewSearch>,
) -> AppResult<Json<SearchRow>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::create_in(&state, id, auth.user_id, body).await?))
}

pub(in crate::routes) async fn delete_saved_search(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    saved_searches::delete_one(&state.db, id, auth.org_id).await?;
    Ok(Json(json!({ "ok": true })))
}

/// Unlike opening it in the app, reading it here leaves its new matches counted as new.
pub(in crate::routes) async fn new_matches(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<NewMatches>> {
    let search = saved_searches::load(&state.db, id, auth.org_id).await?;
    Ok(Json(saved_searches::matches_of(&state.db, &search, q.since, &page).await?))
}

#[derive(Deserialize)]
pub(in crate::routes) struct LookalikesQuery {
    /// Comma-separated domains to match instead of the project's seeds.
    seeds: Option<String>,
}

/// Companies in the database like the project's customers, with the profile they were matched on, like the Companies page's
/// "Like your customers".
pub(in crate::routes) async fn list_lookalikes(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<LookalikesQuery>,
) -> AppResult<Json<Lookalikes>> {
    check_project(&state.db, id, auth.org_id).await?;
    let seeds: Option<Vec<String>> = q.seeds.map(|s| s.split(',').map(String::from).collect());
    let seeds = seeds.as_deref().map(lookalike_routes::domains).transpose()?.filter(|s| !s.is_empty());
    Ok(Json(lookalikes::find(&state.db, id, seeds.as_deref()).await?))
}

pub(in crate::routes) async fn add_lookalikes(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddLookalikesBody>,
) -> AppResult<Json<lookalike_routes::Added>> {
    check_project(&state.db, id, auth.org_id).await?;
    Ok(Json(lookalike_routes::add_matches(&state, auth.org_id, id, body).await?))
}

/// Google Maps through the organization's key, like the Companies page's Local businesses tab. With project_id, says which
/// businesses that project already has.
pub(in crate::routes) async fn search_local_businesses(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<local_businesses::SearchQuery>,
) -> AppResult<Json<Results>> {
    if let Some(project) = q.project_id {
        check_project(&state.db, project, auth.org_id).await?;
    }
    Ok(Json(local_businesses::search(&state, auth.org_id, &q).await?))
}

#[derive(Deserialize)]
pub(in crate::routes) struct AddLocalBody {
    #[serde(default)]
    place_ids: Vec<String>,
}

pub(in crate::routes) async fn add_local_businesses(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<AddLocalBody>,
) -> AppResult<Json<AddOutcome>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    Ok(Json(local_businesses::add(&state, auth.org_id, segment.project_id, segment.id, &body.place_ids).await?))
}
