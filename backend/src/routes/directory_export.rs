//! A company database search as a file to take elsewhere, like a CRM: the Companies page's Download CSV, GET /v1/companies/export
//! and the MCP server's download_companies. Rows stream from Postgres a chunk at a time, so even the biggest download is never
//! held in memory. Each one counts against the organization's downloads this hour and goes in its audit log.

use std::sync::LazyLock;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use futures::{StreamExt, TryStreamExt, stream};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::directory::SearchQuery;
use super::util_csv;
use super::util_input;
use crate::audit::{self, Target, Who};
use crate::auth::Auth;
use crate::directory::{self, Export};
use crate::error::{AppError, AppResult};
use crate::rate_limit::{Limits, RateLimiter};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/directory/companies/export", get(download)).route("/directory/companies/export/count", get(count))
}

/// The most rows one download holds. The API takes the next ones with page=2.
pub const MAX_ROWS: i64 = 50_000;
/// The same as adding ticked companies to a campaign.
const MAX_SELECTED: usize = 500;
/// Sent as one write, so a big file is a hundred writes rather than one a row.
const CHUNK: usize = 500;
/// Apart from the API's hour, which a download is one request of. A download holds a database connection while it streams,
/// so only a few run at once.
static DOWNLOADS: LazyLock<RateLimiter> = LazyLock::new(|| RateLimiter::new(Limits { per_hour: 20, in_flight: 3, what: "company downloads" }));

const COLUMNS: [&str; 14] = [
    "name", "domain", "website", "email", "phone", "address", "city", "region", "postcode", "country", "category", "confidence", "source_id",
    "source_release",
];

#[derive(Deserialize, Serialize, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub(super) enum Format {
    #[default]
    Csv,
    Jsonl,
}

#[derive(Deserialize)]
pub(super) struct ExportQuery {
    /// Comma-separated: only these of the search's companies, the rows ticked on the page.
    domains: Option<String>,
    #[serde(default)]
    format: Format,
    /// Pages like the API's lists, of MAX_ROWS unless per_page asks for fewer.
    page: Option<i64>,
    per_page: Option<i64>,
}

async fn download(State(state): State<AppState>, auth: Auth, Query(q): Query<SearchQuery>, Query(x): Query<ExportQuery>) -> AppResult<Response> {
    export(&state, Who::from(&auth), &q, &x).await
}

#[derive(Serialize)]
struct Count {
    rows: i64,
    capped: bool,
}

/// What Download CSV would take, for its label, since the search's own count stops at 10,000. Free, as it sends no rows.
async fn count(State(state): State<AppState>, auth: Auth, Query(q): Query<SearchQuery>) -> AppResult<Json<Count>> {
    let found = directory::count_from(&state.db, &q.search(Some(auth.org_id))?, 0, MAX_ROWS).await?;
    Ok(Json(Count { rows: found.min(MAX_ROWS), capped: found > MAX_ROWS }))
}

/// The search's rows as a file, in its order. Splee-Rows says how many, and Splee-Capped says more matched than this page holds.
/// Over the organization's downloads this hour it's a 429 with Retry-After.
pub(super) async fn export(state: &AppState, who: Who, q: &SearchQuery, x: &ExportQuery) -> AppResult<Response> {
    let mut search = q.search(Some(who.org_id()))?;
    search.domains = x.domains.as_deref().map(selected).transpose()?;
    let per_page = x.per_page.unwrap_or(MAX_ROWS).clamp(1, MAX_ROWS);
    let page = x.page.unwrap_or(1).max(1);
    let offset = (page - 1) * per_page;
    let permit = match DOWNLOADS.acquire(who.org_id()) {
        Ok(permit) => permit,
        Err(refused) => return Ok(refused.into_response()),
    };
    let found = directory::count_from(&state.db, &search, offset, per_page).await?;
    let (rows, capped) = (found.min(per_page), found > per_page);
    let details = json!({
        "filters": q,
        "selected": search.domains.as_ref().map(Vec::len),
        "format": x.format,
        "page": page,
        "rows": rows,
        "capped": capped,
    });
    audit::record(&state.db, who, "companies.downloaded", Target::new("directory", None, "Company database"), details).await;

    let format = x.format;
    let head = match format {
        Format::Csv => Some(util_csv::encode([COLUMNS]).map_err(|e| AppError::Internal(e.into()))?),
        Format::Jsonl => None,
    };
    let chunks = directory::export(state.db.clone(), search, per_page, offset).try_chunks(CHUNK).map(move |chunk| {
        // Held until the body is dropped, so a download counts as running until it ends or the caller goes away.
        let _running = &permit;
        encode(format, &chunk.map_err(|e| e.1)?)
    });
    let body = stream::iter(head.map(anyhow::Ok)).chain(chunks).inspect_err(|e| tracing::error!(error = ?e, "company download failed"));

    let (kind, extension) = match format {
        Format::Csv => ("text/csv; charset=utf-8", "csv"),
        Format::Jsonl => ("application/jsonl", "jsonl"),
    };
    let mut res = Body::from_stream(body).into_response();
    let headers = res.headers_mut();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(kind));
    let disposition = format!("attachment; filename=\"{}.{extension}\"", filename(q));
    headers.insert(header::CONTENT_DISPOSITION, HeaderValue::from_str(&disposition).map_err(|e| AppError::Internal(e.into()))?);
    headers.insert("splee-rows", HeaderValue::from(rows));
    headers.insert("splee-capped", HeaderValue::from_static(if capped { "true" } else { "false" }));
    Ok(res)
}

/// The ticked companies, each once.
fn selected(raw: &str) -> AppResult<Vec<String>> {
    let mut domains: Vec<String> = Vec::new();
    for domain in raw.split(',').filter_map(util_input::domain) {
        if !domains.contains(&domain) {
            domains.push(domain);
        }
    }
    if domains.is_empty() {
        return Err(AppError::BadRequest("Pick at least one company".into()));
    }
    if domains.len() > MAX_SELECTED {
        return Err(AppError::BadRequest(format!("Download at most {MAX_SELECTED} picked companies at a time, or the whole search")));
    }
    Ok(domains)
}

fn encode(format: Format, rows: &[Export]) -> anyhow::Result<Vec<u8>> {
    Ok(match format {
        Format::Csv => util_csv::encode(rows.iter().map(cells))?,
        Format::Jsonl => {
            let mut out = Vec::new();
            for row in rows {
                serde_json::to_writer(&mut out, row)?;
                out.push(b'\n');
            }
            out
        }
    })
}

fn cells(r: &Export) -> [String; 14] {
    let text = |v: &Option<String>| v.clone().unwrap_or_default();
    [
        r.name.clone(),
        r.domain.clone(),
        r.website.clone(),
        text(&r.email),
        text(&r.phone),
        text(&r.address),
        text(&r.city),
        text(&r.region),
        text(&r.postcode),
        r.country_code.clone(),
        text(&r.category),
        format!("{:.2}", r.confidence),
        r.source_id.clone(),
        r.release.clone(),
    ]
}

/// "companies-dental-leeds-gb", so a few downloads in a row don't all land as companies.csv.
fn filename(q: &SearchQuery) -> String {
    let words = [&q.q, &q.category, &q.city, &q.country, &q.tech].into_iter().flatten().map(|w| w.to_lowercase()).collect::<Vec<_>>().join(" ");
    let slug: Vec<String> = words.split(|c: char| !c.is_ascii_alphanumeric()).filter(|w| !w.is_empty()).map(String::from).collect();
    let name: String = ["companies".to_string()].into_iter().chain(slug).collect::<Vec<_>>().join("-");
    name.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use reqwest::header::HeaderMap;
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use crate::crypto::sha256_hex;

    const KEY: &str = "splee_export_acme";

    struct Server {
        base: String,
        http: reqwest::Client,
        /// Acme's and Beta's ids. Acme's owner is signed in as session "acme" and has the API key KEY, Beta's as "beta".
        orgs: [Uuid; 2],
    }

    impl Server {
        async fn start(db: &PgPool) -> Self {
            let mut orgs = vec![];
            for name in ["acme", "beta"] {
                let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
                let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                    .bind(format!("{name}@{name}.example"))
                    .fetch_one(db)
                    .await
                    .unwrap();
                sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(db).await.unwrap();
                sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                    .bind(sha256_hex(name))
                    .bind(user)
                    .bind(org)
                    .execute(db)
                    .await
                    .unwrap();
                orgs.push(org);
            }
            sqlx::query("insert into api_keys (org_id, name, prefix, key_hash) values ($1, 'export', 'splee_export_', $2)")
                .bind(orgs[0])
                .bind(sha256_hex(KEY))
                .execute(db)
                .await
                .unwrap();
            let state = crate::autopilot::tests::state(db.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}/api", listener.local_addr().unwrap());
            let routes = super::router().merge(super::super::directory::router()).merge(super::super::api_v1::router(&state));
            let app = Router::new().nest("/api", routes).with_state(state);
            tokio::spawn(async move { axum::serve(listener, app).await });
            Self { base, http: reqwest::Client::new(), orgs: [orgs[0], orgs[1]] }
        }

        /// As a signed-in member, or with an API key when `session` starts with splee_.
        async fn get(&self, session: &str, path: &str) -> (u16, HeaderMap, String) {
            let req = self.http.get(format!("{}{path}", self.base));
            let req = if session.starts_with("splee_") { req.bearer_auth(session) } else { req.header("cookie", format!("splee_session={session}")) };
            let res = req.send().await.unwrap();
            (res.status().as_u16(), res.headers().clone(), res.text().await.unwrap())
        }

        async fn csv(&self, session: &str, query: &str) -> Vec<Vec<String>> {
            let (status, headers, body) = self.get(session, &format!("/directory/companies/export?{query}")).await;
            assert_eq!(status, 200, "{body}");
            assert_eq!(headers["content-type"], "text/csv; charset=utf-8");
            let mut reader = csv::ReaderBuilder::new().has_headers(false).from_reader(body.as_bytes());
            reader.records().map(|r| r.unwrap().iter().map(String::from).collect()).collect()
        }
    }

    fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
        headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
    }

    /// Every one has a phone, and is a health business.
    async fn dentists(db: &PgPool) {
        let companies = [
            ("grin.co.uk", "Grin Dental", None, "Leeds", "GB", "dental_clinic", 0.95_f32),
            ("smile.co.uk", "Smile Dental", Some("hi@smile.co.uk"), "Leeds", "GB", "dental_clinic", 0.9),
            ("bakery.co.uk", "Leeds Bakery", Some("buns@bakery.co.uk"), "LEEDS", "GB", "bakery", 0.8),
            ("kayl.lu", "Kayl Dental", Some("hi@kayl.lu"), "Esch", "LU", "dental_clinic", 0.7),
            ("evil.co.uk", "=HYPERLINK(\"https://evil.example\",\"Dental\")", Some("@evil.co.uk"), "Leeds", "GB", "dental_clinic", 0.5),
        ];
        for (domain, name, email, city, country, category, confidence) in companies {
            sqlx::query(
                "insert into directory_companies (domain, name, website, email, phone, address, city, region, postcode, country_code, category,
                     categories, confidence, source_id, release)
                 values ($1, $2, 'https://' || $1, $3, '+44 113 496 0000', '1 Park Row', $4, 'West Yorkshire', 'LS1 5AB', $5, $6,
                     array['health', $6], $7, 'place-' || $1, '2026-10-15.0')",
            )
            .bind(domain)
            .bind(name)
            .bind(email)
            .bind(city)
            .bind(country)
            .bind(category)
            .bind(confidence)
            .execute(db)
            .await
            .unwrap();
        }
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_download_has_the_searchs_companies_in_its_order(db: PgPool) {
        dentists(&db).await;
        let s = Server::start(&db).await;
        for query in ["", "q=dental", "q=dent&country=gb", "city=leeds&has_email=true", "category=health&has_phone=true&country=GB", "q=nothing"] {
            let (_, _, page) = s.get("acme", &format!("/directory/companies?{query}&per_page=200")).await;
            let page: Value = serde_json::from_str(&page).unwrap();
            let searched: Vec<&str> = page["items"].as_array().unwrap().iter().map(|c| c["domain"].as_str().unwrap()).collect();
            let rows = s.csv("acme", query).await;
            assert_eq!(rows[0], super::COLUMNS, "{query}");
            assert_eq!(rows[1..].iter().map(|r| r[1].as_str()).collect::<Vec<_>>(), searched, "{query}");
        }

        let rows = s.csv("acme", "q=dental&country=GB").await;
        assert_eq!(rows[2], [
            "Smile Dental", "smile.co.uk", "https://smile.co.uk", "hi@smile.co.uk", "'+44 113 496 0000", "1 Park Row", "Leeds", "West Yorkshire",
            "LS1 5AB", "GB", "dental_clinic", "0.90", "place-smile.co.uk", "2026-10-15.0",
        ]);
        // A cell that starts like a formula would run in Excel or Sheets, so it starts with an apostrophe instead.
        let evil = rows.iter().find(|r| r[1] == "evil.co.uk").unwrap();
        assert_eq!((evil[0].as_str(), evil[3].as_str()), ("'=HYPERLINK(\"https://evil.example\",\"Dental\")", "'@evil.co.uk"));

        // Ticked rows are the ones downloaded, in the search's order, and only those the search still finds.
        let rows = s.csv("acme", "q=dental&domains=https://www.smile.co.uk/,grin.co.uk,bakery.co.uk,smile.co.uk").await;
        assert_eq!(rows[1..].iter().map(|r| r[1].as_str()).collect::<Vec<_>>(), ["grin.co.uk", "smile.co.uk"]);
        let (status, _, body) = s.get("acme", "/directory/companies/export?domains=,%20,").await;
        assert_eq!((status, body.contains("Pick at least one company")), (400, true), "{body}");

        let (_, headers, _) = s.get("acme", "/directory/companies/export?q=Dental&city=Leeds").await;
        assert_eq!(header(&headers, "content-disposition"), "attachment; filename=\"companies-dental-leeds.csv\"");
        assert_eq!((header(&headers, "splee-rows"), header(&headers, "splee-capped")), ("3", "false"));
        assert_eq!(s.get("acme", "/directory/companies/export?country=GBR").await.0, 400);
        assert_eq!(s.get("nobody", "/directory/companies/export").await.0, 401);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_download_stops_at_the_cap_and_says_so(db: PgPool) {
        sqlx::query(
            "insert into directory_companies (domain, name, website, country_code, confidence, source_id, release)
             select 'c' || lpad(i::text, 5, '0') || '.example', 'Company ' || i, 'https://c' || i || '.example', 'MT', 0.5, 'x', 'r'
             from generate_series(1, 50002) i",
        )
        .execute(&db)
        .await
        .unwrap();
        let s = Server::start(&db).await;

        let (_, _, count) = s.get("acme", "/directory/companies/export/count?country=MT").await;
        assert_eq!(serde_json::from_str::<Value>(&count).unwrap(), json!({ "rows": 50_000, "capped": true }));
        let (status, headers, body) = s.get("acme", "/directory/companies/export?country=MT").await;
        assert_eq!((status, header(&headers, "splee-rows"), header(&headers, "splee-capped")), (200, "50000", "true"));
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!((lines.len(), lines[1]), (50_001, "Company 1,c00001.example,https://c1.example,,,,,,,MT,,0.50,x,r"));
        assert!(lines[50_000].starts_with("Company 50000,c50000.example,"), "{}", lines[50_000]);

        // The API takes the rest as the next page, here as JSON lines.
        let (status, headers, body) = s.get(KEY, "/v1/companies/export?country=MT&page=2&format=jsonl").await;
        assert_eq!((status, header(&headers, "content-type")), (200, "application/jsonl"));
        assert_eq!((header(&headers, "splee-rows"), header(&headers, "splee-capped")), ("2", "false"));
        let rows: Vec<Value> = body.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
        assert_eq!(rows.iter().map(|r| r["domain"].as_str().unwrap()).collect::<Vec<_>>(), ["c50001.example", "c50002.example"]);
        assert_eq!(rows[0]["country_code"], "MT");
        let (_, headers, body) = s.get(KEY, "/v1/companies/export?country=MT&per_page=3&page=2").await;
        assert_eq!((header(&headers, "splee-rows"), header(&headers, "splee-capped")), ("3", "true"));
        assert_eq!(body.lines().nth(1), Some("Company 4,c00004.example,https://c4.example,,,,,,,MT,,0.50,x,r"));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_organization_gets_twenty_downloads_an_hour_each_in_its_audit_log(db: PgPool) {
        dentists(&db).await;
        let s = Server::start(&db).await;
        let [acme, beta] = s.orgs;

        assert_eq!(s.get(KEY, "/v1/companies/export?q=dental&has_email=true").await.0, 200);
        for _ in 0..19 {
            assert_eq!(s.get("acme", "/directory/companies/export?q=dental&domains=smile.co.uk,kayl.lu").await.0, 200);
        }
        let (status, headers, body) = s.get("acme", "/directory/companies/export?q=dental").await;
        assert_eq!(status, 429, "{body}");
        assert!(body.contains("This organization made 20 company downloads this hour"), "{body}");
        assert!(header(&headers, "retry-after").parse::<u64>().unwrap() > 3500);
        assert_eq!(s.get(KEY, "/v1/companies/export").await.0, 429, "the API shares the organization's hour");
        assert_eq!(s.get("acme", "/directory/companies/export/count?q=dental").await.0, 200, "counting isn't a download");
        assert_eq!(s.get("beta", "/directory/companies/export?q=dental").await.0, 200, "another organization has its own hour");

        let events: Vec<(Uuid, Option<Uuid>, Option<Uuid>, Value)> = sqlx::query_as(
            "select org_id, actor_user_id, actor_key_id, details from audit_events where action = 'companies.downloaded' order by created_at, id",
        )
        .fetch_all(&db)
        .await
        .unwrap();
        assert_eq!(events.iter().filter(|e| e.0 == acme).count(), 20, "a refused download isn't recorded");
        assert_eq!(events.iter().filter(|e| e.0 == beta).count(), 1);
        let by_key = events.iter().find(|e| e.2.is_some()).unwrap();
        assert_eq!((by_key.0, by_key.1), (acme, None));
        assert_eq!(by_key.3, json!({
            "filters": {
                "q": "dental", "country": null, "city": null, "category": null, "has_email": true, "has_phone": false, "tech": null,
                "project_id": null, "include_hidden": false,
            },
            "selected": null, "format": "csv", "page": 1, "rows": 3, "capped": false,
        }));
        let ticked = events.iter().find(|e| e.0 == acme && e.1.is_some()).unwrap();
        assert_eq!((ticked.3["selected"].clone(), ticked.3["rows"].clone()), (json!(2), json!(2)));
        assert!(!ticked.3.to_string().contains("smile"), "the log keeps the filters and counts, never the companies: {}", ticked.3);
    }
}
