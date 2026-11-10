//! Search over the company database, and the queue instance operators load countries into it with.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use super::imports::check_segment;
use super::util_input;
use super::util_pagination::PageQuery;
use crate::auth::Auth;
use crate::directory::{self, Country, Listing, Search};
use crate::directory_queue::{self, DirectoryImport};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/directory/companies", get(search))
        .route("/directory/countries", get(countries))
        .route("/directory/imports", get(imports).post(queue))
        .route("/projects/{id}/directory/add", post(add))
}

/// Counting every match of a broad search, like all restaurants in the US, is slow, so the count stops here.
const COUNT_CAP: i64 = 10_000;

#[derive(Deserialize)]
pub(super) struct SearchQuery {
    q: Option<String>,
    country: Option<String>,
    city: Option<String>,
    category: Option<String>,
    #[serde(default)]
    has_email: bool,
    #[serde(default)]
    has_phone: bool,
}

#[derive(Serialize)]
pub(super) struct Page {
    items: Vec<Listing>,
    /// The number of matches, at most COUNT_CAP. `capped` says there are more.
    total: i64,
    capped: bool,
}

/// Companies whose name or category has words starting with each word in `q` and the filters, the most reliable listings first.
async fn search(State(state): State<AppState>, _auth: Auth, Query(q): Query<SearchQuery>, Query(page): Query<PageQuery>) -> AppResult<Json<Page>> {
    Ok(Json(page_of(&state.db, &q, &page).await?))
}

pub(super) async fn page_of(db: &PgPool, q: &SearchQuery, page: &PageQuery) -> AppResult<Page> {
    let search = Search::new(q.q.as_deref(), q.country.as_deref(), q.city.as_deref(), q.category.as_deref(), q.has_email, q.has_phone)
        .map_err(AppError::BadRequest)?;
    let total = directory::count(db, &search, COUNT_CAP).await?;
    let items = directory::search(db, &search, page.limit(), page.offset()).await?;
    Ok(Page { items, total: total.min(COUNT_CAP), capped: total > COUNT_CAP })
}

/// The countries that have companies, most companies first.
async fn countries(State(state): State<AppState>, _auth: Auth) -> AppResult<Json<Arc<Vec<Country>>>> {
    Ok(Json(directory::countries(&state.db).await?))
}

/// The company database is shared by every organization, so only the people who run the instance (OPERATOR_EMAILS) load it.
async fn operator(state: &AppState, auth: &Auth) -> AppResult<()> {
    let email: String = sqlx::query_scalar("select email::text from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    if state.cfg.is_operator(&email) {
        Ok(())
    } else {
        Err(AppError::Forbidden("Only the people who run this Splee can load countries".into()))
    }
}

async fn imports(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<DirectoryImport>>> {
    operator(&state, &auth).await?;
    Ok(Json(directory_queue::list(&state.db).await?))
}

#[derive(Deserialize)]
struct QueueBody {
    country: String,
}

async fn queue(State(state): State<AppState>, auth: Auth, Json(body): Json<QueueBody>) -> AppResult<Json<DirectoryImport>> {
    operator(&state, &auth).await?;
    Ok(Json(directory_queue::queue(&state.db, &body.country, auth.user_id).await?))
}

const MAX_ADD: usize = 500;

#[derive(Deserialize)]
struct AddBody {
    segment_id: Uuid,
    domains: Vec<String>,
}

#[derive(Serialize)]
pub(super) struct Added {
    added: u64,
    skipped: u64,
}

/// Adds companies from the database to a campaign as new companies, where "Find people" picks them up like the ones the agent
/// finds. Companies already in the project, and ones whose domain or address is on the do-not-contact list, are skipped.
async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Added>> {
    auth.project(&state.db, id).await?;
    check_segment(&state, id, Some(body.segment_id)).await?;
    Ok(Json(add_domains(&state.db, auth.org_id, id, body.segment_id, &body.domains).await?))
}

/// The caller checks that the campaign belongs to the project and the project to the organization.
pub(super) async fn add_domains(db: &PgPool, org_id: Uuid, project_id: Uuid, segment_id: Uuid, domains: &[String]) -> AppResult<Added> {
    if domains.len() > MAX_ADD {
        return Err(AppError::BadRequest(format!("Add at most {MAX_ADD} companies at a time")));
    }
    let domains: Vec<String> = domains.iter().filter_map(|d| util_input::domain(d)).collect::<HashSet<_>>().into_iter().collect();
    if domains.is_empty() {
        return Err(AppError::BadRequest("Pick at least one company".into()));
    }
    let added = add_companies(db, org_id, project_id, segment_id, &domains).await?;
    Ok(Added { added, skipped: domains.len() as u64 - added })
}

async fn add_companies(db: &PgPool, org_id: Uuid, project_id: Uuid, segment_id: Uuid, domains: &[String]) -> sqlx::Result<u64> {
    // The category is the closest thing to a description the database has. "Find people" shows it to the agent.
    let done = sqlx::query(
        "insert into companies (project_id, segment_id, domain, name, description, city, country_code, fit_reason, source_url)
         select $1, $2, d.domain, d.name, coalesce(upper(left(d.category, 1)) || replace(substr(d.category, 2), '_', ' '), ''),
                coalesce(d.city, ''), d.country_code, 'Picked from the company database', d.website
         from directory_companies d
         where d.domain = any($3)
           and not exists (select 1 from suppressions s where s.org_id = $4
                           and s.value in (d.domain, d.email, split_part(d.email, '@', 2)))
         on conflict (project_id, domain) do nothing",
    )
    .bind(project_id)
    .bind(segment_id)
    .bind(domains)
    .bind(org_id)
    .execute(db)
    .await?;
    Ok(done.rows_affected())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn adding_companies_skips_known_and_suppressed_ones(db: PgPool) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(&db).await.unwrap();
        let segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id").bind(project).fetch_one(&db).await.unwrap();
        for (domain, email, category) in [
            ("smile.co.uk", None, Some("dental_clinic")),
            ("known.co.uk", None, None),
            ("blocked.co.uk", None, None),
            ("mailblocked.co.uk", Some("hi@mailblocked.co.uk"), None),
        ] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, email, city, country_code, category, source_id, release)
                 values ($1, $1, 'https://' || $1, $2, 'Leeds', 'GB', $3, 'x', 'r')",
            )
            .bind(domain)
            .bind(email)
            .bind(category)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query("insert into companies (project_id, domain) values ($1, 'known.co.uk')").bind(project).execute(&db).await.unwrap();
        for (value, kind) in [("blocked.co.uk", "domain"), ("hi@mailblocked.co.uk", "email")] {
            sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, $2, $3, 'manual')")
                .bind(org)
                .bind(value)
                .bind(kind)
                .execute(&db)
                .await
                .unwrap();
        }

        let domains = ["smile.co.uk", "known.co.uk", "blocked.co.uk", "mailblocked.co.uk", "missing.co.uk"].map(String::from);
        assert_eq!(super::add_companies(&db, org, project, segment, &domains).await.unwrap(), 1);

        let (segment_id, description, status): (Option<Uuid>, String, String) =
            sqlx::query_as("select segment_id, description, status from companies where domain = 'smile.co.uk'").fetch_one(&db).await.unwrap();
        assert_eq!((segment_id, description.as_str(), status.as_str()), (Some(segment), "Dental clinic", "new"));
    }
}
