//! Search over the company database that `splee import-companies` loads.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/directory/companies", get(search))
        .route("/directory/countries", get(countries))
        .route("/projects/{id}/directory/add", post(add))
}

/// Counting every match of a broad search, like all restaurants in the US, is slow, so the count stops here.
const COUNT_CAP: i64 = 10_000;

const FILTER: &str = "from directory_companies
    where ($1::text is null or search @@ to_tsquery('simple', $1))
      and ($2::text is null or country_code = $2)
      and ($3::text is null or lower(city) = lower($3))
      and ($4::text is null or categories @> array[$4])
      and (not $5 or email is not null)
      and (not $6 or phone is not null)";

#[derive(Deserialize)]
struct SearchQuery {
    q: Option<String>,
    country: Option<String>,
    city: Option<String>,
    category: Option<String>,
    #[serde(default)]
    has_email: bool,
    #[serde(default)]
    has_phone: bool,
}

#[derive(Serialize, sqlx::FromRow)]
struct Company {
    domain: String,
    name: String,
    website: String,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    city: Option<String>,
    region: Option<String>,
    postcode: Option<String>,
    country_code: String,
    category: Option<String>,
}

#[derive(Serialize)]
struct Page {
    items: Vec<Company>,
    /// The number of matches, at most COUNT_CAP. `capped` says there are more.
    total: i64,
    capped: bool,
}

/// Companies whose name or category has words starting with each word in `q` and the filters, the most reliable listings first.
async fn search(State(state): State<AppState>, _auth: Auth, Query(q): Query<SearchQuery>, Query(page): Query<PageQuery>) -> AppResult<Json<Page>> {
    let blank = |s: Option<String>| s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let country = blank(q.country).map(|c| c.to_uppercase());
    if country.as_ref().is_some_and(|c| c.len() != 2 || !c.chars().all(|c| c.is_ascii_alphabetic())) {
        return Err(AppError::BadRequest("Pick a country by its two-letter code, like GB".into()));
    }
    let (words, city) = (q.q.as_deref().and_then(prefixes), blank(q.city));
    let category = blank(q.category).map(|c| c.to_lowercase().replace(' ', "_"));
    let total: i64 = sqlx::query_scalar(&format!("select count(*) from (select 1 {FILTER} limit {COUNT_CAP} + 1) matches"))
        .bind(&words)
        .bind(&country)
        .bind(&city)
        .bind(&category)
        .bind(q.has_email)
        .bind(q.has_phone)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Company>(&format!(
        "select domain, name, website, phone, email, address, city, region, postcode, country_code, category {FILTER}
         order by confidence desc, domain limit $7 offset $8"
    ))
    .bind(&words)
    .bind(&country)
    .bind(&city)
    .bind(&category)
    .bind(q.has_email)
    .bind(q.has_phone)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Page { items, total: total.min(COUNT_CAP), capped: total > COUNT_CAP }))
}

#[derive(Serialize, sqlx::FromRow)]
struct Country {
    country_code: String,
    companies: i64,
}

/// Grouping millions of rows takes seconds, and the list only changes when someone runs an import, so it's kept for a while.
const COUNTRIES_TTL: Duration = Duration::from_secs(10 * 60);
static COUNTRIES: Mutex<Option<(Instant, Arc<Vec<Country>>)>> = Mutex::new(None);

/// The countries that have companies, most companies first.
async fn countries(State(state): State<AppState>, _auth: Auth) -> AppResult<Json<Arc<Vec<Country>>>> {
    if let Some((at, list)) = COUNTRIES.lock().expect("not poisoned").as_ref()
        && at.elapsed() < COUNTRIES_TTL
    {
        return Ok(Json(list.clone()));
    }
    let list = Arc::new(
        sqlx::query_as::<_, Country>("select country_code, count(*) as companies from directory_companies group by 1 order by 2 desc, 1")
            .fetch_all(&state.db)
            .await?,
    );
    // An empty table isn't kept, so the page sees a first import right away instead of after the wait.
    if !list.is_empty() {
        *COUNTRIES.lock().expect("not poisoned") = Some((Instant::now(), list.clone()));
    }
    Ok(Json(list))
}

const MAX_ADD: usize = 500;

#[derive(Deserialize)]
struct AddBody {
    segment_id: Uuid,
    domains: Vec<String>,
}

#[derive(Serialize)]
struct Added {
    added: u64,
    skipped: u64,
}

/// Adds companies from the database to a campaign as new companies, where "Find people" picks them up like the ones the agent
/// finds. Companies already in the project, and ones whose domain or address is on the do-not-contact list, are skipped.
async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Added>> {
    auth.project(&state.db, id).await?;
    check_segment(&state, id, Some(body.segment_id)).await?;
    if body.domains.len() > MAX_ADD {
        return Err(AppError::BadRequest(format!("Add at most {MAX_ADD} companies at a time")));
    }
    let domains: Vec<String> = body.domains.iter().filter_map(|d| util_input::domain(d)).collect::<HashSet<_>>().into_iter().collect();
    if domains.is_empty() {
        return Err(AppError::BadRequest("Pick at least one company".into()));
    }
    let added = add_companies(&state.db, auth.org_id, id, body.segment_id, &domains).await?;
    Ok(Json(Added { added, skipped: domains.len() as u64 - added }))
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

/// Every word as a prefix, so "dent" finds dental_clinic and general_dentistry, as a tsquery. Only letters and digits get
/// through, so a query can't hold tsquery syntax.
fn prefixes(q: &str) -> Option<String> {
    let words: Vec<String> = q.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(|w| format!("{}:*", w.to_lowercase())).collect();
    (!words.is_empty()).then(|| words.join(" & "))
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

    #[test]
    fn a_search_becomes_word_prefixes() {
        assert_eq!(super::prefixes("Dentist  London"), Some("dentist:* & london:*".into()));
        assert_eq!(super::prefixes("o'brien & sons | !x"), Some("o:* & brien:* & sons:* & x:*".into()));
        assert_eq!(super::prefixes(" &|! "), None);
    }
}
