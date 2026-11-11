//! Website visitors: the public snippet and page view endpoints a project's site calls, and the Visitors tab's companies, the
//! install card's settings and adding visiting companies to a campaign.

use std::net::{IpAddr, SocketAddr};

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path, Query, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::demo::client_ip;
use super::hiring::add_to_campaign;
use super::util_input;
use super::util_pagination::{PageQuery, Paginated};
use super::util_scope;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::models::{Segment, Ts};
use crate::state::AppState;
use crate::visitors::{self, Hit, Site};

pub fn router() -> Router<AppState> {
    Router::new()
        // One parameter name for both: the router refuses two names in the same place. The first one's is "{key}.js".
        .route("/v/{key}", get(snippet))
        .route("/v/{key}/hit", post(hit).layer(DefaultBodyLimit::max(visitors::MAX_BODY)))
        .route("/projects/{id}/visitors", get(list))
        .route("/projects/{id}/visitors/site", get(site).put(update_site))
        .route("/projects/{id}/visitors/add", post(add))
}

/// Page views a minute from one IP, which a person clicking around stays well under.
const PER_IP: u32 = 60;
/// Page views a minute to one site, from everyone.
const PER_KEY: u32 = 3000;
const MAX_DOMAINS: usize = 20;
const MAX_PATHS: usize = 20;
const MAX_ADD: usize = 500;

pub(super) async fn project_by_key(db: &PgPool, key: &str) -> sqlx::Result<Option<Site>> {
    let row: Option<(Uuid, Vec<String>)> = sqlx::query_as(
        "select s.project_id, s.domains from visitor_sites s join projects p on p.id = s.project_id where s.site_key = $1 and p.org_id is not null",
    )
    .bind(key)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(project_id, domains)| Site { project_id, domains }))
}

/// The same script for every site, so browsers and CDNs can keep it for an hour.
async fn snippet(State(state): State<AppState>, Path(file): Path<String>) -> AppResult<Response> {
    let key = file.strip_suffix(".js").ok_or(AppError::NotFound)?;
    project_by_key(&state.db, key).await?.ok_or(AppError::NotFound)?;
    let headers = [(CONTENT_TYPE, "text/javascript; charset=utf-8"), (CACHE_CONTROL, "public, max-age=3600")];
    Ok((headers, visitors::SNIPPET).into_response())
}

/// The snippet already sends nothing then, but a browser can send the signal without running the snippet's check.
fn asks_not_to_track(headers: &HeaderMap) -> bool {
    ["dnt", "sec-gpc"].iter().any(|name| headers.get(*name).is_some_and(|v| v.as_bytes() == b"1"))
}

/// The host of the page that sent the request: the Origin header's, or the Referer's when a browser leaves Origin out.
pub(super) fn sending_host(headers: &HeaderMap) -> Option<String> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).filter(|v| !v.is_empty() && *v != "null");
    let url = reqwest::Url::parse(header("origin").or(header("referer"))?).ok()?;
    url.host_str().map(str::to_lowercase)
}

/// 204 for a page view that was stored or that the visitor asked not to be tracked for. The body is the snippet's JSON,
/// which sendBeacon sends as text/plain so the browser doesn't ask first.
async fn hit(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<StatusCode> {
    if asks_not_to_track(&headers) {
        return Ok(StatusCode::NO_CONTENT);
    }
    let ip: IpAddr = client_ip(&headers, peer).parse().map_err(|_| AppError::BadRequest("Couldn't read the visitor's address".into()))?;
    let hash = visitors::ip_hash(&state.cfg.app_secret, ip);
    if !visitors::allow(&format!("ip:{hash}"), PER_IP) || !visitors::allow(&format!("key:{key}"), PER_KEY) {
        return Err(AppError::TooManyRequests("Too many page views. Try again in a minute.".into()));
    }
    let site = project_by_key(&state.db, &key).await?.ok_or(AppError::NotFound)?;
    let allowed = sending_host(&headers).is_some_and(|host| visitors::allowed_host(&site.domains, &host));
    if !allowed {
        return Err(AppError::Forbidden("This site isn't one of the project's domains".into()));
    }
    let hit: Hit = serde_json::from_slice(&body).map_err(|_| AppError::BadRequest("Send the page view as JSON".into()))?;
    visitors::record(&state, &site, ip, &hash, hit).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The install card: the key in the snippet's URL, where hits may come from, the pages that alert, and the latest hit.
#[derive(Serialize, FromRow, Debug)]
pub struct VisitorSite {
    site_key: String,
    domains: Vec<String>,
    intent_paths: Vec<String>,
    last_hit_at: Option<Ts>,
    last_hit_path: Option<String>,
}

const SITE: &str = "select s.site_key, s.domains, s.intent_paths, s.last_hit_at,
       (select h.path from visitor_hits h where h.project_id = s.project_id order by h.created_at desc limit 1) as last_hit_path
     from visitor_sites s where s.project_id = $1";

/// Made on first look, allowing the project's own domain.
pub(super) async fn load_site(db: &PgPool, project_id: Uuid, project_domain: Option<&str>) -> sqlx::Result<VisitorSite> {
    let domains: Vec<String> = project_domain.and_then(util_input::domain).into_iter().collect();
    sqlx::query("insert into visitor_sites (project_id, domains) values ($1, $2) on conflict (project_id) do nothing")
        .bind(project_id)
        .bind(&domains)
        .execute(db)
        .await?;
    sqlx::query_as(SITE).bind(project_id).fetch_one(db).await
}

async fn site(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<VisitorSite>> {
    let project = auth.project(&state.db, id).await?;
    Ok(Json(load_site(&state.db, id, project.domain.as_deref()).await?))
}

#[derive(Deserialize)]
struct SiteBody {
    domains: Option<Vec<String>>,
    intent_paths: Option<Vec<String>>,
}

/// A page as typed: a path or a whole link, stored as the path alone. None when it isn't one.
pub(super) fn intent_path(raw: &str) -> Option<String> {
    let raw = raw.trim();
    let path = match raw.split_once("://") {
        Some((_, rest)) => rest.find('/').map_or("/", |i| &rest[i..]),
        None => raw,
    };
    let path = visitors::clean_path(path);
    path.chars().all(|c| c.is_ascii_alphanumeric() || "/-_.~%".contains(c)).then_some(path).filter(|p| p != "/" || raw == "/")
}

async fn update_site(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<SiteBody>) -> AppResult<Json<VisitorSite>> {
    let project = auth.project(&state.db, id).await?;
    load_site(&state.db, id, project.domain.as_deref()).await?;
    let mut domains: Option<Vec<String>> = None;
    if let Some(raw) = body.domains {
        let mut list: Vec<String> = vec![];
        for d in raw.iter().map(|d| d.trim()).filter(|d| !d.is_empty()) {
            let domain = util_input::domain(d).ok_or_else(|| AppError::BadRequest(format!("{d} doesn't look like a website")))?;
            if !list.contains(&domain) {
                list.push(domain);
            }
        }
        if list.is_empty() {
            return Err(AppError::BadRequest("Add the domain your site is on".into()));
        }
        if list.len() > MAX_DOMAINS {
            return Err(AppError::BadRequest(format!("Add at most {MAX_DOMAINS} domains")));
        }
        domains = Some(list);
    }
    let mut paths: Option<Vec<String>> = None;
    if let Some(raw) = body.intent_paths {
        let mut list: Vec<String> = vec![];
        for p in raw.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            let path = intent_path(p).ok_or_else(|| AppError::BadRequest(format!("{p} isn't a page on your site, like /pricing")))?;
            if !list.contains(&path) {
                list.push(path);
            }
        }
        if list.len() > MAX_PATHS {
            return Err(AppError::BadRequest(format!("Mark at most {MAX_PATHS} pages")));
        }
        paths = Some(list);
    }
    sqlx::query("update visitor_sites set domains = coalesce($2, domains), intent_paths = coalesce($3, intent_paths) where project_id = $1")
        .bind(id)
        .bind(domains)
        .bind(paths)
        .execute(&state.db)
        .await?;
    Ok(Json(sqlx::query_as(SITE).bind(id).fetch_one(&state.db).await?))
}

#[derive(Deserialize)]
struct ListQuery {
    /// 7 unless set.
    days: Option<i32>,
    /// Companies that read at least this many pages in the period.
    min_views: Option<i64>,
}

/// A company that visited, most recent first. company_id is set when it's one of the project's companies, campaign when it
/// or one of its leads is in a campaign, and leads counts the project's leads there, so only companies without any can be
/// added. source is how its first visit since Splee started keeping them arrived: a utm_source, a referring site, or empty
/// for a direct visit.
#[derive(Serialize, FromRow, Debug)]
pub struct Visitor {
    domain: String,
    name: String,
    company_id: Option<Uuid>,
    campaign: Option<String>,
    leads: i64,
    views: i64,
    visits: i64,
    last_visit: Ts,
    top_paths: Vec<String>,
    source: String,
}

async fn list(
    State(state): State<AppState>,
    auth: Auth,
    Path(id): Path<Uuid>,
    Query(q): Query<ListQuery>,
    Query(page): Query<PageQuery>,
) -> AppResult<Json<Paginated<Visitor>>> {
    auth.project(&state.db, id).await?;
    let days = q.days.unwrap_or(7).clamp(1, visitors::RETENTION_DAYS);
    Ok(Json(companies(&state.db, id, days, q.min_views.unwrap_or(1).max(1), &page).await?))
}

const VISITS: &str = "with v as (
       select h.domain, count(*) as views, max(h.created_at) as last_visit, nullif(max(h.company), '') as company,
              count(distinct coalesce(nullif(h.visitor_id, ''), h.ip_hash) || ':' || (h.created_at at time zone 'utc')::date) as visits
       from visitor_hits h
       where h.project_id = $1 and h.domain is not null and h.created_at > now() - make_interval(days => $2)
       group by h.domain having count(*) >= $3
     )";

async fn companies(db: &PgPool, project_id: Uuid, days: i32, min_views: i64, page: &PageQuery) -> sqlx::Result<Paginated<Visitor>> {
    let total: i64 = sqlx::query_scalar(&format!("{VISITS} select count(*) from v")).bind(project_id).bind(days).bind(min_views).fetch_one(db).await?;
    let items = sqlx::query_as(&format!(
        "{VISITS}
         select v.domain::text, coalesce(nullif(c.name, ''), d.name, v.company, v.domain::text) as name, c.id as company_id,
                v.views, v.visits, v.last_visit,
                (select s.name from segments s where s.id = coalesce(c.segment_id, (select l.segment_id from leads l
                   where l.project_id = $1 and l.company_domain = v.domain and l.segment_id is not null order by l.created_at limit 1))) as campaign,
                (select count(*) from leads l where l.project_id = $1 and (l.company_id = c.id or l.company_domain = v.domain)) as leads,
                array(select h.path from visitor_hits h
                      where h.project_id = $1 and h.domain = v.domain and h.created_at > now() - make_interval(days => $2)
                      group by h.path order by count(*) desc, h.path limit 3) as top_paths,
                coalesce((select h.source from visitor_hits h where h.project_id = $1 and h.domain = v.domain order by h.created_at, h.id limit 1), '') as source
         from v
         left join companies c on c.project_id = $1 and c.domain = v.domain
         left join directory_companies d on d.domain = lower(v.domain::text)
         order by v.last_visit desc, v.domain
         limit $4 offset $5"
    ))
    .bind(project_id)
    .bind(days)
    .bind(min_views)
    .bind(page.limit())
    .bind(page.offset())
    .fetch_all(db)
    .await?;
    Ok(Paginated { items, total })
}

#[derive(Deserialize)]
struct AddBody {
    segment_id: Uuid,
    domains: Vec<String>,
}

/// Makes the visiting companies the project doesn't have yet into companies, then moves them into the campaign and finds
/// people there, as the Hiring tab does. Only domains that visited the project's site count.
async fn add(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<AddBody>) -> AppResult<Json<Value>> {
    let segment: Segment = util_scope::load(&state.db, "segments", body.segment_id, auth.org_id).await?;
    if segment.project_id != id {
        return Err(AppError::BadRequest("That campaign isn't part of this project".into()));
    }
    if body.domains.len() > MAX_ADD {
        return Err(AppError::BadRequest(format!("Add at most {MAX_ADD} companies at a time")));
    }
    let domains: Vec<String> = body.domains.iter().filter_map(|d| util_input::domain(d)).collect();
    let (company_ids, created) = companies_for(&state.db, id, &domains).await?;
    if company_ids.is_empty() {
        return Err(AppError::BadRequest("Pick at least one company that visited your site".into()));
    }
    match add_to_campaign(&state.db, auth.org_id, &segment, &company_ids).await {
        Ok(added) => Ok(Json(added)),
        Err(e) => {
            sqlx::query("delete from companies where id = any($1)").bind(&created).execute(&state.db).await?;
            Err(e)
        }
    }
}

/// The project's companies for these visiting domains, made for the ones it doesn't have, and the ids it made.
async fn companies_for(db: &PgPool, project_id: Uuid, domains: &[String]) -> sqlx::Result<(Vec<Uuid>, Vec<Uuid>)> {
    let created: Vec<Uuid> = sqlx::query_scalar(
        "insert into companies (project_id, domain, name, city, country_code, fit_reason, source_url)
         select $1, v.domain, coalesce(d.name, v.company, v.domain::text), coalesce(d.city, ''), coalesce(d.country_code, ''),
                'Visited your website', coalesce(d.website, 'https://' || v.domain)
         from (select domain, nullif(max(company), '') as company from visitor_hits
               where project_id = $1 and domain = any($2::citext[]) group by domain) v
         left join directory_companies d on d.domain = lower(v.domain::text)
         on conflict (project_id, domain) do nothing
         returning id",
    )
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "select c.id from companies c where c.project_id = $1 and c.domain = any($2::citext[])
           and exists (select 1 from visitor_hits h where h.project_id = $1 and h.domain = c.domain)",
    )
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await?;
    Ok((ids, created))
}

#[cfg(test)]
mod tests {
    use axum::Json;
    use axum::extract::{Path, Query, State};
    use reqwest::StatusCode;
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::error::AppError;

    async fn org_with_project(db: &PgPool) -> (Auth, Uuid) {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid =
            sqlx::query_scalar("insert into projects (org_id, domain) values ($1, 'https://www.fieldnote.example/') returning id").bind(org).fetch_one(db).await.unwrap();
        (Auth { user_id: Uuid::new_v4(), org_id: org, role: "owner".into(), token_hash: String::new() }, project)
    }

    async fn serve(state: AppState) -> String {
        let app = Router::new().nest("/api", router()).with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await.unwrap() });
        base
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn page_views_are_checked_capped_and_kept_without_the_ip(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (auth, project) = org_with_project(&db).await;
        let Json(site) = super::site(State(state.clone()), auth, Path(project)).await.unwrap();
        assert_eq!(site.domains, ["fieldnote.example"]);
        let base = serve(state.clone()).await;
        let client = reqwest::Client::new();

        let script = client.get(format!("{base}/v/{}.js", site.site_key)).send().await.unwrap();
        assert_eq!(script.status(), StatusCode::OK);
        assert_eq!(script.headers()["content-type"], "text/javascript; charset=utf-8");
        assert_eq!(script.headers()["cache-control"], "public, max-age=3600");
        assert!(script.headers().get("set-cookie").is_none());
        assert_eq!(script.text().await.unwrap(), crate::visitors::SNIPPET);
        assert_eq!(client.get(format!("{base}/v/nope.js")).send().await.unwrap().status(), StatusCode::NOT_FOUND);

        let url = format!("{base}/v/{}/hit", site.site_key);
        let body = json!({"path": "/pricing/?plan=team", "title": "Pricing", "referrer": "https://news.example/item?id=1&email=a@b.example",
            "visitor": "v-1", "utm_source": "newsletter", "utm_campaign": "launch"})
        .to_string();
        let send = |ip: &'static str, headers: &'static [(&'static str, &'static str)], body: String| {
            let mut req = client.post(&url).header("content-type", "text/plain;charset=UTF-8").header("x-forwarded-for", ip).body(body);
            for (name, value) in headers {
                req = req.header(*name, *value);
            }
            async move { req.send().await.unwrap().status() }
        };
        assert_eq!(send("203.0.113.7", &[("origin", "https://www.fieldnote.example")], body.clone()).await, StatusCode::NO_CONTENT);
        // Without an Origin, the Referer's host is checked.
        assert_eq!(send("203.0.113.7", &[("referer", "https://blog.fieldnote.example/post")], "{\"path\": \"/post\"}".into()).await, StatusCode::NO_CONTENT);
        for refused in [&[("origin", "https://evil.example")][..], &[("origin", "https://fieldnote.example.evil.example")], &[("origin", "null")], &[]] {
            assert_eq!(send("203.0.113.7", refused, body.clone()).await, StatusCode::FORBIDDEN);
        }
        // Do Not Track and Global Privacy Control store nothing.
        assert_eq!(send("203.0.113.7", &[("origin", "https://fieldnote.example"), ("dnt", "1")], body.clone()).await, StatusCode::NO_CONTENT);
        assert_eq!(send("203.0.113.7", &[("origin", "https://fieldnote.example"), ("sec-gpc", "1")], body.clone()).await, StatusCode::NO_CONTENT);
        let big = json!({"path": "/", "title": "x".repeat(visitors::MAX_BODY)}).to_string();
        assert_eq!(send("203.0.113.8", &[("origin", "https://fieldnote.example")], big).await, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(send("203.0.113.8", &[("origin", "https://fieldnote.example")], "not json".into()).await, StatusCode::BAD_REQUEST);

        let rows: Vec<(String, String, String, String, String, String)> =
            sqlx::query_as("select ip_hash, path, referrer, source, utm_campaign, to_jsonb(h)::text from visitor_hits h order by id").fetch_all(&db).await.unwrap();
        assert_eq!(rows.len(), 2);
        let (hash, path, referrer, source, campaign, _) = &rows[0];
        assert_eq!(*hash, visitors::ip_hash(&state.cfg.app_secret, "203.0.113.7".parse().unwrap()));
        assert_eq!((path.as_str(), referrer.as_str(), source.as_str(), campaign.as_str()), ("/pricing", "https://news.example/item", "newsletter", "launch"));
        // The blog is the site itself, so it isn't a source.
        assert_eq!((rows[1].1.as_str(), rows[1].3.as_str()), ("/post", ""));
        assert!(rows.iter().all(|r| !r.5.contains("203.0.113")), "no raw IP is stored");
        let last: Option<Ts> = sqlx::query_scalar("select last_hit_at from visitor_sites where project_id = $1").bind(project).fetch_one(&db).await.unwrap();
        assert!(last.is_some());

        // One IP gets PER_IP page views a minute.
        let mut statuses = vec![];
        for _ in 0..=PER_IP {
            statuses.push(send("198.51.100.77", &[("origin", "https://fieldnote.example")], "{}".into()).await);
        }
        assert!(statuses[..PER_IP as usize].iter().all(|s| *s == StatusCode::NO_CONTENT));
        assert_eq!(statuses[PER_IP as usize], StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(send("198.51.100.78", &[("origin", "https://fieldnote.example")], "{}".into()).await, StatusCode::NO_CONTENT, "another IP still gets through");
        assert!((0..PER_KEY).all(|_| visitors::allow("key:test-site", PER_KEY)));
        assert!(!visitors::allow("key:test-site", PER_KEY));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_visitors_tab_lists_companies_adds_them_and_stays_in_the_organization(db: PgPool) {
        let state = crate::autopilot::tests::state(db.clone());
        let (auth, project) = org_with_project(&db).await;
        let (other, other_project) = org_with_project(&db).await;
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Q4') returning id").bind(project).fetch_one(&db).await.unwrap();
        sqlx::query("insert into companies (project_id, segment_id, domain, name) values ($1, $2, 'kestrel.example', 'Kestrel')")
            .bind(project)
            .bind(segment)
            .execute(&db)
            .await
            .unwrap();
        sqlx::query("insert into directory_companies (domain, name, website, country_code, source_id, release) values ('quarry.example', 'Quarry Co', 'https://quarry.example', 'US', 's', 'r')")
            .execute(&db)
            .await
            .unwrap();
        for (domain, company, visitor, path, source, age) in [
            ("quarry.example", "QUARRY-NET", "a", "/pricing", "linkedin", "3 days"),
            ("quarry.example", "QUARRY-NET", "a", "/pricing", "", "3 days"),
            ("quarry.example", "QUARRY-NET", "b", "/docs", "", "1 hour"),
            ("kestrel.example", "Kestrel Labs", "c", "/", "", "2 hours"),
            ("old.example", "Old", "d", "/", "", "20 days"),
        ] {
            sqlx::query(
                "insert into visitor_hits (project_id, ip_hash, visitor_id, domain, company, path, source, created_at) values ($1, 'h', $2, $3, $4, $5, $6, now() - $7::interval)",
            )
            .bind(project)
            .bind(visitor)
            .bind(domain)
            .bind(company)
            .bind(path)
            .bind(source)
            .bind(age)
            .execute(&db)
            .await
            .unwrap();
        }
        let list = |days: Option<i32>, min_views: Option<i64>| {
            let (state, auth) = (state.clone(), auth.clone());
            async move {
                let Json(page) = super::list(State(state), auth, Path(project), Query(ListQuery { days, min_views }), Query(PageQuery { page: None, per_page: None }))
                    .await
                    .unwrap();
                page
            }
        };
        let week = list(None, None).await;
        assert_eq!(week.total, 2);
        // The latest visit first.
        let kestrel = &week.items[1];
        assert_eq!((kestrel.name.as_str(), kestrel.campaign.as_deref(), kestrel.company_id.is_some()), ("Kestrel", Some("Q4"), true));
        let quarry = &week.items[0];
        assert_eq!((quarry.name.as_str(), quarry.views, quarry.visits, quarry.source.as_str()), ("Quarry Co", 3, 2, "linkedin"));
        assert_eq!(quarry.top_paths, ["/pricing", "/docs"]);
        assert_eq!((quarry.campaign.as_deref(), quarry.leads), (None, 0));
        assert_eq!(list(None, Some(2)).await.items.len(), 1);
        assert_eq!(list(Some(30), None).await.total, 3);

        // Adding makes Quarry a company in the campaign and starts Find people. A domain that never visited is left out.
        let body = |domains: &[&str]| Json(AddBody { segment_id: segment, domains: domains.iter().map(|d| d.to_string()).collect() });
        let Json(added) = add(State(state.clone()), auth.clone(), Path(project), body(&["quarry.example", "never.example"])).await.unwrap();
        assert_eq!(added["added"], 1);
        let quarry: (String, Option<Uuid>, String) =
            sqlx::query_as("select name, segment_id, fit_reason from companies where project_id = $1 and domain = 'quarry.example'").bind(project).fetch_one(&db).await.unwrap();
        assert_eq!(quarry, ("Quarry Co".to_string(), Some(segment), "Visited your website".to_string()));
        let never: i64 = sqlx::query_scalar("select count(*) from companies where domain = 'never.example'").fetch_one(&db).await.unwrap();
        assert_eq!(never, 0);
        let refused = add(State(state.clone()), auth.clone(), Path(project), body(&["never.example"])).await;
        assert!(matches!(refused, Err(AppError::BadRequest(_))));

        // Settings: domains and pages as typed, cleaned up.
        let update = |auth: Auth, body: serde_json::Value| update_site(State(state.clone()), auth, Path(project), Json(serde_json::from_value(body).unwrap()));
        let Json(saved) = update(auth.clone(), json!({"domains": ["https://Fieldnote.example/", "app.fieldnote.example"], "intent_paths": ["pricing", "https://fieldnote.example/demo/?x=1"]}))
            .await
            .unwrap();
        assert_eq!((saved.domains, saved.intent_paths), (vec!["fieldnote.example".to_string(), "app.fieldnote.example".into()], vec!["/pricing".to_string(), "/demo".into()]));
        assert!(matches!(update(auth.clone(), json!({"domains": []})).await, Err(AppError::BadRequest(_))));
        assert!(matches!(update(auth.clone(), json!({"intent_paths": ["/pri cing"]})).await, Err(AppError::BadRequest(_))));

        // Another organization's project looks like one that doesn't exist.
        assert!(matches!(super::site(State(state.clone()), other.clone(), Path(project)).await, Err(AppError::NotFound)));
        assert!(matches!(update(other.clone(), json!({"domains": ["evil.example"]})).await, Err(AppError::NotFound)));
        let listed = super::list(State(state.clone()), other.clone(), Path(project), Query(ListQuery { days: None, min_views: None }), Query(PageQuery { page: None, per_page: None })).await;
        assert!(matches!(listed, Err(AppError::NotFound)));
        let added = add(State(state.clone()), other.clone(), Path(other_project), body(&["kestrel.example"])).await;
        assert!(matches!(added, Err(AppError::NotFound)));
        let domains: Vec<String> = sqlx::query_scalar("select unnest(domains) from visitor_sites where project_id = $1").bind(project).fetch_all(&db).await.unwrap();
        assert_eq!(domains, ["fieldnote.example", "app.fieldnote.example"]);
    }
}
