//! The Data export page (exports/): the organization's bucket, a button to test it, Export now, and the last runs. Only admins
//! see or change any of it, since it sends every project's data out of Splee. The secret access key is write-only: it's
//! encrypted on save and the page only learns that one is saved.

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::FromRow;

use crate::audit;
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::exports::s3::{self, Target};
use crate::exports::{self, Run, Settings, StartError, tables};
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/organization/exports", get(status).put(save).delete(remove))
        .route("/organization/exports/test", post(test))
        .route("/organization/exports/run", post(run_now))
}

/// The settings as the page shows them, without the secret.
#[derive(Debug, Serialize, FromRow)]
struct SettingsView {
    endpoint: String,
    region: String,
    bucket: String,
    prefix: String,
    path_style: bool,
    access_key_id: String,
    tables: Vec<String>,
    include_bodies: bool,
    updated_at: Ts,
}

#[derive(Serialize)]
struct TableView {
    name: &'static str,
    label: &'static str,
}

#[derive(Serialize)]
struct Status {
    settings: Option<SettingsView>,
    /// The last 7, newest first.
    runs: Vec<Run>,
    tables: Vec<TableView>,
    quiet_hour_utc: u32,
    schema_version: i32,
}

async fn load(state: &AppState, auth: &Auth) -> AppResult<Json<Status>> {
    let settings = sqlx::query_as(
        "select endpoint, region, bucket, prefix, path_style, access_key_id, tables, include_bodies, updated_at from export_settings where org_id = $1",
    )
    .bind(auth.org_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(Status {
        settings,
        runs: exports::recent(&state.db, auth.org_id, 7).await?,
        tables: tables::TABLES.iter().map(|t| TableView { name: t.name, label: t.label }).collect(),
        quiet_hour_utc: exports::QUIET_HOUR,
        schema_version: tables::SCHEMA_VERSION,
    }))
}

async fn status(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    load(&state, &auth).await
}

#[derive(Deserialize)]
struct Body {
    #[serde(default)]
    endpoint: String,
    #[serde(default)]
    region: String,
    bucket: String,
    #[serde(default)]
    prefix: String,
    #[serde(default)]
    path_style: bool,
    access_key_id: String,
    /// Empty keeps the saved one.
    #[serde(default)]
    secret_access_key: String,
    #[serde(default)]
    tables: Vec<String>,
    #[serde(default)]
    include_bodies: bool,
}

/// The form checked and tidied, with the saved secret when none was typed.
struct Checked {
    target: Target,
    prefix: String,
    /// Typed just now, so it's encrypted and saved.
    new_secret: bool,
}

async fn check(state: &AppState, auth: &Auth, body: &Body) -> AppResult<Checked> {
    let bucket = body.bucket.trim();
    if bucket.is_empty() || bucket.contains('/') {
        return Err(AppError::BadRequest("Enter the bucket's name, without any folders.".into()));
    }
    let access_key_id = body.access_key_id.trim();
    if access_key_id.is_empty() {
        return Err(AppError::BadRequest("Enter the access key ID.".into()));
    }
    let region = match body.region.trim() {
        "" => "us-east-1",
        r => r,
    };
    if !region.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
        return Err(AppError::BadRequest(format!("{region} isn't a region name, like us-east-1 or auto.")));
    }
    s3::endpoint_url(&body.endpoint, region).map_err(AppError::BadRequest)?;
    let prefix = body.prefix.trim().trim_matches('/');
    if !prefix.is_empty() && prefix.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err(AppError::BadRequest("The folder can't have empty parts, . or ..".into()));
    }
    let typed = body.secret_access_key.trim();
    let secret = if typed.is_empty() {
        let saved = Settings::load(&state.db, auth.org_id).await?.ok_or_else(|| AppError::BadRequest("Enter the secret access key.".into()))?;
        saved.target(&state.cfg.app_secret).map_err(AppError::BadRequest)?.secret
    } else {
        typed.to_string()
    };
    let target = Target {
        endpoint: body.endpoint.trim().trim_end_matches('/').to_string(),
        region: region.to_string(),
        bucket: bucket.to_string(),
        path_style: body.path_style,
        access_key_id: access_key_id.to_string(),
        secret,
    };
    Ok(Checked { target, prefix: prefix.to_string(), new_secret: !typed.is_empty() })
}

async fn save(State(state): State<AppState>, auth: Auth, Json(body): Json<Body>) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    let Checked { target, prefix, new_secret } = check(&state, &auth, &body).await?;
    if let Some(bad) = body.tables.iter().find(|n| tables::find(n).is_none()) {
        return Err(AppError::BadRequest(format!("{bad} isn't a table Splee exports.")));
    }
    // In the order of TABLES, once each.
    let picked: Vec<String> = tables::TABLES.iter().filter(|t| body.tables.iter().any(|n| n == t.name)).map(|t| t.name.to_string()).collect();
    if picked.is_empty() {
        return Err(AppError::BadRequest("Pick at least one table to export.".into()));
    }
    let secret_enc = crypto::encrypt(&state.cfg.app_secret, &target.secret);
    sqlx::query(
        "insert into export_settings (org_id, endpoint, region, bucket, prefix, path_style, access_key_id, secret_key_enc, tables, include_bodies)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (org_id) do update set endpoint = excluded.endpoint, region = excluded.region, bucket = excluded.bucket, prefix = excluded.prefix,
           path_style = excluded.path_style, access_key_id = excluded.access_key_id,
           secret_key_enc = case when $11 then excluded.secret_key_enc else export_settings.secret_key_enc end,
           tables = excluded.tables, include_bodies = excluded.include_bodies, updated_at = now()",
    )
    .bind(auth.org_id)
    .bind(&target.endpoint)
    .bind(&target.region)
    .bind(&target.bucket)
    .bind(&prefix)
    .bind(target.path_style)
    .bind(&target.access_key_id)
    .bind(secret_enc)
    .bind(&picked)
    .bind(body.include_bodies)
    .bind(new_secret)
    .execute(&state.db)
    .await?;
    let details = json!({
        "endpoint": target.endpoint, "region": target.region, "bucket": target.bucket, "prefix": prefix, "tables": picked,
        "include_bodies": body.include_bodies, "new_secret_key": new_secret,
    });
    audit::record(&state.db, &auth, "export.saved", audit::Target::new("export", None, &target.bucket), details).await;
    load(&state, &auth).await
}

/// Stops the daily export. The runs stay listed.
async fn remove(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Status>> {
    auth.require_admin()?;
    let bucket: Option<String> =
        sqlx::query_scalar("delete from export_settings where org_id = $1 returning bucket").bind(auth.org_id).fetch_optional(&state.db).await?;
    if let Some(bucket) = bucket {
        audit::record(&state.db, &auth, "export.removed", audit::Target::new("export", None, &bucket), json!({})).await;
    }
    load(&state, &auth).await
}

#[derive(Serialize)]
struct Tested {
    ok: bool,
    /// What went wrong, in plain words.
    error: Option<String>,
}

/// Tries the form as typed, before it's saved.
async fn test(State(state): State<AppState>, auth: Auth, Json(body): Json<Body>) -> AppResult<Json<Tested>> {
    auth.require_admin()?;
    let Checked { target, prefix, .. } = check(&state, &auth, &body).await?;
    let result = exports::test_connection(&state, &target, &prefix).await;
    Ok(Json(Tested { ok: result.is_ok(), error: result.err() }))
}

/// Export now: starts a run and answers straight away, while it writes the files.
async fn run_now(State(state): State<AppState>, auth: Auth) -> AppResult<(StatusCode, Json<Run>)> {
    auth.require_admin()?;
    let started = exports::start(&state.db, auth.org_id, "manual", Utc::now()).await.map_err(|e| match e {
        StartError::NotSetUp => AppError::BadRequest("Save the bucket settings first.".into()),
        StartError::Running => AppError::Conflict("An export is running now. Wait for it to finish.".into()),
        StartError::NotDue | StartError::Db => AppError::Internal(anyhow::anyhow!("couldn't start the export")),
    })?;
    let (org_id, run) = (auth.org_id, started.run.clone());
    tokio::spawn(async move { exports::finish(&state, org_id, started).await });
    Ok((StatusCode::ACCEPTED, Json(run)))
}
