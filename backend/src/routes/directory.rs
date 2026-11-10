//! Search over the company database, the queue instance operators load countries into it with, and adding its companies, or
//! the people LinkedIn finds at them, to a project.

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
use super::util_lead_import::{self, NewLead};
use super::util_pagination::PageQuery;
use crate::auth::Auth;
use crate::directory::{self, Country, Listing, Search};
use crate::directory_queue::{self, DirectoryImport};
use crate::error::{AppError, AppResult};
use crate::social::{self, Failure};
use crate::state::AppState;
use crate::unipile::{PeopleSearch, Person, Unipile, any_title};

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/directory/companies", get(search))
        .route("/directory/countries", get(countries))
        .route("/directory/imports", get(imports).post(queue))
        .route("/projects/{id}/directory/add", post(add))
        .route("/projects/{id}/directory/people", post(find_people))
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

/// Each company is a LinkedIn search or two, a few seconds each, and the page waits for them all.
const MAX_PEOPLE_COMPANIES: usize = 10;
/// From the first page of LinkedIn's results, which lists the best matches first.
const PEOPLE_PER_COMPANY: usize = 5;
const DEFAULT_ROLES: [&str; 4] = ["Owner", "Founder", "CEO", "Managing Director"];

#[derive(Deserialize)]
struct PeopleBody {
    segment_id: Uuid,
    domains: Vec<String>,
}

#[derive(Serialize, Debug, PartialEq)]
pub(super) struct FoundPerson {
    name: String,
    title: String,
    company: String,
    linkedin_url: String,
}

/// Someone LinkedIn found who wasn't added, or a company it found nobody at, with an empty name.
#[derive(Serialize, Debug, PartialEq)]
pub(super) struct Skipped {
    name: String,
    company: String,
    reason: String,
}

#[derive(Serialize, Debug)]
pub(super) struct FoundPeople {
    added: Vec<FoundPerson>,
    skipped: Vec<Skipped>,
}

/// Searches LinkedIn for the campaign's buyer roles at each company and adds the people as leads without an email, through the
/// same import as a CSV, so people already in the project and companies on the do-not-contact list are skipped.
async fn find_people(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PeopleBody>) -> AppResult<Json<FoundPeople>> {
    auth.project(&state.db, id).await?;
    check_segment(&state, id, Some(body.segment_id)).await?;
    let accounts = social::accounts(&state.db, id).await?.filter(|a| !a.unipile_token_enc.is_empty());
    let accounts = accounts.ok_or_else(|| AppError::BadRequest("Connect LinkedIn through Unipile under Integrations first".into()))?;
    let unipile = accounts.unipile(&state.cfg.app_secret).map_err(|e| AppError::BadRequest(e.message()))?;
    Ok(Json(people_at(&state.db, &unipile, auth.org_id, id, body.segment_id, &body.domains).await?))
}

/// The caller checks that the campaign belongs to the project and the project to the organization.
pub(super) async fn people_at(db: &PgPool, unipile: &Unipile, org_id: Uuid, project_id: Uuid, segment_id: Uuid, domains: &[String]) -> AppResult<FoundPeople> {
    let mut wanted: Vec<String> = Vec::new();
    for domain in domains.iter().filter_map(|d| util_input::domain(d)) {
        if !wanted.contains(&domain) {
            wanted.push(domain);
        }
    }
    if wanted.is_empty() {
        return Err(AppError::BadRequest("Pick at least one company".into()));
    }
    if wanted.len() > MAX_PEOPLE_COMPANIES {
        return Err(AppError::BadRequest(format!("Find people at up to {MAX_PEOPLE_COMPANIES} companies at a time")));
    }
    let targeting: serde_json::Value = sqlx::query_scalar("select targeting from segments where id = $1").bind(segment_id).fetch_one(db).await?;
    let mut roles: Vec<String> = targeting["roles"].as_array().into_iter().flatten().filter_map(|r| r.as_str().map(String::from)).collect();
    if roles.iter().all(|r| r.trim().is_empty()) {
        roles = DEFAULT_ROLES.map(String::from).to_vec();
    }
    let title = any_title(&roles);
    let mut companies: Vec<(String, String, String)> =
        sqlx::query_as("select domain::text, name, country_code from directory_companies where domain = any($1)").bind(&wanted).fetch_all(db).await?;
    companies.sort_by_key(|(domain, ..)| wanted.iter().position(|w| w == domain));

    let (mut found, mut skipped): (Vec<(Person, String, String)>, Vec<Skipped>) = (Vec::new(), Vec::new());
    let mut stopped: Option<String> = None;
    for (domain, name, _) in &companies {
        let company_skipped = |reason: String| Skipped { name: String::new(), company: name.clone(), reason };
        if let Some(reason) = &stopped {
            skipped.push(company_skipped(reason.clone()));
            continue;
        }
        match unipile.search_people(db, &PeopleSearch { company: name.clone(), title: title.clone(), ..PeopleSearch::default() }, None).await {
            Ok(page) if page.people.is_empty() => skipped.push(company_skipped("LinkedIn found nobody there in these roles".into())),
            Ok(page) => found.extend(page.people.into_iter().take(PEOPLE_PER_COMPANY).map(|p| (p, name.clone(), domain.clone()))),
            Err(Failure::Post(reason)) => skipped.push(company_skipped(reason)),
            // The account can't search again, like at the daily cap, so the companies left wait for another day.
            Err(Failure::Account(reason)) if found.is_empty() && skipped.is_empty() => return Err(AppError::BadRequest(reason)),
            Err(Failure::Account(reason)) => {
                skipped.push(company_skipped(reason.clone()));
                stopped = Some(reason);
            }
        }
    }

    let country_of = |domain: &str| companies.iter().find(|(d, ..)| d == domain).map(|(.., c)| c.clone()).unwrap_or_default();
    let rows = found
        .iter()
        .enumerate()
        .map(|(i, (p, company, domain))| {
            let lead = NewLead {
                full_name: p.name.clone(),
                title: title_of(&p.headline),
                company_name: company.clone(),
                company_domain: domain.clone(),
                linkedin_url: p.linkedin_url.clone(),
                city: p.location.split_once(',').map(|(city, _)| city.trim().to_string()).unwrap_or_default(),
                country_code: country_of(domain),
                segment_id: Some(segment_id),
                linkedin_provider_id: p.provider_id.clone(),
                ..NewLead::default()
            };
            (i, lead)
        })
        .collect();
    let outcome = util_lead_import::insert(db, org_id, project_id, "linkedin", rows).await?;
    let mut added = Vec::new();
    for (i, (p, company, _)) in found.into_iter().enumerate() {
        match outcome.errors.iter().find(|e| e.row == i) {
            Some(e) => skipped.push(Skipped { name: p.name, company, reason: e.error.clone() }),
            None => added.push(FoundPerson { title: title_of(&p.headline), name: p.name, company, linkedin_url: p.linkedin_url }),
        }
    }
    Ok(FoundPeople { added, skipped })
}

/// A headline like "Owner at Smile Dental" reads as Owner. Others are kept whole, as they're often the title and more.
fn title_of(headline: &str) -> String {
    let title = headline.split_once(" at ").map_or(headline, |(title, _)| title).trim();
    title.chars().take(120).collect()
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

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn finding_people_adds_new_linkedin_leads_and_says_who_was_skipped(db: PgPool) {
        let (base, calls) = crate::unipile::fake::people().await;
        let unipile = crate::unipile::Unipile::new(&base, "TOKEN", "ACC").unwrap();
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(&db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(&db).await.unwrap();
        let segment: Uuid = sqlx::query_scalar(
            "insert into segments (project_id, name, targeting) values ($1, 'Dentists', '{\"roles\": [\"Owner\", \"Practice Manager\"]}') returning id",
        )
        .bind(project)
        .fetch_one(&db)
        .await
        .unwrap();
        for (domain, name) in [("smile.co.uk", "Smile Dental"), ("blocked.co.uk", "Blocked Dental"), ("nowhere.co.uk", "Nowhere Ltd")] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, city, country_code, source_id, release)
                 values ($1, $2, 'https://' || $1, 'Leeds', 'GB', 'x', 'r')",
            )
            .bind(domain)
            .bind(name)
            .execute(&db)
            .await
            .unwrap();
        }
        sqlx::query("insert into suppressions (org_id, value, kind, reason) values ($1, 'blocked.co.uk', 'domain', 'manual')").bind(org).execute(&db).await.unwrap();
        // The same person under another spelling of their link.
        sqlx::query("insert into leads (project_id, email, linkedin_url) values ($1, 'ana@smile.co.uk', 'https://linkedin.com/in/Ana-Diaz/')")
            .bind(project)
            .execute(&db)
            .await
            .unwrap();

        let domains = ["https://smile.co.uk", "blocked.co.uk", "nowhere.co.uk", "smile.co.uk"].map(String::from);
        let found = super::people_at(&db, &unipile, org, project, segment, &domains).await.unwrap();
        assert_eq!(found.added, [super::FoundPerson {
            name: "Sam Lee".into(),
            title: "Owner".into(),
            company: "Smile Dental".into(),
            linkedin_url: "https://www.linkedin.com/in/sam-lee".into(),
        }]);
        let skipped: Vec<(&str, &str, &str)> = found.skipped.iter().map(|s| (s.name.as_str(), s.company.as_str(), s.reason.as_str())).collect();
        assert_eq!(skipped, [
            ("", "Nowhere Ltd", "LinkedIn has no company page by that name"),
            ("Ana Diaz", "Smile Dental", "Already a lead"),
            ("Cy Doe", "Blocked Dental", "On the do-not-contact list"),
        ]);
        let searches = calls.lock().unwrap().clone();
        assert_eq!(searches.len(), 2);
        assert!(searches[0].contains(r#""advanced_keywords":{"title":"Owner OR \"Practice Manager\""}"#), "{}", searches[0]);

        let lead: (Option<String>, String, String, String, String, String, String, Option<Uuid>) = sqlx::query_as(
            "select email::text, source, title, company_domain::text, city, country_code, linkedin_provider_id, segment_id
             from leads where full_name = 'Sam Lee'",
        )
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(lead, (None, "linkedin".into(), "Owner".into(), "smile.co.uk".into(), "Leeds".into(), "GB".into(), "P-SAM".into(), Some(segment)));

        // Searching again finds the same people, who are leads now.
        let again = super::people_at(&db, &unipile, org, project, segment, &["smile.co.uk".to_string()]).await.unwrap();
        assert!(again.added.is_empty());
        assert_eq!(again.skipped.iter().map(|s| s.reason.as_str()).collect::<Vec<_>>(), ["Already a lead", "Already a lead"]);
        let leads: i64 = sqlx::query_scalar("select count(*) from leads where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        assert_eq!(leads, 2);
    }
}
