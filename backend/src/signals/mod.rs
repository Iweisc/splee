//! Funding and company news: each project's companies are checked for Form D filings in SEC EDGAR (edgar.rs) and for posts on
//! their own blog or newsroom feed (feeds.rs), each source at most once a week per company. What's found is kept in
//! company_signals, scores the company for 90 days, and gives the email writer something true and recent to open with.

pub mod edgar;
pub mod feeds;

use std::collections::HashMap;
use std::time::Duration;

use chrono::{Datelike, NaiveDate};
use futures::{StreamExt, stream};
use quick_xml::events::BytesRef;
use serde::Serialize;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::config::Config;
use crate::hooks::{self, ChatAlert};
use edgar::Edgar;
use feeds::Feeds;

/// A signal stops counting this many days after its date, in the score, for the writer and on the lead.
pub const WINDOW_DAYS: i32 = 90;
/// Each source checks a company at most this often. Funding rounds and company posts are weekly news at most.
const RECHECK_DAYS: i32 = 7;
/// Companies each source checks per project a day, oldest-checked first. A Form D check is one EDGAR search plus one request per
/// filing found, and a feed check one to five page fetches the first time and one after.
pub const DAILY_COMPANIES: i64 = 300;
/// Companies checked at the same time. EDGAR's own limiter spaces its requests whatever this is.
const PARALLEL: usize = 4;
/// A filing this recent, at a company in a running campaign, pings the team when the project asks for it. Older filings found on a
/// first check are kept quietly.
const ALERT_DAYS: i64 = 14;

pub const KINDS: [&str; 6] = ["funding_filing", "funding_post", "launch", "acquisition", "expansion", "leadership"];

/// A company to check, with what Splee knows about where it is.
#[derive(Debug, Clone, FromRow)]
pub struct Target {
    pub domain: String,
    pub name: String,
    pub city: String,
    /// The company database's region, like "CA" or "California".
    pub region: String,
}

/// A signal a source found.
#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub kind: &'static str,
    pub key: String,
    pub happened_on: NaiveDate,
    pub title: String,
    pub amount: Option<i64>,
    pub offered: Option<i64>,
    pub industry: String,
    pub url: String,
}

pub struct Sources {
    /// None without SEC_USER_AGENT, which turns Form D checks off.
    pub edgar: Option<Edgar>,
    pub feeds: Feeds,
    public_url: String,
}

impl Sources {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        Ok(Self {
            edgar: Edgar::new(cfg.sec_user_agent.as_deref(), &cfg.sec_api_url)?,
            feeds: Feeds::new(&cfg.news_sites_url)?,
            public_url: cfg.public_url.clone(),
        })
    }
}

pub fn spawn_worker(db: PgPool, cfg: &Config) {
    let sources = match Sources::new(cfg) {
        Ok(sources) => sources,
        Err(e) => return tracing::error!(error = ?e, "could not start the company news worker"),
    };
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(3600), || tick(&db, &sources, DAILY_COMPANIES), "company news").await });
}

/// Checks the due companies of every project for each source that's on. Demo projects are left out.
pub async fn tick(db: &PgPool, sources: &Sources, daily: i64) -> anyhow::Result<()> {
    let projects: Vec<Uuid> = sqlx::query_scalar("select id from projects where org_id is not null order by created_at").fetch_all(db).await?;
    for project_id in projects {
        // Held until this project's checks are done, so a second backend on the database skips it instead of checking it twice.
        let mut lock = db.begin().await?;
        let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('signals:' || $1::text, 0))")
            .bind(project_id)
            .fetch_one(&mut *lock)
            .await?;
        if !owned {
            continue;
        }
        for source in ["sec", "feed"] {
            if source == "sec" && sources.edgar.is_none() {
                continue;
            }
            let checks = stream::iter(due(db, project_id, source, daily).await?)
                .map(|target| async move { check(db, sources, project_id, source, &target).await })
                .buffer_unordered(PARALLEL);
            checks.collect::<Vec<_>>().await.into_iter().collect::<anyhow::Result<()>>()?;
        }
        lock.commit().await?;
    }
    Ok(())
}

/// The project's company and lead domains this source hasn't checked in RECHECK_DAYS, oldest-checked first, up to what's left of
/// today's `daily`. Form D checks need a name and a city or region to match filers on, and skip companies known to be outside the
/// US, which don't file them.
async fn due(db: &PgPool, project_id: Uuid, source: &str, daily: i64) -> sqlx::Result<Vec<Target>> {
    sqlx::query_as(
        "with named as (
           select distinct on (lower(domain::text)) domain, name, city, country_code from (
             select domain, name, city, country_code, 0 as rank from companies where project_id = $1 and status <> 'excluded'
             union all
             select company_domain, company_name, '', '', 1 from leads where project_id = $1 and company_domain is not null and company_domain <> ''
           ) t order by lower(domain::text), rank, name = '', name
         ), targets as (
           select n.domain, coalesce(nullif(n.name, ''), d.name, '') as name, coalesce(nullif(n.city, ''), d.city, '') as city,
                  coalesce(d.region, '') as region, coalesce(nullif(n.country_code, ''), d.country_code, '') as country
           from named n left join directory_companies d on d.domain = lower(n.domain::text)
         )
         select t.domain::text, t.name, t.city, t.region from targets t
         left join company_signal_checks k on k.project_id = $1 and k.domain = t.domain and k.source = $2
         where (k.updated_at is null or k.updated_at < now() - make_interval(days => $3))
           and ($2 <> 'sec' or (t.name <> '' and (t.city <> '' or t.region <> '') and upper(t.country) in ('', 'US')))
         order by k.updated_at nulls first, t.domain
         limit greatest(0, $4 - (select count(*) from company_signal_checks where project_id = $1 and source = $2 and updated_at >= now() - interval '1 day'))",
    )
    .bind(project_id)
    .bind(source)
    .bind(RECHECK_DAYS)
    .bind(daily)
    .fetch_all(db)
    .await
}

/// Asks the source about the company and keeps what it found. A source that can't be reached is tried again next week, like a
/// company with nothing new.
async fn check(db: &PgPool, sources: &Sources, project_id: Uuid, source: &str, target: &Target) -> anyhow::Result<()> {
    let today = chrono::Utc::now().date_naive();
    let found = match (source, &sources.edgar) {
        ("sec", Some(edgar)) => edgar.find(target, today).await,
        _ => feeds::find(db, &sources.feeds, &target.domain, today, WINDOW_DAYS.into()).await,
    };
    match found {
        Ok(found) => {
            for new in store(db, project_id, &target.domain, source, &found).await? {
                if new.kind == "funding_filing" && (today - new.happened_on).num_days() <= ALERT_DAYS {
                    alert(db, &sources.public_url, project_id, &target.domain, &new).await?;
                }
            }
        }
        Err(e) => tracing::info!(domain = %target.domain, source, error = %e, "could not check a company for news"),
    }
    sqlx::query(
        "insert into company_signal_checks (project_id, domain, source) values ($1, $2, $3)
         on conflict (project_id, domain, source) do update set updated_at = now()",
    )
    .bind(project_id)
    .bind(&target.domain)
    .bind(source)
    .execute(db)
    .await?;
    Ok(())
}

/// Upserts the signals by key and returns the ones that are new. A filing's amendment updates its amounts and link but keeps the
/// first filing's date, which is when the round was news.
async fn store(db: &PgPool, project_id: Uuid, domain: &str, source: &str, found: &[Found]) -> sqlx::Result<Vec<Found>> {
    let mut new = vec![];
    for f in found {
        let inserted: bool = sqlx::query_scalar(
            "insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, offered, industry, url)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
             on conflict (project_id, key) do update set title = excluded.title, amount = excluded.amount, offered = excluded.offered,
               industry = excluded.industry, url = excluded.url
             returning xmax = 0",
        )
        .bind(project_id)
        .bind(domain)
        .bind(f.kind)
        .bind(source)
        .bind(&f.key)
        .bind(f.happened_on)
        .bind(&f.title)
        .bind(f.amount)
        .bind(f.offered)
        .bind(&f.industry)
        .bind(&f.url)
        .fetch_one(db)
        .await?;
        if inserted {
            new.push(f.clone());
        }
    }
    Ok(new)
}

/// Pings the project's chats about a new Form D at a company in a running campaign, when the project turned it on. True when sent.
async fn alert(db: &PgPool, public_url: &str, project_id: Uuid, domain: &str, filing: &Found) -> anyhow::Result<bool> {
    let row: Option<(String, String)> = sqlx::query_as(
        "select coalesce((select nullif(c.name, '') from companies c where c.project_id = $1 and c.domain = $2::citext),
                         (select nullif(max(l.company_name), '') from leads l where l.project_id = $1 and l.company_domain = $2::citext), $2),
                (select s.name from segments s where s.project_id = $1 and s.status in ('scaling', 'working') and s.id in (
                   select segment_id from companies where project_id = $1 and domain = $2::citext
                   union select segment_id from leads where project_id = $1 and company_domain = $2::citext) order by s.position limit 1)
         from project_settings ps where ps.project_id = $1 and ps.funding_alerts",
    )
    .bind(project_id)
    .bind(domain)
    .fetch_optional(db)
    .await?
    .and_then(|(name, campaign): (String, Option<String>)| Some((name, campaign?)));
    let Some((name, campaign)) = row else { return Ok(false) };
    let today = chrono::Utc::now().date_naive();
    let summary = format!("{name} ({domain}) {}", lowercase_first(&funding_text(filing.amount, filing.offered, filing.happened_on, today)));
    let quote = format!("They're in your campaign {campaign}. A new round is a good moment to reach out.");
    let link = format!("{public_url}/app/p/{project_id}/companies?tab=news");
    let alert = ChatAlert { label: "Raised money", summary: &summary, quote: Some(&quote), link: &link, link_text: "See the news" };
    hooks::post_chat(db, project_id, &alert).await;
    Ok(true)
}

fn lowercase_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map(|c| c.to_lowercase().chain(chars).collect()).unwrap_or_default()
}

/// One of quick-xml's entity references as text. Feeds escape their HTML, and unknown HTML entities like &nbsp; read as a space.
fn entity(r: &BytesRef) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    match r.as_ref() {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => " ",
    }
    .to_string()
}

/// "$12M", "$1.5M", "$750K", "$2B".
pub fn dollars(amount: i64) -> String {
    let (value, unit) = match amount {
        a if a >= 1_000_000_000 => (a as f64 / 1e9, "B"),
        a if a >= 1_000_000 => (a as f64 / 1e6, "M"),
        a if a >= 1_000 => (a as f64 / 1e3, "K"),
        a => return format!("${a}"),
    };
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 { format!("${rounded:.0}{unit}") } else { format!("${rounded:.1}{unit}") }
}

/// "Oct 3", with the year when it isn't this year's.
pub fn short_date(on: NaiveDate, today: NaiveDate) -> String {
    if on.year() == today.year() { on.format("%b %-d").to_string() } else { on.format("%b %-d, %Y").to_string() }
}

fn funding_text(amount: Option<i64>, offered: Option<i64>, on: NaiveDate, today: NaiveDate) -> String {
    let when = short_date(on, today);
    match (amount, offered) {
        (Some(sold), _) => format!("Raised {} (SEC filing, {when})", dollars(sold)),
        (None, Some(offered)) => format!("Raising {} (SEC filing, {when})", dollars(offered)),
        _ => format!("Filed to raise money (SEC filing, {when})"),
    }
}

/// Titles longer than this are cut in reasons, where they share a line with others.
const HEADLINE_TITLE_CHARS: usize = 70;

/// What the signal says in a line, for score reasons, the writer and alerts: "Raised $12M (SEC filing, Oct 3)" or
/// "Expanding: Acme opens its Austin office (Oct 3)".
pub fn headline(kind: &str, title: &str, amount: Option<i64>, offered: Option<i64>, on: NaiveDate, today: NaiveDate) -> String {
    if kind == "funding_filing" {
        return funding_text(amount, offered, on, today);
    }
    let label = match kind {
        "funding_post" => "Funding news",
        "acquisition" => "Acquisition",
        "expansion" => "Expanding",
        "leadership" => "New leader",
        _ => "Launched",
    };
    let title = title.trim();
    let title = match title.char_indices().nth(HEADLINE_TITLE_CHARS) {
        Some((cut, _)) => format!("{}…", title[..cut].trim_end()),
        None => title.to_string(),
    };
    format!("{label}: {title} ({})", short_date(on, today))
}

/// The signal a company leads with: its newest funding signal in the window, else its newest one.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Brief {
    #[serde(skip)]
    pub domain: String,
    pub kind: String,
    pub title: String,
    pub amount: Option<i64>,
    pub offered: Option<i64>,
    pub happened_on: NaiveDate,
    pub url: String,
    #[sqlx(skip)]
    pub headline: String,
}

/// The leading signal of each of these domains with one in the window, by lowercase domain.
pub async fn leading(db: &PgPool, project_id: Uuid, domains: &[String]) -> sqlx::Result<HashMap<String, Brief>> {
    let rows: Vec<Brief> = sqlx::query_as(
        "select distinct on (lower(domain::text)) lower(domain::text) as domain, kind, title, amount, offered, happened_on, url
         from company_signals
         where project_id = $1 and domain = any($2::citext[]) and happened_on > current_date - $3
         order by lower(domain::text), kind in ('funding_filing', 'funding_post') desc, happened_on desc, first_seen desc",
    )
    .bind(project_id)
    .bind(domains)
    .bind(WINDOW_DAYS)
    .fetch_all(db)
    .await?;
    let today = chrono::Utc::now().date_naive();
    Ok(rows.into_iter().map(|mut b| (b.domain.clone(), { b.headline = headline(&b.kind, &b.title, b.amount, b.offered, b.happened_on, today); b })).collect())
}

/// A signal for the News tab, the API and the lead's page. company_id is set when the company is in the project, leads counts the
/// project's leads on its domain, and checked_at is when its source last looked at the company.
#[derive(Debug, Serialize, FromRow)]
pub struct Listed {
    pub id: Uuid,
    pub domain: String,
    pub company_id: Option<Uuid>,
    pub company_name: String,
    pub leads: i64,
    pub kind: String,
    pub source: String,
    pub happened_on: NaiveDate,
    pub title: String,
    pub amount: Option<i64>,
    pub offered: Option<i64>,
    pub industry: String,
    pub url: String,
    pub first_seen: crate::models::Ts,
    pub checked_at: Option<crate::models::Ts>,
    #[sqlx(skip)]
    pub headline: String,
}

/// The page and the API show this many at most, newest first.
pub const MAX_LISTED: i64 = 1000;

/// The project's signals from the last `days` days (every kept one without it), of `kind` when given, newest first. With `domain`,
/// only that company's.
pub async fn list(db: &PgPool, project_id: Uuid, kind: Option<&str>, days: Option<i32>, domain: Option<&str>) -> sqlx::Result<Vec<Listed>> {
    let mut rows: Vec<Listed> = sqlx::query_as(
        "select g.id, g.domain::text, c.id as company_id, coalesce(nullif(c.name, ''), l.company_name, g.domain::text) as company_name,
                coalesce(l.leads, 0) as leads, g.kind, g.source, g.happened_on, g.title, g.amount, g.offered, g.industry, g.url, g.first_seen,
                k.updated_at as checked_at
         from company_signals g
         left join companies c on c.project_id = g.project_id and c.domain = g.domain
         left join lateral (select count(*) as leads, nullif(max(company_name), '') as company_name from leads
                            where project_id = g.project_id and company_domain = g.domain) l on true
         left join company_signal_checks k on k.project_id = g.project_id and k.domain = g.domain and k.source = g.source
         where g.project_id = $1 and ($2::text is null or g.kind = $2) and ($3::int is null or g.happened_on > current_date - $3)
           and ($4::text is null or g.domain = $4::citext)
         order by g.happened_on desc, g.first_seen desc, company_name
         limit $5",
    )
    .bind(project_id)
    .bind(kind)
    .bind(days)
    .bind(domain)
    .bind(MAX_LISTED)
    .fetch_all(db)
    .await?;
    let today = chrono::Utc::now().date_naive();
    for r in &mut rows {
        r.headline = headline(&r.kind, &r.title, r.amount, r.offered, r.happened_on, today);
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, Query, State};
    use axum::http::{HeaderMap, StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::{get, post};
    use serde_json::{Value, json};
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::edgar::tests::{hit, primary_doc};
    use super::*;

    /// EDGAR, company sites under /site/{host}/..., and a chat webhook, on one server. Every request is kept with its User-Agent.
    #[derive(Clone, Default)]
    struct Fake {
        /// Search hits by the quoted name searched for.
        hits: Arc<Mutex<HashMap<String, Vec<Value>>>>,
        docs: Arc<Mutex<HashMap<String, String>>>,
        pages: Arc<Mutex<HashMap<String, String>>>,
        requests: Arc<Mutex<Vec<(String, String, tokio::time::Instant)>>>,
        chats: Arc<Mutex<Vec<Value>>>,
    }

    impl Fake {
        fn page(&self, path: &str, body: &str) {
            self.pages.lock().unwrap().insert(path.into(), body.into());
        }

        fn requests(&self, prefix: &str) -> Vec<(String, String, tokio::time::Instant)> {
            self.requests.lock().unwrap().iter().filter(|r| r.0.starts_with(prefix)).cloned().collect()
        }
    }

    fn keep(fake: &Fake, uri: &Uri, headers: &HeaderMap) {
        let ua = headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
        fake.requests.lock().unwrap().push((uri.path().to_string(), ua, tokio::time::Instant::now()));
    }

    async fn search(State(fake): State<Fake>, uri: Uri, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
        keep(&fake, &uri, &headers);
        assert_eq!((q["forms"].as_str(), q["dateRange"].as_str()), ("D,D/A", "custom"));
        let hits = fake.hits.lock().unwrap().get(&q["q"]).cloned().unwrap_or_default();
        axum::Json(json!({ "hits": { "total": { "value": hits.len() }, "hits": hits } })).into_response()
    }

    async fn archive(State(fake): State<Fake>, uri: Uri, headers: HeaderMap) -> Response {
        keep(&fake, &uri, &headers);
        match fake.docs.lock().unwrap().get(uri.path()) {
            Some(xml) => xml.clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn site(State(fake): State<Fake>, uri: Uri, headers: HeaderMap) -> Response {
        keep(&fake, &uri, &headers);
        // Feed links on a page are often absolute paths, which land outside /site/.
        let path = uri.path().strip_prefix("/site/").unwrap_or(uri.path()).trim_matches('/');
        match fake.pages.lock().unwrap().get(path) {
            Some(body) => body.clone().into_response(),
            None => StatusCode::NOT_FOUND.into_response(),
        }
    }

    async fn chat(State(fake): State<Fake>, Path(_): Path<String>, axum::Json(body): axum::Json<Value>) -> StatusCode {
        fake.chats.lock().unwrap().push(body);
        StatusCode::OK
    }

    async fn serve() -> (Fake, String) {
        let fake = Fake::default();
        let app = Router::new()
            .route("/LATEST/search-index", get(search))
            .route("/Archives/{*path}", get(archive))
            .route("/site/{*path}", get(site))
            .route("/chat/{id}", post(chat))
            .fallback(get(site))
            .with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        hooks::TEST_RECEIVERS.lock().unwrap().push(port);
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (fake, format!("http://127.0.0.1:{port}"))
    }

    fn sources(base: &str, user_agent: Option<&str>) -> Sources {
        Sources { edgar: Edgar::new(user_agent, base).unwrap(), feeds: Feeds::new(base).unwrap(), public_url: "https://splee.example".into() }
    }

    async fn project(db: &PgPool) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
        project
    }

    async fn company(db: &PgPool, project: Uuid, domain: &str, name: &str, city: &str, country: &str) {
        sqlx::query("insert into companies (project_id, domain, name, city, country_code) values ($1, $2, $3, $4, $5)")
            .bind(project)
            .bind(domain)
            .bind(name)
            .bind(city)
            .bind(country)
            .execute(db)
            .await
            .unwrap();
    }

    fn days_ago(days: i64) -> NaiveDate {
        chrono::Utc::now().date_naive() - chrono::Duration::days(days)
    }

    fn rss(items: &[(&str, &str, NaiveDate)]) -> String {
        let items: String = items
            .iter()
            .map(|(title, link, on)| format!("<item><title>{title}</title><link>{link}</link><pubDate>{}</pubDate></item>", on.and_hms_opt(9, 0, 0).unwrap().and_utc().to_rfc2822()))
            .collect();
        format!("<?xml version=\"1.0\"?><rss version=\"2.0\"><channel><title>Blog</title>{items}</channel></rss>")
    }

    async fn kinds(db: &PgPool, project: Uuid) -> Vec<(String, String, Option<i64>)> {
        sqlx::query_as("select domain::text, kind, amount from company_signals where project_id = $1 order by domain, happened_on desc")
            .bind(project)
            .fetch_all(db)
            .await
            .unwrap()
    }

    #[test]
    fn headlines_say_what_happened_and_when() {
        let today = NaiveDate::from_ymd_opt(2026, 11, 13).unwrap();
        let oct3 = NaiveDate::from_ymd_opt(2026, 10, 3).unwrap();
        assert_eq!(headline("funding_filing", "Acme Robotics, Inc.", Some(12_000_000), Some(15_000_000), oct3, today), "Raised $12M (SEC filing, Oct 3)");
        assert_eq!(headline("funding_filing", "", None, Some(1_500_000), oct3, today), "Raising $1.5M (SEC filing, Oct 3)");
        assert_eq!(headline("funding_filing", "", None, None, NaiveDate::from_ymd_opt(2025, 12, 20).unwrap(), today), "Filed to raise money (SEC filing, Dec 20, 2025)");
        assert_eq!(headline("expansion", "Acme opens its Austin office", None, None, oct3, today), "Expanding: Acme opens its Austin office (Oct 3)");
        let long = "A".repeat(80);
        assert_eq!(headline("launch", &long, None, None, oct3, today), format!("Launched: {}… (Oct 3)", "A".repeat(70)));
        assert_eq!((dollars(750_000), dollars(2_000_000_000), dollars(12_345_678), dollars(999)), ("$750K".into(), "$2B".into(), "$12.3M".into(), "$999".into()));
    }

    #[tokio::test]
    async fn edgar_gets_a_named_user_agent_and_requests_are_spaced() {
        assert!(Edgar::new(None, "").unwrap().is_none(), "off without SEC_USER_AGENT");
        assert!(Edgar::new(Some("  "), "").unwrap().is_none());
        let (fake, base) = serve().await;
        let edgar = Edgar::new(Some("Splee news check admin@splee.example"), &base).unwrap().unwrap();
        let t = edgar::tests::target("Acme Robotics", "", "");
        let today = chrono::Utc::now().date_naive();
        futures::future::join_all((0..3).map(|_| edgar.find(&t, today))).await.into_iter().for_each(|r| assert!(r.unwrap().is_empty()));
        let searches = fake.requests("/LATEST");
        assert_eq!(searches.len(), 3);
        assert!(searches.iter().all(|(_, ua, _)| ua == "Splee news check admin@splee.example"));
        let mut at: Vec<_> = searches.iter().map(|r| r.2).collect();
        at.sort();
        assert!(at[2] - at[0] >= Duration::from_millis(480), "three requests take two gaps: {:?}", at[2] - at[0]);

        let limiter = edgar::Limiter::new(Duration::from_millis(40));
        let start = tokio::time::Instant::now();
        futures::future::join_all((0..5).map(|_| limiter.wait())).await;
        assert!(start.elapsed() >= Duration::from_millis(160));
    }

    #[tokio::test]
    async fn a_feed_is_found_from_the_homepage_link_or_a_common_path() {
        let (fake, base) = serve().await;
        let feeds = Feeds::new(&base).unwrap();
        let feed = rss(&[("Hello", "https://acme.example/hello", days_ago(1))]);
        fake.page("linked.example", r#"<html><head><link rel="alternate" type="application/atom+xml" href="/news/atom.xml"></head></html>"#);
        assert_eq!(feeds.discover("linked.example").await, Some(format!("{base}/news/atom.xml")));
        // No link, so the paths are tried in order on the host that answered.
        fake.page("www.pathed.example", "<html>No feed link</html>");
        fake.page("www.pathed.example/rss.xml", &feed);
        fake.page("www.pathed.example/blog/rss.xml", &feed);
        assert_eq!(feeds.discover("pathed.example").await, Some(format!("{base}/site/www.pathed.example/rss.xml")));
        // A page at a feed's path that isn't a feed doesn't count.
        fake.page("none.example", "<html>Home</html>");
        fake.page("none.example/feed", "<html>Our feed of updates</html>");
        assert_eq!(feeds.discover("none.example").await, None);
        let tried: Vec<String> = fake.requests("/site/none.example").into_iter().map(|r| r.0).collect();
        assert_eq!(tried, ["/site/none.example", "/site/none.example/feed", "/site/none.example/rss.xml", "/site/none.example/blog/rss.xml"]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn filings_and_posts_become_signals_and_each_source_waits_a_week(db: PgPool) {
        let (fake, base) = serve().await;
        let sources = sources(&base, Some("Splee news check admin@splee.example"));
        let p = project(&db).await;
        company(&db, p, "acme.example", "Acme Robotics, Inc.", "San Francisco", "US").await;
        company(&db, p, "notion.example", "Notion", "", "").await;
        company(&db, p, "texan.example", "Texan Labs", "", "").await;
        company(&db, p, "berlin.example", "Berlin Robotics GmbH", "Berlin", "DE").await;
        sqlx::query("insert into directory_companies (domain, name, website, region, country_code, source_id, release)
                     values ('texan.example', 'Texan Labs', 'https://texan.example', 'TX', 'US', 'x', 'r')")
            .execute(&db)
            .await
            .unwrap();
        let (filed, amended) = (days_ago(10).to_string(), days_ago(3).to_string());
        fake.hits.lock().unwrap().extend([
            ("\"acme robotics\"".to_string(), vec![
                hit("0001234567-26-000002", "0001234567", "Acme Robotics, Inc.", "D/A", &amended, "021-111", "San Francisco, CA"),
                hit("0001234567-26-000001", "0001234567", "Acme Robotics, Inc.", "D", &filed, "021-111", "San Francisco, CA"),
            ]),
            // Nothing known about the place, so it is never searched for.
            ("\"notion\"".to_string(), vec![hit("0000000001-26-000001", "0000000001", "Notion, Inc.", "D", &filed, "021-1", "San Francisco, CA")]),
            // The company database says Texas, the filer is in California.
            ("\"texan labs\"".to_string(), vec![hit("0000000002-26-000001", "0000000002", "Texan Labs, Inc.", "D", &filed, "021-2", "Palo Alto, CA")]),
        ]);
        fake.docs.lock().unwrap().insert(
            "/Archives/edgar/data/1234567/000123456726000002/primary_doc.xml".into(),
            primary_doc("Acme Robotics, Inc.", "Other Technology", "15000000", "12000000"),
        );
        fake.page("acme.example", r#"<link rel="alternate" type="application/rss+xml" href="/blog/rss.xml">"#);
        fake.page("blog/rss.xml", &rss(&[
            ("Acme opens a new office in Austin", "/blog/austin", days_ago(5)),
            ("Five tips for spring", "/blog/tips", days_ago(6)),
            ("Acme acquires Brightline", "/blog/brightline", days_ago(120)),
        ]));

        tick(&db, &sources, 300).await.unwrap();
        assert_eq!(kinds(&db, p).await, [
            ("acme.example".to_string(), "expansion".to_string(), None),
            ("acme.example".to_string(), "funding_filing".to_string(), Some(12_000_000)),
        ]);
        let (on, url, title, industry): (NaiveDate, String, String, String) =
            sqlx::query_as("select happened_on, url, title, industry from company_signals where kind = 'funding_filing'").fetch_one(&db).await.unwrap();
        assert_eq!(on, days_ago(10), "the first filing's date, with the amendment's amounts");
        assert_eq!(url, format!("{base}/Archives/edgar/data/1234567/000123456726000002/0001234567-26-000002-index.htm"));
        assert_eq!((title.as_str(), industry.as_str()), ("Acme Robotics, Inc.", "Other Technology"));
        let post: String = sqlx::query_scalar("select url from company_signals where kind = 'expansion'").fetch_one(&db).await.unwrap();
        assert_eq!(post, format!("{base}/blog/austin"));
        // Notion and the German company were never searched for.
        assert!(fake.requests("/LATEST").len() == 2);

        // Within the week nothing is asked again, and after it each source checks again, finding the same signals once.
        let searches = fake.requests("/LATEST").len();
        tick(&db, &sources, 300).await.unwrap();
        assert_eq!(fake.requests("/LATEST").len(), searches);
        sqlx::query("update company_signal_checks set updated_at = now() - interval '6 days 23 hours'").execute(&db).await.unwrap();
        tick(&db, &sources, 300).await.unwrap();
        assert_eq!(fake.requests("/LATEST").len(), searches);
        sqlx::query("update company_signal_checks set updated_at = now() - interval '7 days 1 hour'").execute(&db).await.unwrap();
        tick(&db, &sources, 300).await.unwrap();
        assert_eq!(fake.requests("/LATEST").len(), searches + 2);
        assert_eq!(kinds(&db, p).await.len(), 2);
        let checks: Vec<(String, String)> =
            sqlx::query_as("select domain::text, source from company_signal_checks where project_id = $1 order by domain, source").bind(p).fetch_all(&db).await.unwrap();
        assert_eq!(checks.len(), 6, "four feeds and two searches: {checks:?}");
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_daily_cap_and_sec_off_without_a_user_agent(db: PgPool) {
        let (fake, base) = serve().await;
        let p = project(&db).await;
        for domain in ["a.example", "b.example", "c.example"] {
            company(&db, p, domain, "Some Company", "", "").await;
        }
        tick(&db, &sources(&base, None), 2).await.unwrap();
        assert!(fake.requests("/LATEST").is_empty());
        let feed_checks: Vec<String> = sqlx::query_scalar("select domain::text from company_signal_checks where source = 'feed' order by domain").fetch_all(&db).await.unwrap();
        assert_eq!(feed_checks, ["a.example", "b.example"]);
        tick(&db, &sources(&base, None), 2).await.unwrap();
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from company_signal_checks").fetch_one(&db).await.unwrap(), 2);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_new_filing_pings_the_team_only_for_running_campaigns_when_asked(db: PgPool) {
        let (fake, base) = serve().await;
        let p = project(&db).await;
        company(&db, p, "acme.example", "Acme Robotics", "", "").await;
        sqlx::query("insert into chat_channels (project_id, url) values ($1, $2)").bind(p).bind(format!("{base}/chat/1")).execute(&db).await.unwrap();
        let filing = Found {
            kind: "funding_filing",
            key: "sec:1:021-1".into(),
            happened_on: days_ago(2),
            title: "Acme Robotics, Inc.".into(),
            amount: Some(12_000_000),
            offered: None,
            industry: String::new(),
            url: "https://www.sec.gov/x".into(),
        };
        let send = || alert(&db, "https://splee.example", p, "acme.example", &filing);
        assert!(!send().await.unwrap(), "alerts are off");
        sqlx::query("update project_settings set funding_alerts = true where project_id = $1").bind(p).execute(&db).await.unwrap();
        assert!(!send().await.unwrap(), "not in a campaign");
        let segment: Uuid = sqlx::query_scalar("insert into segments (project_id, name, status) values ($1, 'Robotics CFOs', 'draft') returning id")
            .bind(p)
            .fetch_one(&db)
            .await
            .unwrap();
        sqlx::query("update companies set segment_id = $1").bind(segment).execute(&db).await.unwrap();
        assert!(!send().await.unwrap(), "the campaign isn't running");
        sqlx::query("update segments set status = 'working'").execute(&db).await.unwrap();
        assert!(send().await.unwrap());
        let chats = fake.chats.lock().unwrap().clone();
        assert_eq!(chats.len(), 1);
        let text = chats[0].to_string();
        assert!(text.contains(&format!("Acme Robotics (acme.example) raised $12M (SEC filing, {})", short_date(days_ago(2), days_ago(0)))), "{text}");
        assert!(text.contains("Robotics CFOs") && text.contains("companies?tab=news"), "{text}");
        // Another organization's chat never hears of it.
        let other = project(&db).await;
        sqlx::query("update project_settings set funding_alerts = true where project_id = $1").bind(other).execute(&db).await.unwrap();
        assert!(!alert(&db, "https://splee.example", other, "acme.example", &filing).await.unwrap());
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn signals_count_for_90_days_and_funding_leads(db: PgPool) {
        let p = project(&db).await;
        let other = project(&db).await;
        let add = |project: Uuid, domain: &'static str, kind: &'static str, key: &'static str, age: i64| {
            sqlx::query("insert into company_signals (project_id, domain, kind, source, key, happened_on, title, amount, url)
                         values ($1, $2, $3, 'feed', $4, current_date - $5::int, $4, case when $3 = 'funding_filing' then 5000000 end, 'https://x.example/' || $4)")
                .bind(project)
                .bind(domain)
                .bind(kind)
                .bind(key)
                .bind(age as i32)
                .execute(&db)
        };
        add(p, "acme.example", "launch", "launch", 2).await.unwrap();
        add(p, "acme.example", "funding_filing", "filing", 30).await.unwrap();
        add(p, "old.example", "funding_post", "old", 91).await.unwrap();
        add(other, "acme.example", "acquisition", "theirs", 1).await.unwrap();
        let leading = leading(&db, p, &["ACME.example".into(), "old.example".into()]).await.unwrap();
        assert_eq!(leading.len(), 1, "old.example's is past 90 days");
        assert_eq!(leading["acme.example"].kind, "funding_filing", "funding leads even when older");
        assert!(leading["acme.example"].headline.starts_with("Raised $5M (SEC filing, "));

        let listed = list(&db, p, None, None, None).await.unwrap();
        assert_eq!(listed.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["launch", "filing", "old"]);
        assert_eq!(list(&db, p, Some("launch"), Some(30), None).await.unwrap().len(), 1);
        assert_eq!(list(&db, p, None, Some(30), None).await.unwrap().len(), 1, "the filing is 30 days old");
        assert_eq!(list(&db, other, None, None, None).await.unwrap().len(), 1);
    }
}
