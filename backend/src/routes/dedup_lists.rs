//! Dedup lists: named lists of company domains, like a CRM's accounts, current customers or a competitor's clients, that the
//! company database leaves out of its search, count, export, saved searches and lookalikes (directory.rs, lookalikes.rs).
//! They only hide search results. Unlike suppression_lists.rs they never stop an email, an import or adding a company.

use std::collections::HashSet;

use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use super::imports;
use super::suppression_lists::{self, Kind, ScopeBody};
use super::util_input;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::DedupList;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/dedup/companies", get(list).post(upload))
        .route("/dedup/companies/{id}", get(show).patch(rescope).delete(remove))
        .route("/projects/{id}/dedup/hidden", get(hidden_here))
        .layer(DefaultBodyLimit::max(20 * 1024 * 1024))
}

/// SQL that's true when one of org `org`'s lists covering project `project` has `domain`. A null project is covered only by
/// the lists for every project. Each argument is an SQL expression, `domain` a lowercased one of type text.
pub fn hidden(org: &str, project: &str, domain: &str) -> String {
    format!(
        "exists (select 1 from dedup_list_entries de join dedup_lists dl on dl.id = de.list_id
                 where de.org_id = {org} and de.domain = {domain} and (dl.project_ids is null or {project} = any(dl.project_ids)))"
    )
}

const VIEW: &str = "select l.id, l.name::text as name,
            case when l.project_ids is not null then array(select p.id from projects p where p.id = any(l.project_ids) order by p.created_at) end
              as project_ids,
            l.created_by, l.created_at, (select count(*) from dedup_list_entries e where e.list_id = l.id) as size
     from dedup_lists l where l.org_id = $1 and ($2::uuid is null or l.id = $2) order by l.created_at desc";

/// The org's lists, newest first.
pub async fn all(db: &PgPool, org_id: Uuid) -> sqlx::Result<Vec<DedupList>> {
    sqlx::query_as::<_, DedupList>(VIEW).bind(org_id).bind(None::<Uuid>).fetch_all(db).await
}

async fn one(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<DedupList> {
    sqlx::query_as::<_, DedupList>(VIEW).bind(org_id).bind(id).fetch_optional(db).await?.ok_or(AppError::NotFound)
}

/// Takes turns per org, so two uploads with the same name can't both pick the same free one.
async fn lock_names(conn: &mut PgConnection, org_id: Uuid) -> sqlx::Result<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('dedup-lists:' || $1::text, 0))").bind(org_id).execute(conn).await?;
    Ok(())
}

/// Makes a list named `name`, or "name (2)" and on when the org has one by that name, so an upload never merges into another.
async fn create(conn: &mut PgConnection, org_id: Uuid, name: &str, scope: Option<Vec<Uuid>>, by: Option<Uuid>) -> sqlx::Result<Uuid> {
    lock_names(conn, org_id).await?;
    let taken: HashSet<String> =
        sqlx::query_scalar("select lower(name::text) from dedup_lists where org_id = $1").bind(org_id).fetch_all(&mut *conn).await?.into_iter().collect();
    let free = std::iter::once(name.to_string())
        .chain((2..).map(|n| format!("{name} ({n})")))
        .find(|n| !taken.contains(&n.to_lowercase()))
        .unwrap_or_default();
    sqlx::query_scalar("insert into dedup_lists (org_id, name, project_ids, created_by) values ($1, $2, $3, $4) returning id")
        .bind(org_id)
        .bind(free)
        .bind(scope)
        .bind(by)
        .fetch_one(conn)
        .await
}

/// Adds the valid domains, from URLs too, and returns how many were new and how many were skipped as invalid or already on
/// it. Blank cells don't count.
async fn add_domains<'a>(conn: &mut PgConnection, list_id: Uuid, org_id: Uuid, raws: impl IntoIterator<Item = &'a str>) -> sqlx::Result<(u64, u64)> {
    let mut given = 0;
    let mut domains = Vec::new();
    for raw in raws.into_iter().map(str::trim).filter(|r| !r.is_empty()) {
        given += 1;
        domains.extend(util_input::domain(raw));
    }
    let added = sqlx::query("insert into dedup_list_entries (list_id, org_id, domain) select $1, $2, v from unnest($3::text[]) v on conflict do nothing")
        .bind(list_id)
        .bind(org_id)
        .bind(&domains)
        .execute(conn)
        .await?
        .rows_affected();
    Ok((added, given - added))
}

#[derive(Serialize)]
pub struct Added {
    pub list: DedupList,
    pub added: u64,
    pub skipped: u64,
}

/// For the public API: adds to the org's list called `name`, making it when there's none, so a CRM can sync in batches.
/// `scope` left out keeps an existing list's projects and covers all of them for a new one.
pub async fn add_by_name<'a>(
    db: &PgPool,
    org_id: Uuid,
    name: &str,
    scope: Option<Option<Vec<Uuid>>>,
    raws: impl IntoIterator<Item = &'a str>,
) -> AppResult<Added> {
    let name = suppression_lists::check_name(name)?;
    let scope = match scope {
        Some(ids) => Some(suppression_lists::check_scope(db, org_id, ids).await?),
        None => None,
    };
    let mut tx = db.begin().await?;
    lock_names(&mut tx, org_id).await?;
    let existing: Option<Uuid> =
        sqlx::query_scalar("select id from dedup_lists where org_id = $1 and name = $2::citext").bind(org_id).bind(name).fetch_optional(&mut *tx).await?;
    let id = match existing {
        Some(id) => {
            if let Some(scope) = &scope {
                sqlx::query("update dedup_lists set project_ids = $2 where id = $1").bind(id).bind(scope).execute(&mut *tx).await?;
            }
            id
        }
        None => create(&mut tx, org_id, name, scope.flatten(), None).await?,
    };
    let (added, skipped) = add_domains(&mut tx, id, org_id, raws).await?;
    tx.commit().await?;
    Ok(Added { list: one(db, org_id, id).await?, added, skipped })
}

/// Points a list at other projects. `ids` None covers all of them.
pub async fn set_scope(db: &PgPool, org_id: Uuid, id: Uuid, ids: Option<Vec<Uuid>>) -> AppResult<DedupList> {
    let scope = suppression_lists::check_scope(db, org_id, ids).await?;
    let updated = sqlx::query("update dedup_lists set project_ids = $3 where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .bind(scope)
        .execute(db)
        .await?
        .rows_affected();
    if updated == 0 {
        return Err(AppError::NotFound);
    }
    one(db, org_id, id).await
}

/// Deletes a list and its domains, which show up in searches again right away.
pub async fn delete(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<()> {
    let deleted = sqlx::query("delete from dedup_lists where id = $1 and org_id = $2").bind(id).bind(org_id).execute(db).await?.rows_affected();
    if deleted == 0 {
        return Err(AppError::NotFound);
    }
    Ok(())
}

#[derive(Serialize)]
pub struct Contents {
    #[serde(flatten)]
    pub list: DedupList,
    /// `limit` of its domains from `offset`, in alphabetical order. `size` is how many there are.
    pub domains: Vec<String>,
}

#[derive(Deserialize)]
pub struct OffsetQuery {
    limit: Option<i64>,
    offset: Option<i64>,
}

/// A list with a page of its domains, 100 unless `limit` asks for 1 to 1,000.
pub async fn contents(db: &PgPool, org_id: Uuid, id: Uuid, q: &OffsetQuery) -> AppResult<Contents> {
    let list = one(db, org_id, id).await?;
    let domains = sqlx::query_scalar("select domain from dedup_list_entries where list_id = $1 order by domain limit $2 offset $3")
        .bind(id)
        .bind(q.limit.unwrap_or(100).clamp(1, 1000))
        .bind(q.offset.unwrap_or(0).max(0))
        .fetch_all(db)
        .await?;
    Ok(Contents { list, domains })
}

#[derive(Deserialize)]
struct UploadBody {
    name: String,
    /// A CSV to read the domains from, or `domains` pasted in.
    csv: Option<String>,
    /// Which column of `csv` to read, by position. Left out, the one with the most domains.
    column: Option<usize>,
    #[serde(default)]
    domains: Vec<String>,
    /// Null covers every project.
    project_ids: Option<Vec<Uuid>>,
}

/// A new list from a CSV's column of domains or website addresses, or from pasted ones. A name the org already uses gets
/// " (2)" and on.
async fn upload(State(state): State<AppState>, auth: Auth, Json(body): Json<UploadBody>) -> AppResult<Json<Added>> {
    let name = suppression_lists::check_name(&body.name)?;
    let scope = suppression_lists::check_scope(&state.db, auth.org_id, body.project_ids).await?;
    let csv = body.csv.as_deref().map(|c| imports::parse_csv(c.as_bytes())).transpose()?;
    let raws: Vec<&str> = match &csv {
        Some(csv) => suppression_lists::column_of(csv, Kind::Companies, body.column)
            .ok_or_else(|| AppError::BadRequest("The file has no column of domains or websites".into()))?
            .collect(),
        None => body.domains.iter().map(String::as_str).collect(),
    };
    if raws.iter().all(|r| r.trim().is_empty()) {
        return Err(AppError::BadRequest("Add at least one domain".into()));
    }
    let mut tx = state.db.begin().await?;
    let id = create(&mut tx, auth.org_id, name, scope, Some(auth.user_id)).await?;
    let (added, skipped) = add_domains(&mut tx, id, auth.org_id, raws).await?;
    tx.commit().await?;
    Ok(Json(Added { list: one(&state.db, auth.org_id, id).await?, added, skipped }))
}

/// How many companies in the database the project's searches leave out.
pub async fn hidden_in(db: &PgPool, org_id: Uuid, project_id: Uuid) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "select count(distinct de.domain) from dedup_list_entries de join dedup_lists dl on dl.id = de.list_id
         where de.org_id = $1 and (dl.project_ids is null or $2 = any(dl.project_ids))
           and exists (select 1 from directory_companies d where d.domain = de.domain)",
    )
    .bind(org_id)
    .bind(project_id)
    .fetch_one(db)
    .await
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<DedupList>>> {
    Ok(Json(all(&state.db, auth.org_id).await?))
}

async fn show(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Query(q): Query<OffsetQuery>) -> AppResult<Json<Contents>> {
    Ok(Json(contents(&state.db, auth.org_id, id, &q).await?))
}

async fn rescope(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ScopeBody>) -> AppResult<Json<DedupList>> {
    Ok(Json(set_scope(&state.db, auth.org_id, id, body.into_scope()?).await?))
}

async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    delete(&state.db, auth.org_id, id).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn hidden_here(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.project(&state.db, id).await?;
    Ok(Json(json!({ "hidden": hidden_in(&state.db, auth.org_id, id).await? })))
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::extract::{Json, State};
    use futures::TryStreamExt;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::UploadBody;
    use crate::auth::Auth;
    use crate::crypto::sha256_hex;
    use crate::directory;
    use crate::models::Lead;
    use crate::routes::directory::SearchQuery;
    use crate::routes::util_pagination::PageQuery;

    const KEY_A: &str = "splee_dedup_key_a";
    const KEY_B: &str = "splee_dedup_key_b";

    struct Org {
        id: Uuid,
        projects: [Uuid; 2],
    }

    /// Acme with KEY_A and Other with KEY_B, two projects each, and three dental clinics in Leeds, the most reliable first.
    async fn seed(db: &PgPool) -> (Org, Org) {
        let mut orgs = vec![];
        for (name, key) in [("Acme", KEY_A), ("Other", KEY_B)] {
            let id: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
            let mut projects = [Uuid::nil(); 2];
            for (i, p) in projects.iter_mut().enumerate() {
                *p = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id")
                    .bind(id)
                    .bind(format!("{name} {i}"))
                    .fetch_one(db)
                    .await
                    .unwrap();
            }
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'CRM', 'splee_dedup_', $2)")
                .bind(id)
                .bind(sha256_hex(key))
                .execute(db)
                .await
                .unwrap();
            orgs.push(Org { id, projects });
        }
        for (domain, confidence) in [("smile.co.uk", 0.9), ("grin.co.uk", 0.8), ("cafe.co.uk", 0.7)] {
            sqlx::query(
                "insert into directory_companies (domain, name, website, email, city, country_code, category, categories, confidence, source_id, release)
                 values ($1, $1, 'https://' || $1, 'hi@' || $1, 'Leeds', 'GB', 'dental_clinic', '{dental_clinic}', $2, 'x', 'r')",
            )
            .bind(domain)
            .bind(confidence)
            .execute(db)
            .await
            .unwrap();
        }
        let b = orgs.pop().unwrap();
        (orgs.pop().unwrap(), b)
    }

    fn dentists(project_id: Option<Uuid>) -> SearchQuery {
        SearchQuery { category: Some("dental_clinic".into()), project_id, ..SearchQuery::default() }
    }

    /// The domains the page, the count, the download and its count find, which must agree.
    async fn found(db: &PgPool, org: Uuid, q: &SearchQuery) -> Vec<String> {
        let page = crate::routes::directory::page_of(db, org, q, &PageQuery { page: None, per_page: None }).await.unwrap();
        let json = serde_json::to_value(&page).unwrap();
        let listed: Vec<String> = json["items"].as_array().unwrap().iter().map(|c| c["domain"].as_str().unwrap().to_string()).collect();
        assert_eq!(json["total"], json!(listed.len()), "the count leaves out what the page does");
        let exported: Vec<String> = directory::export(db.clone(), q.search(Some(org)).unwrap(), 100, 0).map_ok(|e| e.domain).try_collect().await.unwrap();
        assert_eq!(exported, listed, "the download leaves out what the page does");
        assert_eq!(directory::count_from(db, &q.search(Some(org)).unwrap(), 0, 100).await.unwrap(), listed.len() as i64);
        listed
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn lists_hide_companies_from_searches_in_the_projects_they_cover_until_deleted(db: PgPool) {
        let (a, b) = seed(&db).await;
        let [first, second] = a.projects;
        let crm = super::add_by_name(&db, a.id, "CRM accounts", Some(Some(vec![first])), ["https://www.Smile.co.uk/about"]).await.unwrap();
        assert_eq!((crm.added, crm.skipped), (1, 0));
        super::add_by_name(&db, a.id, "Competitor clients", None, ["GRIN.co.uk"]).await.unwrap();
        // Another org's list covers all its projects. It never applies to Acme.
        super::add_by_name(&db, b.id, "Theirs", None, ["cafe.co.uk"]).await.unwrap();

        assert_eq!(found(&db, a.id, &dentists(Some(first))).await, ["cafe.co.uk"]);
        assert_eq!(found(&db, a.id, &dentists(Some(second))).await, ["smile.co.uk", "cafe.co.uk"], "the CRM list covers the first project only");
        assert_eq!(found(&db, a.id, &dentists(None)).await, ["smile.co.uk", "cafe.co.uk"], "without a project, only lists for every project");
        let every = SearchQuery { include_hidden: true, ..dentists(Some(first)) };
        assert_eq!(found(&db, a.id, &every).await, ["smile.co.uk", "grin.co.uk", "cafe.co.uk"]);
        assert_eq!(found(&db, b.id, &dentists(Some(b.projects[0]))).await, ["smile.co.uk", "grin.co.uk"]);
        assert_eq!((super::hidden_in(&db, a.id, first).await.unwrap(), super::hidden_in(&db, a.id, second).await.unwrap()), (2, 1));

        // A saved search records only what the search shows, and a deleted list's companies are new matches on the next check.
        let saved: crate::models::SavedSearch = sqlx::query_as(
            "insert into saved_searches (project_id, name, q, country, city, category, has_email, has_phone, tech)
             values ($1, 'Dentists', '', '', '', 'dental_clinic', false, false, '') returning *",
        )
        .bind(first)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!(crate::saved_searches::check(&db, &saved).await.unwrap(), None);
        let matched = || {
            sqlx::query_scalar::<_, String>("select domain from saved_search_matches where search_id = $1 order by domain").bind(saved.id).fetch_all(&db)
        };
        assert_eq!(matched().await.unwrap(), ["cafe.co.uk"]);
        assert!(matches!(super::delete(&db, b.id, crm.list.id).await, Err(crate::error::AppError::NotFound)), "another org can't delete it");
        super::delete(&db, a.id, crm.list.id).await.unwrap();
        assert_eq!(found(&db, a.id, &dentists(Some(first))).await, ["smile.co.uk", "cafe.co.uk"]);
        let saved: crate::models::SavedSearch = sqlx::query_as("select * from saved_searches where id = $1").bind(saved.id).fetch_one(&db).await.unwrap();
        let check = crate::saved_searches::check(&db, &saved).await.unwrap().unwrap();
        assert_eq!(check.new, 1);
        assert_eq!(matched().await.unwrap(), ["cafe.co.uk", "smile.co.uk"]);
        let (pool, search, since) = (&db, &saved, check.at);
        let new = move || async move {
            let page = PageQuery { page: None, per_page: None };
            let m = crate::routes::saved_searches::matches_of(pool, search, Some(since), &page).await.unwrap();
            serde_json::to_value(&m).unwrap()["total"].clone()
        };
        assert_eq!(new().await, json!(1));
        // Listed again after it was found, it drops out of the search's new matches too.
        super::add_by_name(&db, a.id, "CRM accounts", None, ["smile.co.uk"]).await.unwrap();
        assert_eq!(new().await, json!(0));

        // Sending and adding to a campaign never look at the lists.
        let lead: Lead = sqlx::query_as(
            "insert into leads (project_id, email, company_domain, status) values ($1, 'ana@grin.co.uk', 'grin.co.uk', 'queued') returning *",
        )
            .bind(first)
            .fetch_one(&db)
            .await
            .unwrap();
        assert!(!crate::mail::sender::suppressed(&db, a.id, &lead, "ana@grin.co.uk").await.unwrap());
        let segment: Uuid =
            sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id").bind(first).fetch_one(&db).await.unwrap();
        let added = crate::routes::directory::add_domains(&db, a.id, first, segment, &["grin.co.uk".to_string()]).await.unwrap();
        assert_eq!(added.added, 1);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn an_upload_reads_the_domain_column_or_pasted_domains(db: PgPool) {
        let (a, b) = seed(&db).await;
        let user: Uuid =
            sqlx::query_scalar("insert into users (email, password_hash) values ('jo@acme.example', 'x') returning id").fetch_one(&db).await.unwrap();
        let state = crate::autopilot::tests::state(db.clone());
        let auth = |org_id| Auth { user_id: user, org_id, role: "owner".into(), token_hash: String::new() };
        let upload = |org_id: Uuid, name: &str, csv: Option<&str>, domains: &[&str]| {
            let body = UploadBody {
                name: name.into(),
                csv: csv.map(String::from),
                column: None,
                domains: domains.iter().map(|d| d.to_string()).collect(),
                project_ids: None,
            };
            super::upload(State(state.clone()), auth(org_id), Json(body))
        };

        let csv = "Account,Owner email,Website\nAcme,jo@acme.example,https://www.Acme.example/pricing\nGlobex,,globex.example\nAgain,,ACME.example\n";
        let Json(made) = upload(a.id, "CRM accounts", Some(csv), &[]).await.unwrap();
        assert_eq!((made.list.name.as_str(), made.list.size, made.added, made.skipped), ("CRM accounts", 2, 2, 1));
        assert_eq!(made.list.created_by, Some(user));
        let Json(pasted) = upload(a.id, "crm accounts", None, &["www.initech.example", "nope", ""]).await.unwrap();
        assert_eq!((pasted.list.name.as_str(), pasted.added, pasted.skipped), ("crm accounts (2)", 1, 1));
        let first_page = super::OffsetQuery { limit: None, offset: None };
        assert_eq!(super::contents(&db, a.id, made.list.id, &first_page).await.unwrap().domains, ["acme.example", "globex.example"]);
        assert!(matches!(super::contents(&db, b.id, made.list.id, &first_page).await, Err(crate::error::AppError::NotFound)));
        assert!(upload(a.id, "Emails", Some("Email\njo@acme.example\n"), &[]).await.is_err(), "a file with no domains");
        assert!(upload(a.id, "Nothing", None, &[" "]).await.is_err());
        assert_eq!(super::all(&db, b.id).await.unwrap().len(), 0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_api_manages_lists_for_its_own_org_only(db: PgPool) {
        let (a, b) = seed(&db).await;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let state = crate::autopilot::tests::state(db.clone());
        let app = Router::new().nest("/api", super::super::api_v1::router(&state)).with_state(state);
        tokio::spawn(async move { axum::serve(listener, app).await });
        let client = reqwest::Client::new();
        let call = |method: reqwest::Method, key: &str, path: &str, body: Option<Value>| {
            let mut req = client.request(method, format!("{base}{path}")).bearer_auth(key);
            if let Some(body) = body {
                req = req.json(&body);
            }
            async move {
                let res = req.send().await.unwrap();
                (res.status().as_u16(), res.json::<Value>().await.unwrap_or(Value::Null))
            }
        };
        let post = |body: Value| call(reqwest::Method::POST, KEY_A, "/v1/dedup/companies", Some(body));

        assert_eq!(post(json!({"name": "CRM", "domains": ["smile.co.uk"], "project_ids": []})).await.0, 422);
        assert_eq!(post(json!({"name": "CRM", "domains": ["smile.co.uk"], "project_ids": [b.projects[0]]})).await.0, 422);
        let body = json!({"name": "CRM", "domains": ["https://www.SMILE.co.uk/", "smile.co.uk", "not a domain"], "project_ids": [a.projects[0]]});
        let (status, made) = post(body).await;
        assert_eq!((status, made["added"].clone(), made["skipped"].clone()), (200, json!(1), json!(2)), "{made}");
        assert_eq!((made["list"]["size"].clone(), made["list"]["project_ids"].clone()), (json!(1), json!([a.projects[0]])));
        let id = made["list"]["id"].as_str().unwrap().to_string();
        // The same name adds to that list and keeps its projects, so a CRM can sync in batches.
        let (_, more) = post(json!({"name": "crm", "domains": ["grin.co.uk"]})).await;
        assert_eq!((more["list"]["id"].as_str(), more["list"]["size"].clone()), (Some(id.as_str()), json!(2)));
        assert_eq!(more["list"]["project_ids"], json!([a.projects[0]]));

        let search = format!("/v1/companies?category=dental_clinic&project_id={}", a.projects[0]);
        let (_, page) = call(reqwest::Method::GET, KEY_A, &search, None).await;
        assert_eq!((page["total"].clone(), page["items"][0]["domain"].clone()), (json!(1), json!("cafe.co.uk")), "{page}");
        let (_, page) = call(reqwest::Method::GET, KEY_A, &format!("{search}&include_hidden=true"), None).await;
        assert_eq!(page["total"], json!(3));

        // A CRM's accounts come back in their order, cleaned up and counted once, minus what the project's lists hide.
        fn found(page: &Value) -> Vec<&str> {
            page["items"].as_array().unwrap().iter().map(|c| c["domain"].as_str().unwrap()).collect()
        }
        let lookup = |key: &str, body: Value| call(reqwest::Method::POST, key, "/v1/companies/by-domains", Some(body));
        let domains = json!(["https://www.Grin.co.uk/team", "cafe.co.uk", "nowhere.example", "CAFE.co.uk", "smile.co.uk"]);
        let (status, page) = lookup(KEY_A, json!({"domains": domains, "project_id": a.projects[0]})).await;
        assert_eq!((status, found(&page)), (200, vec!["cafe.co.uk"]), "{page}");
        assert_eq!(page["not_found"], json!(["grin.co.uk", "nowhere.example", "smile.co.uk"]));
        let (_, page) = lookup(KEY_A, json!({"domains": domains, "project_id": a.projects[0], "include_hidden": true})).await;
        assert_eq!((found(&page), page["not_found"].clone()), (vec!["grin.co.uk", "cafe.co.uk", "smile.co.uk"], json!(["nowhere.example"])));
        assert_eq!(found(&lookup(KEY_B, json!({"domains": domains})).await.1).len(), 3, "A's lists hide nothing from B");
        let (status, error) = lookup(KEY_A, json!({"domains": ["cafe.co.uk", "not a domain"]})).await;
        assert_eq!((status, error["error"].clone()), (400, json!("\"not a domain\" isn't a domain")));
        assert_eq!(lookup(KEY_A, json!({"domains": []})).await.0, 400);
        let too_many: Vec<String> = (0..1001).map(|i| format!("d{i}.example")).collect();
        assert_eq!(lookup(KEY_A, json!({ "domains": too_many })).await.0, 400);

        assert_eq!(call(reqwest::Method::GET, KEY_A, "/v1/dedup/companies", None).await.1.as_array().map(Vec::len), Some(1));
        assert_eq!(call(reqwest::Method::GET, KEY_B, "/v1/dedup/companies", None).await, (200, json!([])));
        let one = format!("/v1/dedup/companies/{id}");
        let (status, shown) = call(reqwest::Method::GET, KEY_A, &format!("{one}?limit=1&offset=1"), None).await;
        assert_eq!((status, shown["name"].clone(), shown["size"].clone()), (200, json!("CRM"), json!(2)));
        assert_eq!(shown["domains"], json!(["smile.co.uk"]));
        assert_eq!(call(reqwest::Method::GET, KEY_B, &one, None).await.0, 404);
        assert_eq!(call(reqwest::Method::PATCH, KEY_A, &one, Some(json!({}))).await.0, 422, "project_ids is required");
        assert_eq!(call(reqwest::Method::PATCH, KEY_B, &one, Some(json!({"project_ids": null}))).await.0, 404);
        let (status, all) = call(reqwest::Method::PATCH, KEY_A, &one, Some(json!({"project_ids": null}))).await;
        assert_eq!((status, all["project_ids"].clone()), (200, Value::Null));
        let (_, page) = call(reqwest::Method::GET, KEY_A, "/v1/companies?category=dental_clinic", None).await;
        assert_eq!(page["total"], json!(1), "a list for every project applies without project_id");
        assert_eq!(call(reqwest::Method::DELETE, KEY_B, &one, None).await.0, 404);
        assert_eq!(call(reqwest::Method::DELETE, KEY_A, &one, None).await, (200, json!({"ok": true})));
        assert_eq!(call(reqwest::Method::GET, KEY_A, &search, None).await.1["total"], json!(3));
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from dedup_list_entries").fetch_one(&db).await.unwrap(), 0);
    }
}
