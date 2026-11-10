//! Hiring signals: once a day, each project's company domains are checked for open roles that match the roles it watches,
//! and the matches are kept in job_openings. A company hiring for a role the product helps with is a warm lead, and the
//! post is the best opener for its email.

use std::time::Duration;

use futures::{StreamExt, stream};
use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::jobs::{self, Boards, Opening};

/// Domains each project gets checked a day, oldest-checked first. Each one costs a few requests the first time, while its
/// board is found, and one after that.
pub const DAILY_DOMAINS: i64 = 500;
/// Domains checked at the same time. Each is a different company's site or board, so this stays polite.
const PARALLEL: usize = 4;

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move {
        let boards = match Boards::new() {
            Ok(boards) => boards,
            Err(e) => return tracing::error!(error = ?e, "could not start the hiring signals worker"),
        };
        crate::mail::every(Duration::from_secs(3600), || tick(&db, &boards, DAILY_DOMAINS), "hiring signals").await
    });
}

/// Checks the due domains of every project that watches some roles. Demo projects are left out.
pub async fn tick(db: &PgPool, boards: &Boards, daily: i64) -> anyhow::Result<()> {
    let projects: Vec<Uuid> = sqlx::query_scalar("select id from projects where org_id is not null order by created_at").fetch_all(db).await?;
    for project_id in projects {
        let roles = roles(db, project_id).await?;
        if roles.is_empty() {
            continue;
        }
        let roles = &roles;
        let checks = stream::iter(due_domains(db, project_id, daily).await?)
            .map(|domain| async move { check(db, boards, project_id, &domain, roles).await })
            .buffer_unordered(PARALLEL);
        checks.collect::<Vec<_>>().await.into_iter().collect::<anyhow::Result<()>>()?;
    }
    Ok(())
}

/// The roles the project watches. Until it picks its own, those are the roles its campaigns target. An empty list turns it off.
pub async fn roles(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<String>> {
    let chosen: Option<Value> = sqlx::query_scalar("select targeting -> 'hiring_roles' from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await?
        .flatten();
    match chosen.and_then(|v| serde_json::from_value::<Vec<String>>(v).ok()) {
        Some(roles) => Ok(roles.into_iter().map(|r| r.trim().to_string()).filter(|r| !r.is_empty()).collect()),
        None => default_roles(db, project_id).await,
    }
}

/// The decision-maker roles of the project's campaigns in their order, each once. The Targeting page suggests these.
pub async fn default_roles(db: &PgPool, project_id: Uuid) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "select (array_agg(trim(r.role) order by s.position, r.n))[1]
         from segments s,
              jsonb_array_elements_text(case jsonb_typeof(s.targeting -> 'roles') when 'array' then s.targeting -> 'roles' else '[]' end)
                with ordinality r(role, n)
         where s.project_id = $1 and trim(r.role) <> ''
         group by lower(trim(r.role)) order by min(array[s.position::bigint, r.n])",
    )
    .bind(project_id)
    .fetch_all(db)
    .await
}

/// The project's company and lead domains that weren't checked in the last day, oldest-checked first, up to what's left of
/// today's `daily` checks.
async fn due_domains(db: &PgPool, project_id: Uuid, daily: i64) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "with domains as (
           select domain from companies where project_id = $1 and status <> 'excluded'
           union select company_domain from leads where project_id = $1 and company_domain is not null
         )
         select d.domain::text from domains d left join hiring_checks h on h.project_id = $1 and h.domain = d.domain
         where h.checked_at is null or h.checked_at < now() - interval '1 day'
         order by h.checked_at nulls first, d.domain
         limit greatest(0, $2 - (select count(*) from hiring_checks where project_id = $1 and checked_at >= now() - interval '1 day'))",
    )
    .bind(project_id)
    .bind(daily)
    .fetch_all(db)
    .await
}

/// Reads the domain's board and keeps its openings that match `roles`. Ones the board no longer lists are closed. A board
/// that can't be read is tried again tomorrow, with what's stored left alone.
async fn check(db: &PgPool, boards: &Boards, project_id: Uuid, domain: &str, roles: &[String]) -> anyhow::Result<()> {
    match jobs::openings(db, boards, domain).await {
        Ok(found) => {
            let (provider, openings) = found.map_or(("", vec![]), |(board, openings)| (board.provider.name(), openings));
            store(db, project_id, domain, provider, &openings, roles).await?;
        }
        Err(e) => tracing::info!(%domain, error = %e, "could not read a job board"),
    }
    sqlx::query(
        "insert into hiring_checks (project_id, domain) values ($1, $2) on conflict (project_id, domain) do update set checked_at = now()",
    )
    .bind(project_id)
    .bind(domain)
    .execute(db)
    .await?;
    Ok(())
}

async fn store(db: &PgPool, project_id: Uuid, domain: &str, provider: &str, openings: &[Opening], roles: &[String]) -> sqlx::Result<()> {
    let mut tx = db.begin().await?;
    for (o, role) in openings.iter().filter_map(|o| jobs::matching_roles(&o.title, roles).first().map(|r| (o, *r))) {
        sqlx::query(
            "insert into job_openings (project_id, domain, provider, title, role, location, department, url, excerpt, posted_at)
             values ($1, $2, $3, $4, $10, $5, $6, $7, $8, $9)
             on conflict (project_id, url) do update set title = excluded.title, role = excluded.role, location = excluded.location,
               department = excluded.department, excerpt = excluded.excerpt, posted_at = excluded.posted_at, last_seen = now(), closed_at = null",
        )
        .bind(project_id)
        .bind(domain)
        .bind(provider)
        .bind(&o.title)
        .bind(&o.location)
        .bind(&o.department)
        .bind(&o.url)
        .bind(&o.excerpt)
        .bind(o.posted_at)
        .bind(role)
        .execute(&mut *tx)
        .await?;
    }
    // Closed by what the board still lists, not by what matches: a post kept under roles the project has since dropped
    // stays open until it's taken down.
    let listed: Vec<&str> = openings.iter().map(|o| o.url.as_str()).collect();
    sqlx::query("update job_openings set closed_at = now() where project_id = $1 and domain = $2 and closed_at is null and url <> all($3)")
        .bind(project_id)
        .bind(domain)
        .bind(&listed)
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct Post {
    pub domain: String,
    pub title: String,
    pub url: String,
    pub excerpt: String,
}

/// The open roles kept for these domains, newest first, for the email writer and the lead's page.
pub async fn open_at(db: &PgPool, project_id: Uuid, domains: &[String]) -> sqlx::Result<Vec<Post>> {
    sqlx::query_as(
        "select domain::text, title, url, excerpt from job_openings
         where project_id = $1 and domain = any($2::citext[]) and closed_at is null order by first_seen desc",
    )
    .bind(project_id)
    .bind(domains)
    .fetch_all(db)
    .await
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::jobs::tests::{greenhouse, serve};

    async fn project(db: &PgPool, roles: Option<&[&str]>) -> Uuid {
        let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Test') returning id").fetch_one(db).await.unwrap();
        let project: Uuid = sqlx::query_scalar("insert into projects (org_id) values ($1) returning id").bind(org).fetch_one(db).await.unwrap();
        let targeting = roles.map_or(json!({}), |r| json!({"hiring_roles": r}));
        sqlx::query("insert into project_settings (project_id, targeting) values ($1, $2)").bind(project).bind(targeting).execute(db).await.unwrap();
        project
    }

    async fn checked(db: &PgPool, project: Uuid) -> Vec<String> {
        sqlx::query_scalar("select domain::text from hiring_checks where project_id = $1 and checked_at > now() - interval '1 minute' order by domain")
            .bind(project)
            .fetch_all(db)
            .await
            .unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn each_day_checks_the_oldest_domains_up_to_the_cap(db: PgPool) {
        let (boards, _) = serve().await;
        let p = project(&db, Some(&["SDR"])).await;
        for domain in ["a.com", "b.com", "c.com", "old.com", "recent.com"] {
            sqlx::query("insert into companies (project_id, domain) values ($1, $2)").bind(p).bind(domain).execute(&db).await.unwrap();
        }
        sqlx::query("insert into companies (project_id, domain, status) values ($1, 'skip.com', 'excluded')").bind(p).execute(&db).await.unwrap();
        for (domain, age) in [("old.com", "2 days"), ("recent.com", "1 hour")] {
            sqlx::query("insert into hiring_checks (project_id, domain, checked_at) values ($1, $2, now() - $3::interval)")
                .bind(p)
                .bind(domain)
                .bind(age)
                .execute(&db)
                .await
                .unwrap();
        }
        // A demo project has no organization and is never checked.
        let demo: Uuid = sqlx::query_scalar("insert into projects default values returning id").fetch_one(&db).await.unwrap();
        sqlx::query("insert into companies (project_id, domain) values ($1, 'demo.com')").bind(demo).execute(&db).await.unwrap();

        // recent.com already used one of today's three checks, so two never-checked domains go first.
        tick(&db, &boards, 3).await.unwrap();
        assert_eq!(checked(&db, p).await, ["a.com", "b.com"]);
        tick(&db, &boards, 3).await.unwrap();
        assert_eq!(checked(&db, p).await, ["a.com", "b.com"]);
        let demo_checks: i64 = sqlx::query_scalar("select count(*) from hiring_checks where project_id = $1").bind(demo).fetch_one(&db).await.unwrap();
        assert_eq!(demo_checks, 0);

        // A day later the cap has room again, and the rest are next, the longest-waiting first.
        sqlx::query("update hiring_checks set checked_at = checked_at - interval '1 day' where project_id = $1").bind(p).execute(&db).await.unwrap();
        tick(&db, &boards, 3).await.unwrap();
        assert_eq!(checked(&db, p).await, ["c.com", "old.com", "recent.com"]);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn matching_posts_are_kept_once_and_closed_when_gone(db: PgPool) {
        let (boards, fake) = serve().await;
        let p = project(&db, Some(&["SDR", "account executive"])).await;
        // A lead's company counts even without a company row.
        sqlx::query("insert into leads (project_id, full_name, email, company_domain) values ($1, 'Ana', 'ana@acme.com', 'acme.com')")
            .bind(p)
            .execute(&db)
            .await
            .unwrap();
        let sdr = ("SDR, EMEA", "https://job-boards.greenhouse.io/acme/jobs/1");
        let ae = ("Account Executive", "https://job-boards.greenhouse.io/acme/jobs/2");
        let engineer = ("Backend Engineer", "https://job-boards.greenhouse.io/acme/jobs/3");
        fake.board("greenhouse/acme", greenhouse(&[sdr, engineer]));
        let next_day = || sqlx::query("update hiring_checks set checked_at = now() - interval '25 hours' where project_id = $1").bind(p).execute(&db);
        let open = || {
            sqlx::query_scalar::<_, String>("select title from job_openings where project_id = $1 and closed_at is null order by title")
                .bind(p)
                .fetch_all(&db)
        };

        tick(&db, &boards, 500).await.unwrap();
        assert_eq!(open().await.unwrap(), ["SDR, EMEA"]);
        let (first_seen, provider): (chrono::DateTime<chrono::Utc>, String) =
            sqlx::query_as("select first_seen, provider from job_openings where url = $1").bind(sdr.1).fetch_one(&db).await.unwrap();
        assert_eq!(provider, "greenhouse");
        let role: String = sqlx::query_scalar("select role from job_openings where url = $1").bind(sdr.1).fetch_one(&db).await.unwrap();
        assert_eq!(role, "SDR");

        // The same post the next day is the same row, seen again.
        fake.board("greenhouse/acme", greenhouse(&[sdr, ae, engineer]));
        next_day().await.unwrap();
        tick(&db, &boards, 500).await.unwrap();
        assert_eq!(open().await.unwrap(), ["Account Executive", "SDR, EMEA"]);
        let (rows, same_first_seen, seen_again): (i64, bool, bool) = sqlx::query_as(
            "select count(*), bool_and(first_seen = $2 or url <> $3), bool_and(last_seen > first_seen or url <> $3) from job_openings where project_id = $1",
        )
        .bind(p)
        .bind(first_seen)
        .bind(sdr.1)
        .fetch_one(&db)
        .await
        .unwrap();
        assert_eq!((rows, same_first_seen, seen_again), (2, true, true));

        // Taken down, then put back up.
        fake.board("greenhouse/acme", greenhouse(&[ae]));
        next_day().await.unwrap();
        tick(&db, &boards, 500).await.unwrap();
        assert_eq!(open().await.unwrap(), ["Account Executive"]);
        fake.board("greenhouse/acme", greenhouse(&[sdr, ae]));
        next_day().await.unwrap();
        tick(&db, &boards, 500).await.unwrap();
        assert_eq!(open().await.unwrap(), ["Account Executive", "SDR, EMEA"]);

        // The whole board is gone, and no other one is found.
        fake.remove_board("greenhouse/acme");
        next_day().await.unwrap();
        tick(&db, &boards, 500).await.unwrap();
        assert!(open().await.unwrap().is_empty());
        let rows: i64 = sqlx::query_scalar("select count(*) from job_openings").fetch_one(&db).await.unwrap();
        assert_eq!(rows, 2);
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn roles_default_to_the_campaigns_and_an_empty_list_turns_it_off(db: PgPool) {
        let p = project(&db, None).await;
        for (position, roles) in [(0, json!(["Head of Sales", " VP Sales "])), (1, json!(["head of sales", "SDR", ""])), (2, json!("Owner"))] {
            sqlx::query("insert into segments (project_id, name, position, targeting) values ($1, 'C', $2, $3)")
                .bind(p)
                .bind(position)
                .bind(json!({"roles": roles}))
                .execute(&db)
                .await
                .unwrap();
        }
        assert_eq!(roles(&db, p).await.unwrap(), ["Head of Sales", "VP Sales", "SDR"]);
        sqlx::query("update project_settings set targeting = '{\"hiring_roles\": []}' where project_id = $1").bind(p).execute(&db).await.unwrap();
        assert!(roles(&db, p).await.unwrap().is_empty());
        assert_eq!(default_roles(&db, p).await.unwrap().len(), 3);
    }
}
