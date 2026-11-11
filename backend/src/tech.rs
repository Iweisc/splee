//! Tech-stack signals: each project's company domains are scanned once for the tools they use (fingerprints.rs), and
//! again after 30 days. A scan is one homepage fetch plus the domain's MX and TXT records, kept per domain in tech_scans,
//! so a company in several projects or organizations is fetched once. The whole worker scans at most 300 domains an hour.

use std::collections::HashMap;
use std::time::Duration;

use futures::{StreamExt, stream};
use hickory_resolver::TokioResolver;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::fingerprints::{self, CATALOGUE, Evidence};
use crate::mail::dns;

/// Domains scanned an hour, across every project. Each costs one or two page fetches and two DNS lookups.
pub const HOURLY_DOMAINS: i64 = 300;
const PARALLEL: usize = 4;
const RESCAN_DAYS: i32 = 30;
const TIMEOUT: Duration = Duration::from_secs(8);
const USER_AGENT: &str = "Mozilla/5.0 (compatible; Splee tech check)";
/// The head and the scripts near the top of the body are what matter, so a longer page is cut here rather than skipped.
const MAX_PAGE_BYTES: usize = 2 << 20;

pub struct Scanner {
    client: reqwest::Client,
    /// The homepage URL template, with {domain}.
    site: String,
    /// Off in tests only, whose fake site is on this machine.
    public_only: bool,
    /// None in tests, which check DNS matching on fixture answers instead.
    resolver: Option<TokioResolver>,
}

impl Scanner {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder().user_agent(USER_AGENT).timeout(TIMEOUT).connect_timeout(Duration::from_secs(4)).build()?,
            site: "https://{domain}".into(),
            public_only: true,
            resolver: Some(dns::resolver()?),
        })
    }

    #[cfg(test)]
    pub fn at(base: &str) -> Self {
        Self {
            client: reqwest::Client::builder().timeout(Duration::from_secs(5)).build().unwrap(),
            site: format!("{base}/site/{{domain}}"),
            public_only: false,
            resolver: None,
        }
    }

    /// The tools found, and whether the homepage could be read.
    pub async fn scan(&self, domain: &str) -> (Vec<&'static str>, bool) {
        let (page, (mx, txt)) = tokio::join!(self.homepage(domain), async {
            match &self.resolver {
                Some(r) => tokio::join!(dns::mx_exchanges(r, domain), dns::txt_records(r, domain)),
                None => (vec![], vec![]),
            }
        });
        let read = page.is_some();
        let (headers, html) = page.unwrap_or_default();
        (fingerprints::detect(&Evidence { html, headers, mx, txt }), read)
    }

    /// The headers and start of the domain's homepage, or of www's when the bare domain doesn't answer.
    async fn homepage(&self, domain: &str) -> Option<(Vec<(String, String)>, String)> {
        let www = (!domain.starts_with("www.")).then(|| format!("www.{domain}"));
        for host in std::iter::once(domain).chain(www.as_deref()) {
            let url = self.site.replace("{domain}", host);
            let Some(mut res) = crate::hooks::get_page(&self.client, &url, self.public_only, USER_AGENT, TIMEOUT).await else { continue };
            let headers = res.headers().iter().map(|(n, v)| (n.as_str().to_string(), String::from_utf8_lossy(v.as_bytes()).into_owned())).collect();
            let mut body = Vec::new();
            while body.len() < MAX_PAGE_BYTES {
                match res.chunk().await {
                    Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                    Ok(None) => break,
                    Err(_) if !body.is_empty() => break,
                    Err(_) => return None,
                }
            }
            body.truncate(MAX_PAGE_BYTES);
            return Some((headers, String::from_utf8_lossy(&body).into_owned()));
        }
        None
    }
}

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move {
        let scanner = match Scanner::new() {
            Ok(scanner) => scanner,
            Err(e) => return tracing::error!(error = ?e, "could not start the tech signals worker"),
        };
        crate::mail::every(Duration::from_secs(600), || tick(&db, &scanner, HOURLY_DOMAINS), "tech signals").await
    });
}

/// Scans the due domains of every project, oldest project first, until the hour's budget is spent. Demo projects are left out.
pub async fn tick(db: &PgPool, scanner: &Scanner, hourly: i64) -> anyhow::Result<()> {
    let projects: Vec<Uuid> = sqlx::query_scalar("select id from projects where org_id is not null order by created_at").fetch_all(db).await?;
    for project_id in projects {
        // Held until this project's scans are done, so a second backend on the database moves on to another project.
        let mut lock = db.begin().await?;
        let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('tech:' || $1::text, 0))")
            .bind(project_id)
            .fetch_one(&mut *lock)
            .await?;
        if !owned {
            continue;
        }
        let scans = stream::iter(claim(db, project_id, hourly).await?)
            .map(|domain| async move {
                let (techs, read) = scanner.scan(&domain).await;
                store(db, &domain, &techs, read).await
            })
            .buffer_unordered(PARALLEL);
        scans.collect::<Vec<_>>().await.into_iter().collect::<sqlx::Result<()>>()?;
        lock.commit().await?;
    }
    Ok(())
}

/// Takes the project's due domains, never-scanned first then the longest-waiting, up to what's left of the hour's budget.
/// A domain taken a day ago whose scan never finished, because the backend stopped, is due again.
async fn claim(db: &PgPool, project_id: Uuid, hourly: i64) -> sqlx::Result<Vec<String>> {
    let mut tx = db.begin().await?;
    // Every backend counts and takes under this lock, so two of them can't both spend the same part of the budget.
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('tech:budget', 0))").execute(&mut *tx).await?;
    let taken = sqlx::query_scalar(
        "with domains as (
           select domain from companies where project_id = $1 and status <> 'excluded'
           union select company_domain from leads where project_id = $1 and company_domain is not null and company_domain <> ''
         ), due as (
           select d.domain, t.checked_at from domains d left join tech_scans t on t.domain = d.domain
           where t.domain is null or t.checked_at < now() - make_interval(days => $3)
              or (t.checked_at < now() - interval '1 day' and (t.scanned_at is null or t.scanned_at < t.checked_at))
           order by t.checked_at nulls first, d.domain
           limit greatest(0, $2 - (select count(*) from tech_scans where checked_at > now() - interval '1 hour'))
         )
         insert into tech_scans (domain) select domain from due
         on conflict (domain) do update set checked_at = now()
         returning domain::text",
    )
    .bind(project_id)
    .bind(hourly)
    .bind(RESCAN_DAYS)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(taken)
}

/// A homepage that couldn't be read keeps what earlier scans found on it, so a site that's down for a day keeps its badges.
async fn store(db: &PgPool, domain: &str, found: &[&'static str], read: bool) -> sqlx::Result<()> {
    let mut techs: Vec<&str> = found.to_vec();
    if !read {
        let before: Vec<String> = sqlx::query_scalar("select unnest(techs) from tech_scans where domain = $1").bind(domain).fetch_all(db).await?;
        techs.extend(before.iter().filter_map(|name| fingerprints::find(name)).map(|t| t.name));
    }
    let position = |name: &&str| CATALOGUE.iter().position(|t| t.name == *name);
    techs.sort_by_key(position);
    techs.dedup();
    sqlx::query("update tech_scans set techs = $2, scanned_at = now() where domain = $1").bind(domain).bind(&techs).execute(db).await?;
    Ok(())
}

/// A tool the project looks for. kind is "uses" or "competitor".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, sqlx::FromRow)]
pub struct Pick {
    pub tech: String,
    pub kind: String,
}

pub async fn picks(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<Pick>> {
    sqlx::query_as("select tech, kind from project_tech_picks where project_id = $1 order by position, tech").bind(project_id).fetch_all(db).await
}

/// "Uses HubSpot and Intercom" for each of these domains whose scan found some of the project's picks, by lowercase domain.
/// Only picks, in the project's order, so the writer only mentions tools that matter to the offer.
pub async fn fact_lines(db: &PgPool, project_id: Uuid, domains: &[String]) -> sqlx::Result<HashMap<String, String>> {
    let rows: Vec<(String, Vec<String>)> = sqlx::query_as(
        "select t.domain::text, array(select p.tech from project_tech_picks p where p.project_id = $1 and p.tech = any(t.techs) order by p.position, p.tech)
         from tech_scans t where t.domain = any($2::citext[])",
    )
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().filter_map(|(domain, names)| Some((domain.to_lowercase(), fingerprints::fact_line(&names)?))).collect())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    use axum::Router;
    use axum::extract::{Path, State};
    use axum::http::{HeaderMap, StatusCode};
    use axum::response::{Html, IntoResponse, Response};
    use axum::routing::get;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;

    type Headers = Vec<(&'static str, &'static str)>;

    /// Homepages by host, each with its own headers, and every request counted by host.
    #[derive(Clone, Default)]
    struct Fake {
        pages: Arc<Mutex<HashMap<String, (Headers, String)>>>,
        hits: Arc<Mutex<Vec<String>>>,
    }

    impl Fake {
        fn page(&self, host: &str, headers: &[(&'static str, &'static str)], html: &str) {
            self.pages.lock().unwrap().insert(host.into(), (headers.to_vec(), html.into()));
        }

        fn hits(&self, host: &str) -> usize {
            self.hits.lock().unwrap().iter().filter(|h| *h == host).count()
        }
    }

    async fn page(State(fake): State<Fake>, Path(host): Path<String>) -> Response {
        fake.hits.lock().unwrap().push(host.clone());
        match fake.pages.lock().unwrap().get(&host) {
            Some((headers, html)) => {
                let mut map = HeaderMap::new();
                for (name, value) in headers {
                    map.append(*name, value.parse().unwrap());
                }
                (map, Html(html.clone())).into_response()
            }
            None => (StatusCode::NOT_FOUND, "Not Found").into_response(),
        }
    }

    async fn serve() -> (Scanner, Fake) {
        let fake = Fake::default();
        let app = Router::new().route("/site/{host}", get(page)).with_state(fake.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (Scanner::at(&base), fake)
    }

    async fn project(db: &PgPool, domains: &[&str]) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        for domain in domains {
            sqlx::query("insert into companies (project_id, domain) values ($1, $2)").bind(project).bind(domain).execute(db).await.unwrap();
        }
        project
    }

    async fn techs(db: &PgPool, domain: &str) -> Vec<String> {
        sqlx::query_scalar("select techs from tech_scans where domain = $1").bind(domain).fetch_one(db).await.unwrap()
    }

    async fn scanned_last_minute(db: &PgPool) -> Vec<String> {
        sqlx::query_scalar("select domain::text from tech_scans where checked_at > now() - interval '1 minute' order by domain").fetch_all(db).await.unwrap()
    }

    #[tokio::test]
    async fn a_scan_reads_the_homepage_or_its_www_and_its_headers() {
        let (scanner, fake) = serve().await;
        fake.page(
            "acme.com",
            &[("server", "cloudflare"), ("set-cookie", "hubspotutk=1; Path=/"), ("set-cookie", "_ga=GA1; Path=/")],
            r#"<script src="https://widget.intercom.io/widget/abc"></script>"#,
        );
        assert_eq!(scanner.scan("acme.com").await, (vec!["HubSpot", "Intercom", "Google Analytics", "Cloudflare"], true));
        fake.page("www.wwwonly.com", &[], r#"<meta name="generator" content="Webflow">"#);
        assert_eq!(scanner.scan("wwwonly.com").await, (vec!["Webflow"], true));
        assert_eq!(scanner.scan("down.com").await, (vec![], false));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_domain_is_fetched_once_for_every_project_and_again_after_30_days(db: PgPool) {
        let (scanner, fake) = serve().await;
        fake.page("shared.com", &[], r#"<script src="https://js.stripe.com/v3"></script>"#);
        fake.page("solo.com", &[("x-vercel-id", "fra1")], "<p>Hi</p>");
        // Two organizations with the same company, and a lead's company that has no company row.
        let a = project(&db, &["shared.com"]).await;
        project(&db, &["shared.com", "solo.com"]).await;
        sqlx::query("insert into leads (project_id, full_name, email, company_domain) values ($1, 'Ana', 'ana@lead.com', 'lead.com')")
            .bind(a)
            .execute(&db)
            .await
            .unwrap();

        tick(&db, &scanner, 300).await.unwrap();
        assert_eq!(fake.hits("shared.com"), 1);
        assert_eq!(techs(&db, "shared.com").await, ["Stripe"]);
        assert_eq!(techs(&db, "solo.com").await, ["Vercel"]);
        // lead.com didn't answer, and neither did www.lead.com.
        assert_eq!((fake.hits("lead.com"), fake.hits("www.lead.com")), (1, 1));
        assert!(techs(&db, "lead.com").await.is_empty());

        // Nothing is due again within 30 days.
        sqlx::query("update tech_scans set checked_at = now() - interval '29 days', scanned_at = now() - interval '29 days'").execute(&db).await.unwrap();
        tick(&db, &scanner, 300).await.unwrap();
        assert_eq!(fake.hits("shared.com"), 1);

        // After 30 days it is, and a site that's down this time keeps what it had.
        sqlx::query("update tech_scans set checked_at = now() - interval '31 days', scanned_at = now() - interval '31 days'").execute(&db).await.unwrap();
        fake.pages.lock().unwrap().remove("shared.com");
        fake.page("solo.com", &[("x-nf-request-id", "01")], "<p>Moved</p>");
        tick(&db, &scanner, 300).await.unwrap();
        assert_eq!(fake.hits("shared.com"), 2);
        assert_eq!(techs(&db, "shared.com").await, ["Stripe"]);
        assert_eq!(techs(&db, "solo.com").await, ["Netlify"]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_hourly_budget_is_shared_and_a_locked_project_is_skipped(db: PgPool) {
        let (scanner, _) = serve().await;
        let first = project(&db, &["a.com", "b.com", "c.com"]).await;
        project(&db, &["d.com", "e.com"]).await;
        sqlx::query("insert into companies (project_id, domain, status) values ($1, 'skip.com', 'excluded')").bind(first).execute(&db).await.unwrap();
        // A demo project has no organization and is never scanned.
        let demo: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into companies (project_id, domain) values ($1, 'demo.com')").bind(demo).execute(&db).await.unwrap();

        // Another backend is scanning the first project, so this one only takes the second's.
        let mut other = db.begin().await.unwrap();
        sqlx::query("select pg_advisory_xact_lock(hashtextextended('tech:' || $1::text, 0))").bind(first).execute(&mut *other).await.unwrap();
        tick(&db, &scanner, 4).await.unwrap();
        assert_eq!(scanned_last_minute(&db).await, ["d.com", "e.com"]);
        other.rollback().await.unwrap();

        // Two of the hour's four are spent, so two of the first project's three go now and the last waits.
        tick(&db, &scanner, 4).await.unwrap();
        assert_eq!(scanned_last_minute(&db).await, ["a.com", "b.com", "d.com", "e.com"]);
        tick(&db, &scanner, 4).await.unwrap();
        assert_eq!(scanned_last_minute(&db).await.len(), 4);

        // An hour later there's room again.
        sqlx::query("update tech_scans set checked_at = checked_at - interval '61 minutes', scanned_at = scanned_at - interval '61 minutes'")
            .execute(&db)
            .await
            .unwrap();
        tick(&db, &scanner, 4).await.unwrap();
        assert_eq!(scanned_last_minute(&db).await, ["c.com"]);
        let never: i64 = sqlx::query_scalar("select count(*) from tech_scans where domain in ('skip.com', 'demo.com')").fetch_one(&db).await.unwrap();
        assert_eq!(never, 0);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_scan_that_never_finished_is_due_again_the_next_day(db: PgPool) {
        let (scanner, fake) = serve().await;
        project(&db, &["stuck.com", "fresh.com"]).await;
        sqlx::query("insert into tech_scans (domain, checked_at) values ('stuck.com', now() - interval '25 hours'), ('fresh.com', now() - interval '2 hours')")
            .execute(&db)
            .await
            .unwrap();
        tick(&db, &scanner, 300).await.unwrap();
        assert_eq!((fake.hits("stuck.com"), fake.hits("fresh.com")), (1, 0));
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_writer_fact_names_only_the_projects_picks(db: PgPool) {
        let p = project(&db, &[]).await;
        sqlx::query(
            "insert into tech_scans (domain, techs, scanned_at) values
               ('acme.com', '{HubSpot,Intercom,Cloudflare,Google Analytics}', now()), ('plain.com', '{Cloudflare}', now())",
        )
        .execute(&db)
        .await
        .unwrap();
        sqlx::query("insert into project_tech_picks (project_id, tech, kind, position) values ($1, 'Intercom', 'competitor', 1), ($1, 'HubSpot', 'uses', 0), ($1, 'Stripe', 'uses', 2)")
            .bind(p)
            .execute(&db)
            .await
            .unwrap();
        let lines = fact_lines(&db, p, &["ACME.com".into(), "plain.com".into(), "unscanned.com".into()]).await.unwrap();
        assert_eq!(lines, HashMap::from([("acme.com".to_string(), "Uses HubSpot and Intercom".to_string())]));
    }
}
