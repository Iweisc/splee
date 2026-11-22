use axum::Json;
use axum::extract::{Path, Query, State};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::SinceQuery;
use crate::auth::ApiKeyAuth;
use crate::error::{AppError, AppResult};
use crate::models::{DedupList, Suppression, SuppressionList};
use crate::routes::dedup_lists::{self, Contents, OffsetQuery as DedupPage};
use crate::routes::suppression_lists::{self as lists, Kind, ScopeBody};
use crate::routes::suppressions;
use crate::routes::util_pagination::{PageQuery, Paginated};
use crate::state::AppState;

const MAX_SUPPRESSIONS_PER_REQUEST: usize = 1000;
const MAX_LIST_ENTRIES_PER_REQUEST: usize = 10_000;

#[derive(Deserialize)]
pub(super) struct SuppressionsBody {
    values: Vec<String>,
}

/// For syncing do-not-contact lists from a CRM. The sender checks suppressions before every email, so queued emails to these stop too.
/// The org's do-not-contact list, oldest first, so a CRM or another sending tool can copy unsubscribes and bounces. Pages,
/// not a cursor: one paste or POST adds many entries with the same created_at, so `since` alone can't resume mid-batch.
pub(super) async fn list_suppressions(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Query(q): Query<SinceQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Suppression>>> {
    let filter = "from suppressions where org_id = $1 and ($2::timestamptz is null or created_at > $2)";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {filter}")).bind(auth.org_id).bind(q.since).fetch_one(&state.db).await?;
    let items = sqlx::query_as::<_, Suppression>(&format!("select * {filter} order by created_at, id limit $3 offset $4"))
        .bind(auth.org_id)
        .bind(q.since)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(&state.db)
        .await?;
    Ok(Json(Paginated { items, total }))
}

pub(super) async fn add_suppressions(State(state): State<AppState>, auth: ApiKeyAuth, Json(body): Json<SuppressionsBody>) -> AppResult<Json<Value>> {
    if body.values.len() > MAX_SUPPRESSIONS_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_SUPPRESSIONS_PER_REQUEST} values per request")));
    }
    let added = suppressions::insert(&state.db, auth.org_id, body.values.iter().map(|v| v.trim()).filter(|v| !v.is_empty())).await?;
    Ok(Json(json!({ "added": added })))
}

pub(super) async fn list_suppression_lists(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<SuppressionList>>> {
    Ok(Json(lists::all(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct ListPerson {
    email: String,
}

#[derive(Deserialize)]
pub(super) struct PeopleListBody {
    list: String,
    people: Vec<ListPerson>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

#[derive(Deserialize)]
pub(super) struct CompaniesListBody {
    list: String,
    domains: Vec<String>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

fn check_list_size(n: usize) -> AppResult<()> {
    if n > MAX_LIST_ENTRIES_PER_REQUEST {
        return Err(AppError::BadRequest(format!("Send at most {MAX_LIST_ENTRIES_PER_REQUEST} entries per request")));
    }
    Ok(())
}

/// Adds people to the named list, making it when the org has none by that name. Sends to them stop in the projects it covers.
pub(super) async fn add_people_to_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<PeopleListBody>,
) -> AppResult<Json<lists::Added>> {
    check_list_size(body.people.len())?;
    let emails = body.people.iter().map(|p| p.email.as_str());
    Ok(Json(lists::add_by_name(&state.db, auth.org_id, &body.list, Kind::People, body.project_ids, emails).await?))
}

pub(super) async fn add_companies_to_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<CompaniesListBody>,
) -> AppResult<Json<lists::Added>> {
    check_list_size(body.domains.len())?;
    let domains = body.domains.iter().map(String::as_str);
    Ok(Json(lists::add_by_name(&state.db, auth.org_id, &body.list, Kind::Companies, body.project_ids, domains).await?))
}

pub(super) async fn rescope_suppression_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ScopeBody>,
) -> AppResult<Json<SuppressionList>> {
    Ok(Json(lists::set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

pub(super) async fn delete_suppression_list(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    lists::delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub(super) struct OffsetQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

pub(super) async fn suppression_list_entries(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<OffsetQuery>,
) -> AppResult<Json<Paginated<String>>> {
    let (limit, offset) = (q.limit.unwrap_or(100).clamp(1, 1000), q.offset.unwrap_or(0).max(0));
    Ok(Json(lists::entries_page(&state.db, auth.org_id, id, limit, offset).await?))
}

pub(in crate::routes) async fn list_dedup_lists(State(state): State<AppState>, auth: ApiKeyAuth) -> AppResult<Json<Vec<DedupList>>> {
    Ok(Json(dedup_lists::all(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
pub(in crate::routes) struct DedupBody {
    name: String,
    domains: Vec<String>,
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    project_ids: Option<Option<Vec<Uuid>>>,
}

/// Adds domains to the named list, making it when the org has none by that name. Searches in the projects it covers leave
/// those companies out. Sending is never affected.
pub(in crate::routes) async fn add_to_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Json(body): Json<DedupBody>,
) -> AppResult<Json<dedup_lists::Added>> {
    check_list_size(body.domains.len())?;
    let domains = body.domains.iter().map(String::as_str);
    Ok(Json(dedup_lists::add_by_name(&state.db, auth.org_id, &body.name, body.project_ids, domains).await?))
}

pub(in crate::routes) async fn get_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Query(q): Query<DedupPage>,
) -> AppResult<Json<Contents>> {
    Ok(Json(dedup_lists::contents(&state.db, auth.org_id, id, &q).await?))
}

pub(in crate::routes) async fn rescope_dedup_list(
    State(state): State<AppState>,
    auth: ApiKeyAuth,
    Path(id): Path<Uuid>,
    Json(body): Json<ScopeBody>,
) -> AppResult<Json<DedupList>> {
    Ok(Json(dedup_lists::set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

pub(in crate::routes) async fn delete_dedup_list(State(state): State<AppState>, auth: ApiKeyAuth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    dedup_lists::delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}
