//! The queue behind "Load a country" on the Companies page. Instance operators queue countries, and a worker loads them
//! from Overture one at a time across the instance, writing its progress into the row as it goes.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::watch;
use uuid::Uuid;

use crate::directory::{self, Progress};
use crate::error::{AppError, AppResult};

/// How often a running import writes its progress. A running row whose heartbeat is older than STALE belonged to a
/// process that died, so it's marked failed and the next country can start.
const HEARTBEAT: Duration = Duration::from_secs(10);
const STALE: &str = "1 minute";

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct DirectoryImport {
    pub id: Uuid,
    pub country: String,
    pub status: String,
    pub groups_done: i32,
    pub groups_total: i32,
    pub rows_seen: i64,
    pub rows_kept: i64,
    pub error: String,
    /// The email of the operator who asked for it.
    pub requested_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

const SELECT: &str = "select i.id, i.country, i.status, i.groups_done, i.groups_total, i.rows_seen, i.rows_kept, i.error,
        u.email::text as requested_by, i.created_at, i.started_at, i.finished_at
    from directory_imports i left join users u on u.id = i.requested_by";

/// The latest 50, newest first.
pub async fn list(db: &PgPool) -> sqlx::Result<Vec<DirectoryImport>> {
    sqlx::query_as(&format!("{SELECT} order by i.created_at desc limit 50")).fetch_all(db).await
}

/// Queues `country`, a two-letter code, unless it's already queued or loading.
pub async fn queue(db: &PgPool, country: &str, user_id: Uuid) -> AppResult<DirectoryImport> {
    let country = country.trim().to_uppercase();
    if country.len() != 2 || !country.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(AppError::BadRequest("Pick a country by its two-letter code, like GB".into()));
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into directory_imports (country, requested_by) values ($1, $2) on conflict (country) where status in ('queued', 'running') do nothing
         returning id",
    )
    .bind(&country)
    .bind(user_id)
    .fetch_optional(db)
    .await?;
    let Some(id) = id else { return Err(AppError::Conflict(format!("{country} is already waiting or loading"))) };
    Ok(sqlx::query_as(&format!("{SELECT} where i.id = $1")).bind(id).fetch_one(db).await?)
}

pub fn spawn_worker(db: PgPool) {
    tokio::spawn(async move { crate::mail::every(Duration::from_secs(5), || tick(&db, directory::CATALOG), "company import").await });
}

/// Fails imports whose process died, then loads the oldest queued country unless one is loading. Returns once it's done.
pub async fn tick(db: &PgPool, catalog: &str) -> anyhow::Result<()> {
    recover(db).await?;
    let Some((id, country)) = claim(db).await? else { return Ok(()) };
    run(db, catalog, id, &country).await
}

async fn recover(db: &PgPool) -> sqlx::Result<()> {
    sqlx::query(&format!(
        "update directory_imports set status = 'failed', error = 'Splee stopped while loading it. Load it again.', finished_at = now()
         where status = 'running' and heartbeat_at < now() - interval '{STALE}'"
    ))
    .execute(db)
    .await?;
    Ok(())
}

async fn claim(db: &PgPool) -> sqlx::Result<Option<(Uuid, String)>> {
    let claimed = sqlx::query_as(
        "update directory_imports set status = 'running', started_at = now(), heartbeat_at = now()
         where id = (select id from directory_imports where status = 'queued'
                       and not exists (select 1 from directory_imports where status = 'running')
                     order by created_at limit 1 for update skip locked)
         returning id, country",
    )
    .fetch_optional(db)
    .await;
    match claimed {
        // directory_imports_one_running: another backend on this database started one at the same moment.
        Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23505") => Ok(None),
        other => other,
    }
}

async fn run(db: &PgPool, catalog: &str, id: Uuid, country: &str) -> anyhow::Result<()> {
    let (tx, rx) = watch::channel(Progress::default());
    let report = move |p: Progress| {
        tx.send_replace(p);
    };
    // Writes on a timer rather than per row group, so a big country doesn't write thousands of times, and the
    // heartbeat keeps going while a slow row group downloads.
    let heartbeat = async {
        let mut every = tokio::time::interval(HEARTBEAT);
        loop {
            every.tick().await;
            let progress = *rx.borrow();
            if let Err(e) = save(db, id, progress, None).await {
                tracing::error!(error = ?e, "could not save a company import's progress");
            }
        }
    };
    let countries = [country.to_string()];
    let result = tokio::select! {
        result = directory::import(db, catalog, &countries, Some(&report)) => result,
        _ = heartbeat => unreachable!("the heartbeat never stops"),
    };
    match result {
        Ok(progress) => save(db, id, progress, Some(Ok(()))).await?,
        Err(e) => {
            tracing::error!(error = ?e, country, "company import failed");
            let progress = *rx.borrow();
            save(db, id, progress, Some(Err(format!("{e:#}")))).await?;
        }
    }
    directory::forget_countries();
    Ok(())
}

/// Writes the progress, and with `outcome`, finishes the import.
async fn save(db: &PgPool, id: Uuid, p: Progress, outcome: Option<Result<(), String>>) -> sqlx::Result<()> {
    let (status, error) = match outcome {
        None => (None, None),
        Some(Ok(())) => (Some("done"), None),
        Some(Err(e)) => (Some("failed"), Some(e)),
    };
    sqlx::query(
        "update directory_imports set groups_done = $2, groups_total = $3, rows_seen = $4, rows_kept = $5, heartbeat_at = now(),
                status = coalesce($6, status), error = coalesce($7, error), finished_at = case when $6 is null then null else now() end
         where id = $1 and status = 'running'",
    )
    .bind(id)
    .bind(p.groups_done as i32)
    .bind(p.groups_total as i32)
    .bind(p.rows_seen as i64)
    .bind(p.rows_kept as i64)
    .bind(status)
    .bind(error)
    .execute(db)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use sqlx::PgPool;
    use uuid::Uuid;

    use super::*;
    use crate::directory::fake;

    async fn user(db: &PgPool) -> Uuid {
        sqlx::query_scalar("insert into users (email, name, password_hash) values ('ops@example.com', 'Ops', 'x') returning id")
            .fetch_one(db)
            .await
            .unwrap()
    }

    async fn status(db: &PgPool, id: Uuid) -> (String, String) {
        sqlx::query_as("select status, error from directory_imports where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    // Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_country_is_queued_once_at_a_time(db: PgPool) {
        let user = user(&db).await;
        assert!(matches!(queue(&db, "gbr", user).await, Err(AppError::BadRequest(_))));
        assert!(matches!(queue(&db, "1x", user).await, Err(AppError::BadRequest(_))));

        let gb = queue(&db, " gb ", user).await.unwrap();
        assert_eq!((gb.country.as_str(), gb.status.as_str(), gb.requested_by.as_deref()), ("GB", "queued", Some("ops@example.com")));
        assert!(matches!(queue(&db, "GB", user).await, Err(AppError::Conflict(_))));
        queue(&db, "FR", user).await.unwrap();

        sqlx::query("update directory_imports set status = 'running' where id = $1").bind(gb.id).execute(&db).await.unwrap();
        assert!(matches!(queue(&db, "GB", user).await, Err(AppError::Conflict(_))));
        sqlx::query("update directory_imports set status = 'done' where id = $1").bind(gb.id).execute(&db).await.unwrap();
        let again = queue(&db, "GB", user).await.unwrap();

        let listed: Vec<String> = list(&db).await.unwrap().iter().map(|i| format!("{} {}", i.country, i.status)).collect();
        assert_eq!(listed, ["GB queued", "FR queued", "GB done"]);
        assert_ne!(again.id, gb.id);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_restart_fails_the_import_a_dead_process_left_running_and_starts_the_next(db: PgPool) {
        let user = user(&db).await;
        let base = fake::start().await;
        let catalog = format!("{base}/catalog.json");
        let running = |id: Uuid, ago: &'static str| {
            let db = db.clone();
            async move {
                sqlx::query("update directory_imports set status = 'running', heartbeat_at = now() - $2::interval where id = $1")
                    .bind(id)
                    .bind(ago)
                    .execute(&db)
                    .await
                    .unwrap();
            }
        };
        let dead = queue(&db, "GB", user).await.unwrap().id;
        running(dead, "5 minutes").await;
        let lu = queue(&db, "LU", user).await.unwrap().id;
        tick(&db, &catalog).await.unwrap();
        assert_eq!(status(&db, dead).await, ("failed".into(), "Splee stopped while loading it. Load it again.".into()));
        assert_eq!(status(&db, lu).await, ("done".into(), String::new()));

        // A worker that still beats, maybe another backend's, holds the queue.
        let alive = queue(&db, "FR", user).await.unwrap().id;
        running(alive, "5 seconds").await;
        let mt = queue(&db, "MT", user).await.unwrap().id;
        tick(&db, &catalog).await.unwrap();
        assert_eq!(status(&db, alive).await.0, "running");
        assert_eq!(status(&db, mt).await.0, "queued");
        assert!(claim(&db).await.unwrap().is_none());
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_worker_loads_a_country_and_records_its_progress(db: PgPool) {
        let user = user(&db).await;
        let base = fake::start().await;
        sqlx::query(
            "insert into directory_companies (domain, name, website, country_code, source_id, release)
             values ('gone.lu', 'Gone', 'https://gone.lu', 'LU', 'x', '2026-09-17.0'), ('kept.mt', 'Kept', 'https://kept.mt', 'MT', 'x', '2026-09-17.0')",
        )
        .execute(&db)
        .await
        .unwrap();
        let lu = queue(&db, "lu", user).await.unwrap().id;
        tick(&db, &format!("{base}/catalog.json")).await.unwrap();

        let done = list(&db).await.unwrap().into_iter().find(|i| i.id == lu).unwrap();
        assert_eq!((done.status.as_str(), done.groups_done, done.groups_total, done.rows_seen, done.rows_kept), ("done", 1, 1, 4, 2));
        assert!(done.started_at.is_some() && done.finished_at.is_some());
        let domains: Vec<String> = sqlx::query_scalar("select domain from directory_companies order by domain").fetch_all(&db).await.unwrap();
        assert_eq!(domains, ["kayl.lu", "kept.mt", "weber.lu"]);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_failed_import_says_why(db: PgPool) {
        let user = user(&db).await;
        let base = fake::start().await;
        let id = queue(&db, "LU", user).await.unwrap().id;
        tick(&db, &format!("{base}/missing.json")).await.unwrap();
        let (status, error) = status(&db, id).await;
        assert_eq!(status, "failed");
        assert!(error.contains("404"), "{error}");
    }
}
