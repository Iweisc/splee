//! The daily data export to the organization's own S3-compatible bucket, for loading into a warehouse like Snowflake, BigQuery,
//! Redshift or DuckDB. An admin sets it up on the Data export page (routes/exports.rs). Once a day after QUIET_HOUR UTC, and on
//! Export now, every chosen table of every project in the organization is written as a full snapshot:
//!
//!   {prefix}/{table}/dt=YYYY-MM-DD/part-0000.jsonl.gz    gzipped newline-delimited JSON, see tables.rs
//!   {prefix}/_manifest/dt=YYYY-MM-DD.json                written last, so a day with a manifest is complete
//!
//! All tables are read in one read-only repeatable-read transaction, so they agree with each other, and rows stream from
//! Postgres into the gzip encoder while the bytes go up a part at a time (s3::FileWriter).

pub mod s3;
pub mod tables;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{NaiveDate, Timelike, Utc};
use futures::TryStreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::crypto;
use crate::hooks::{self, ChatAlert};
use crate::models::Ts;
use crate::state::AppState;
use s3::{Client, FileWriter, Target};

/// The UTC hour from which the daily export runs, when few people anywhere are using Splee.
pub const QUIET_HOUR: u32 = 3;
/// How many times a day the daily export is tried, an hour apart, before it waits for the next day.
const DAILY_TRIES: i64 = 3;
const RESTARTED: &str = "Splee restarted during this export. It runs again on its own.";
pub const PAGE: &str = "/app/data-export";

/// The settings as the database holds them, the secret still encrypted.
#[derive(Debug, Clone, FromRow)]
pub struct Settings {
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub path_style: bool,
    pub access_key_id: String,
    pub secret_key_enc: String,
    pub tables: Vec<String>,
    pub include_bodies: bool,
}

impl Settings {
    pub async fn load(db: &PgPool, org_id: Uuid) -> sqlx::Result<Option<Settings>> {
        sqlx::query_as(
            "select endpoint, region, bucket, prefix, path_style, access_key_id, secret_key_enc, tables, include_bodies
             from export_settings where org_id = $1",
        )
        .bind(org_id)
        .fetch_optional(db)
        .await
    }

    pub fn target(&self, app_secret: &str) -> Result<Target, String> {
        let secret = crypto::decrypt(app_secret, &self.secret_key_enc).map_err(|e| {
            tracing::error!(error = %e, "data export: the secret access key couldn't be decrypted");
            "Splee can't read the saved secret access key anymore. Paste it again.".to_string()
        })?;
        Ok(Target {
            endpoint: self.endpoint.clone(),
            region: self.region.clone(),
            bucket: self.bucket.clone(),
            path_style: self.path_style,
            access_key_id: self.access_key_id.clone(),
            secret,
        })
    }
}

/// `{prefix}/{rest}`, or just `rest` without a prefix.
pub fn key(prefix: &str, rest: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() { rest.to_string() } else { format!("{prefix}/{rest}") }
}

pub fn file_key(prefix: &str, table: &str, dt: NaiveDate) -> String {
    key(prefix, &format!("{table}/dt={dt}/part-0000.jsonl.gz"))
}

pub fn manifest_key(prefix: &str, dt: NaiveDate) -> String {
    key(prefix, &format!("_manifest/dt={dt}.json"))
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Run {
    pub id: Uuid,
    pub dt: NaiveDate,
    /// daily or manual.
    pub trigger: String,
    /// running, done or failed.
    pub status: String,
    /// [{"table", "key", "rows", "bytes"}].
    pub files: Value,
    pub rows: i64,
    pub bytes: i64,
    pub error: Option<String>,
    pub started_at: Ts,
    pub finished_at: Option<Ts>,
}

pub const RUN_COLUMNS: &str = "id, dt, trigger, status, files, rows, bytes, error, started_at, finished_at";

pub async fn recent(db: &PgPool, org_id: Uuid, limit: i64) -> sqlx::Result<Vec<Run>> {
    sqlx::query_as(&format!("select {RUN_COLUMNS} from export_runs where org_id = $1 order by started_at desc limit $2"))
        .bind(org_id)
        .bind(limit)
        .fetch_all(db)
        .await
}

#[derive(Debug, PartialEq)]
pub enum StartError {
    NotSetUp,
    Running,
    /// The daily export isn't due: before QUIET_HOUR, the day is done, or it failed less than an hour ago or DAILY_TRIES times.
    NotDue,
    Db,
}

/// A run and the organization's export lock, which `finish` holds until the run is saved and any alert is sent.
pub struct Started {
    pub run: Run,
    lock: sqlx::Transaction<'static, sqlx::Postgres>,
}

/// Notes a new run for the UTC day of `now`, unless one is running already or, for a daily run, the day isn't due. The caller
/// runs it with `finish`.
pub async fn start(db: &PgPool, org_id: Uuid, trigger: &str, now: Ts) -> Result<Started, StartError> {
    let failed = |e: sqlx::Error| {
        tracing::error!(error = %e, "data export: couldn't start the run");
        StartError::Db
    };
    // Held until the run is saved, so a second backend on the database neither starts the same export nor closes this one.
    let mut lock = db.begin().await.map_err(failed)?;
    let owned: bool = sqlx::query_scalar("select pg_try_advisory_xact_lock(hashtextextended('data-export:' || $1::text, 0))")
        .bind(org_id)
        .fetch_one(&mut *lock)
        .await
        .map_err(failed)?;
    if !owned {
        return Err(StartError::Running);
    }
    let set_up: bool =
        sqlx::query_scalar("select exists (select 1 from export_settings where org_id = $1)").bind(org_id).fetch_one(db).await.map_err(failed)?;
    if !set_up {
        return Err(StartError::NotSetUp);
    }
    // Nobody holds the lock, so a run still marked running was cut off when its backend stopped.
    sqlx::query("update export_runs set status = 'failed', error = $2, finished_at = now() where org_id = $1 and status = 'running'")
        .bind(org_id)
        .bind(RESTARTED)
        .execute(db)
        .await
        .map_err(failed)?;
    if trigger == "daily" && !due(db, org_id, now).await.map_err(failed)? {
        return Err(StartError::NotDue);
    }
    let run = sqlx::query_as(&format!("insert into export_runs (org_id, dt, trigger, started_at) values ($1, $2, $3, $4) returning {RUN_COLUMNS}"))
        .bind(org_id)
        .bind(now.date_naive())
        .bind(trigger)
        .bind(now)
        .fetch_one(db)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.constraint() == Some("export_runs_running_idx") => StartError::Running,
            e => failed(e),
        })?;
    Ok(Started { run, lock })
}

/// Whether the daily export should run now: past QUIET_HOUR, with no finished export of the day, daily or Export now, and one
/// that failed is tried again an hour later, DAILY_TRIES times a day.
async fn due(db: &PgPool, org_id: Uuid, now: Ts) -> sqlx::Result<bool> {
    if now.hour() < QUIET_HOUR {
        return Ok(false);
    }
    sqlx::query_scalar(
        "select not exists (select 1 from export_runs r where r.org_id = $1 and r.dt = $2
                              and (r.status <> 'failed' or (r.trigger = 'daily' and r.started_at > $3 - interval '1 hour')))
           and (select count(*) from export_runs r where r.org_id = $1 and r.dt = $2 and r.trigger = 'daily') < $4",
    )
    .bind(org_id)
    .bind(now.date_naive())
    .bind(now)
    .bind(DAILY_TRIES)
    .fetch_one(db)
    .await
}

#[derive(Debug, Serialize)]
pub struct File {
    pub table: String,
    pub key: String,
    pub rows: i64,
    pub bytes: i64,
}

/// Writes the files and the manifest, then keeps how it went on the run, and alerts the team chat when this is the second
/// failure in a row.
pub async fn finish(state: &AppState, org_id: Uuid, started: Started) {
    let Started { run, lock } = started;
    let result = export(state, org_id, run.dt).await;
    let (status, files, error) = match &result {
        Ok(files) => ("done", files.as_slice(), None),
        Err(e) => ("failed", &[][..], Some(e.as_str())),
    };
    let saved = sqlx::query(
        "update export_runs set status = $2, files = $3, rows = $4, bytes = $5, error = $6, finished_at = now() where id = $1",
    )
    .bind(run.id)
    .bind(status)
    .bind(json!(files))
    .bind(files.iter().map(|f| f.rows).sum::<i64>())
    .bind(files.iter().map(|f| f.bytes).sum::<i64>())
    .bind(error)
    .execute(&state.db)
    .await;
    if let Err(e) = saved {
        return tracing::error!(error = %e, "data export: couldn't save how the run went");
    }
    // Still under the lock, so no other run lands between this one and the check for a second failure.
    if let Err(e) = &result {
        tracing::info!(org = %org_id, error = e, "data export failed");
        if let Err(e) = alert_second_failure(state, org_id, e).await {
            tracing::warn!(error = %e, "data export: couldn't check for a second failure");
        }
    }
    if let Err(e) = lock.commit().await {
        tracing::warn!(error = %e, "data export: couldn't release the lock");
    }
}

/// Something on Splee's side, which the person reading the run can't fix.
fn ours(e: impl std::fmt::Display) -> String {
    tracing::error!(error = %e, "data export: database error");
    "Something went wrong on Splee's side while reading your data. It tries again on its own.".into()
}

async fn export(state: &AppState, org_id: Uuid, dt: NaiveDate) -> Result<Vec<File>, String> {
    let settings = Settings::load(&state.db, org_id).await.map_err(ours)?.ok_or("The data export was turned off.")?;
    let client = Client::new(&state.cfg, &settings.target(&state.cfg.app_secret)?)?;
    let mut tx = state.db.begin().await.map_err(ours)?;
    sqlx::query("set transaction isolation level repeatable read, read only").execute(&mut *tx).await.map_err(ours)?;
    // Timestamps come out as 2026-11-13T03:00:00.123456+00:00 whatever the server's time zone.
    sqlx::query("set local timezone = 'UTC'").execute(&mut *tx).await.map_err(ours)?;
    // A day exported before stops counting as complete until every file is written again.
    client.delete(&manifest_key(&settings.prefix, dt)).await.map_err(|e| format!("Manifest: {e}"))?;
    let mut files = Vec::new();
    for table in tables::TABLES.iter().filter(|t| settings.tables.iter().any(|n| n == t.name)) {
        let mut writer = FileWriter::new(&client, file_key(&settings.prefix, table.name, dt));
        let mut query = sqlx::query_scalar::<_, String>(table.sql).bind(org_id);
        if table.bodies {
            query = query.bind(settings.include_bodies);
        }
        let mut rows = query.fetch(&mut *tx);
        let streamed: Result<(), String> = async {
            while let Some(line) = rows.try_next().await.map_err(ours)? {
                writer.line(&line).await?;
            }
            Ok(())
        }
        .await;
        drop(rows);
        if let Err(e) = streamed {
            writer.abort().await;
            return Err(format!("{}: {e}", table.label));
        }
        let (key, rows, bytes) = writer.finish().await.map_err(|e| format!("{}: {e}", table.label))?;
        files.push(File { table: table.name.into(), key, rows, bytes });
    }
    tx.rollback().await.map_err(ours)?;
    let manifest = json!({
        "schema_version": tables::SCHEMA_VERSION,
        "dt": dt,
        "exported_at": Utc::now(),
        "organization_id": org_id,
        "files": files,
    });
    let manifest = serde_json::to_vec_pretty(&manifest).expect("a JSON value serializes");
    client.put(&manifest_key(&settings.prefix, dt), "application/json", manifest).await.map_err(|e| format!("Manifest: {e}"))?;
    Ok(files)
}

/// Posts to the team chat when the last two runs failed and the one before didn't, so a bucket that stays broken alerts once.
async fn alert_second_failure(state: &AppState, org_id: Uuid, error: &str) -> sqlx::Result<()> {
    let last: Vec<String> = sqlx::query_scalar("select status from export_runs where org_id = $1 and status <> 'running' order by started_at desc limit 3")
        .bind(org_id)
        .fetch_all(&state.db)
        .await?;
    if !(last.len() >= 2 && last[..2].iter().all(|s| s == "failed") && last.get(2).is_none_or(|s| s != "failed")) {
        return Ok(());
    }
    let bucket: String = sqlx::query_scalar("select bucket from export_settings where org_id = $1").bind(org_id).fetch_optional(&state.db).await?.unwrap_or_default();
    let summary = format!("The export to {bucket} failed twice in a row, so your warehouse is missing today's data.");
    let link = format!("{}{PAGE}", state.cfg.public_url);
    let alert = ChatAlert { label: "Data export failed", summary: &summary, quote: Some(error), link: &link, link_text: "Open the data export" };
    hooks::post_org_chat(&state.db, org_id, &alert).await;
    Ok(())
}

/// Starts the daily export of each organization that's due, see `due`. Organizations another backend is exporting are left
/// to it.
pub async fn tick(state: &AppState, now: Ts) -> sqlx::Result<()> {
    let orgs: Vec<Uuid> = sqlx::query_scalar("select org_id from export_settings order by org_id").fetch_all(&state.db).await?;
    for org_id in orgs {
        match start(&state.db, org_id, "daily", now).await {
            Ok(started) => finish(state, org_id, started).await,
            Err(StartError::Db) => tracing::warn!(org = %org_id, "data export: couldn't start the daily run"),
            Err(_) => {}
        }
    }
    Ok(())
}

pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = tick(&state, Utc::now()).await {
                tracing::error!(error = %e, "data export: tick failed");
            }
            tokio::time::sleep(Duration::from_secs(5 * 60)).await;
        }
    });
}

/// Writes a small file under the prefix and deletes it, to check the settings before saving them.
pub async fn test_connection(state: &AppState, target: &Target, prefix: &str) -> Result<(), String> {
    let client = Client::new(&state.cfg, target)?;
    let marker = key(prefix, &format!("_splee/connection-test-{}.txt", Uuid::new_v4()));
    client.put(&marker, "text/plain", b"Splee checks that it can write here. It deletes this file right away.\n".to_vec()).await?;
    client
        .delete(&marker)
        .await
        .map_err(|e| format!("Splee could write to the bucket but couldn't delete its test file {marker}: {e} Delete the file by hand."))
}
