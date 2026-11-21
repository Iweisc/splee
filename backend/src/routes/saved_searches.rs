//! Saved company database searches on a project's Companies page. saved_searches.rs checks them for new companies.

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::directory::SearchQuery;
use super::imports::check_segment;
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::auth::Auth;
use crate::directory::{LISTING, Listing};
use crate::error::{AppError, AppResult};
use crate::models::{SavedSearch, Ts};
use crate::saved_searches;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/directory/searches", get(list).post(create))
        .route("/directory/searches/{id}", patch(update).delete(remove))
        .route("/directory/searches/{id}/new", get(new_matches))
}

/// Each search is checked hourly, so a project's are capped.
const MAX_PER_PROJECT: i64 = 50;

/// A saved search as the list shows it.
#[derive(Serialize, FromRow)]
pub(super) struct SearchRow {
    #[serde(flatten)]
    #[sqlx(flatten)]
    search: SavedSearch,
    campaign_name: Option<String>,
    /// Companies found since it was last opened in the app, and when the first of them was found.
    new_matches: i64,
    new_since: Option<Ts>,
}

const ROW: &str = "select s.*, g.name as campaign_name,
        (select count(*) from saved_search_matches m where m.search_id = s.id and m.found_at > coalesce(s.opened_at, '-infinity')) as new_matches,
        (select min(m.found_at) from saved_search_matches m where m.search_id = s.id and m.found_at > coalesce(s.opened_at, '-infinity')) as new_since
    from saved_searches s left join segments g on g.id = s.add_to_segment_id";

/// The project's searches, newest first.
pub(super) async fn list_of(db: &PgPool, project_id: Uuid) -> AppResult<Vec<SearchRow>> {
    Ok(sqlx::query_as(&format!("{ROW} where s.project_id = $1 order by s.created_at desc, s.id")).bind(project_id).fetch_all(db).await?)
}

async fn row(db: &PgPool, id: Uuid) -> AppResult<SearchRow> {
    Ok(sqlx::query_as(&format!("{ROW} where s.id = $1")).bind(id).fetch_one(db).await?)
}

/// A search in the organization, or 404.
pub(super) async fn load(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<SavedSearch> {
    util_scope::load(db, "saved_searches", id, org_id).await
}

#[derive(Deserialize)]
pub(super) struct NewSearch {
    name: String,
    #[serde(flatten)]
    filters: SearchQuery,
    #[serde(default)]
    chat_alerts: bool,
    /// A campaign of the project to add new matches to.
    segment_id: Option<Uuid>,
}

fn name(name: &str) -> AppResult<String> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 120 {
        return Err(AppError::BadRequest("Name the search in 120 characters or fewer".into()));
    }
    Ok(name)
}

/// Saves the search and records what it matches now, so its first alert is about companies that come after. `user_id` is
/// who hears about new matches, None for an API key. The caller checks the project belongs to the organization.
pub(super) async fn create_in(state: &AppState, project_id: Uuid, user_id: Option<Uuid>, body: NewSearch) -> AppResult<SearchRow> {
    let name = name(&body.name)?;
    let f = &body.filters;
    let field = |v: &Option<String>| v.as_deref().unwrap_or_default().trim().to_string();
    let (q, country, city, category, tech) = (field(&f.q), field(&f.country).to_uppercase(), field(&f.city), field(&f.category), field(&f.tech));
    if [&q, &country, &city, &category, &tech].iter().all(|v| v.is_empty()) && !f.has_email && !f.has_phone {
        return Err(AppError::BadRequest("Pick at least one filter to save".into()));
    }
    // A bad country or a tool Splee can't detect is turned away here rather than at every check.
    f.search()?;
    check_segment(state, project_id, body.segment_id).await?;
    let saved: i64 = sqlx::query_scalar("select count(*) from saved_searches where project_id = $1").bind(project_id).fetch_one(&state.db).await?;
    if saved >= MAX_PER_PROJECT {
        return Err(AppError::BadRequest(format!("A project can save up to {MAX_PER_PROJECT} searches. Delete one first")));
    }
    // checked_at keeps the hourly check from also taking it while the baseline is recorded below.
    let search: SavedSearch = sqlx::query_as(
        "insert into saved_searches (project_id, user_id, name, q, country, city, category, has_email, has_phone, tech, chat_alerts,
            add_to_segment_id, checked_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, now()) returning *",
    )
    .bind(project_id)
    .bind(user_id)
    .bind(&name)
    .bind(&q)
    .bind(&country)
    .bind(&city)
    .bind(&category)
    .bind(f.has_email)
    .bind(f.has_phone)
    .bind(&tech)
    .bind(body.chat_alerts)
    .bind(body.segment_id)
    .fetch_one(&state.db)
    .await?;
    saved_searches::check(&state.db, &search).await?;
    row(&state.db, search.id).await
}

pub(super) async fn delete_one(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<()> {
    let search = load(db, id, org_id).await?;
    sqlx::query("delete from saved_searches where id = $1").bind(search.id).execute(db).await?;
    Ok(())
}

#[derive(Deserialize)]
pub(super) struct SinceQuery {
    pub(super) since: Option<Ts>,
}

#[derive(Serialize, FromRow)]
pub(super) struct NewMatch {
    #[serde(flatten)]
    #[sqlx(flatten)]
    company: Listing,
    found_at: Ts,
}

#[derive(Serialize)]
pub(super) struct NewMatches {
    /// What the companies were found since, or null when none are new.
    since: Option<Ts>,
    #[serde(flatten)]
    page: Paginated<NewMatch>,
}

/// The companies the search found at or after `since`, newest first. Without `since`, the ones found since it was last
/// opened in the app.
pub(super) async fn matches_of(db: &PgPool, search: &SavedSearch, since: Option<Ts>, page: &PageQuery) -> AppResult<NewMatches> {
    let since: Option<Ts> = match since {
        Some(since) => Some(since),
        None => sqlx::query_scalar("select min(found_at) from saved_search_matches where search_id = $1 and found_at > coalesce($2, '-infinity')")
            .bind(search.id)
            .bind(search.opened_at)
            .fetch_one(db)
            .await?,
    };
    let Some(since) = since else { return Ok(NewMatches { since: None, page: Paginated { items: Vec::new(), total: 0 } }) };
    const FROM: &str = "from saved_search_matches m join directory_companies d on d.domain = m.domain where m.search_id = $1 and m.found_at >= $2";
    let total: i64 = sqlx::query_scalar(&format!("select count(*) {FROM}")).bind(search.id).bind(since).fetch_one(db).await?;
    let items = sqlx::query_as(&format!("select {LISTING}, m.found_at {FROM} order by m.found_at desc, d.confidence desc, d.domain limit $3 offset $4"))
        .bind(search.id)
        .bind(since)
        .bind(page.limit())
        .bind(page.offset())
        .fetch_all(db)
        .await?;
    Ok(NewMatches { since: Some(since), page: Paginated { items, total } })
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<SearchRow>>> {
    auth.project(&state.db, id).await?;
    Ok(Json(list_of(&state.db, id).await?))
}

async fn create(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<NewSearch>) -> AppResult<Json<SearchRow>> {
    auth.project(&state.db, id).await?;
    Ok(Json(create_in(&state, id, Some(auth.user_id), body).await?))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SearchChanges {
    name: Option<String>,
    chat_alerts: Option<bool>,
    /// A campaign to add new matches to, or null to stop.
    #[serde(default, deserialize_with = "crate::deals::nullable")]
    segment_id: Option<Option<Uuid>>,
}

/// Renames it and sets its alerts. Its filters stay as saved, since its matches so far are of those.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SearchChanges>) -> AppResult<Json<SearchRow>> {
    let search = load(&state.db, id, auth.org_id).await?;
    let name = body.name.as_deref().map(name).transpose()?;
    if let Some(segment_id) = body.segment_id {
        check_segment(&state, search.project_id, segment_id).await?;
    }
    sqlx::query(
        "update saved_searches set name = coalesce($2, name), chat_alerts = coalesce($3, chat_alerts),
            add_to_segment_id = case when $4 then $5 else add_to_segment_id end
         where id = $1",
    )
    .bind(search.id)
    .bind(name)
    .bind(body.chat_alerts)
    .bind(body.segment_id.is_some())
    .bind(body.segment_id.flatten())
    .execute(&state.db)
    .await?;
    Ok(Json(row(&state.db, search.id).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<serde_json::Value>> {
    delete_one(&state.db, id, auth.org_id).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Opening the search in the app. When its owner opens it, its new matches count from now.
async fn new_matches(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<SinceQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<NewMatches>> {
    let search = load(&state.db, id, auth.org_id).await?;
    let found = matches_of(&state.db, &search, q.since, &page).await?;
    if search.user_id.is_none_or(|owner| owner == auth.user_id) {
        sqlx::query("update saved_searches set opened_at = now() where id = $1").bind(search.id).execute(&state.db).await?;
    }
    Ok(Json(found))
}
