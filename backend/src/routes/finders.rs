//! The project's email finders, in the order the waterfall asks them (waterfall.rs), and "Find phone numbers too".

use axum::extract::{Path, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::settings::load_or_create;
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;
use crate::verify::Finder;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/finders", get(list))
        // Static, so they win over {provider} below.
        .route("/projects/{id}/finders/order", put(reorder))
        .route("/projects/{id}/finders/phones", put(set_phones))
        .route("/projects/{id}/finders/{provider}", put(connect).delete(disconnect))
}

/// A connected finder as the integrations page shows it. The key never leaves the server.
#[derive(Debug, Serialize, FromRow)]
pub struct ConnectedFinder {
    pub provider: String,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    /// Out of credits or past its rate limit, so the waterfall skips it until then.
    pub paused_until: Option<Ts>,
}

#[derive(Debug, Serialize)]
pub struct Finders {
    /// In the order they're asked.
    pub finders: Vec<ConnectedFinder>,
    pub find_phones: bool,
}

async fn finders(db: &PgPool, project_id: Uuid) -> AppResult<Json<Finders>> {
    let finders = sqlx::query_as(
        "select provider, error, failed_at, case when paused_until > now() then paused_until end as paused_until
         from project_finders where project_id = $1 order by position, created_at",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let find_phones = sqlx::query_scalar("select find_phones from project_settings where project_id = $1").bind(project_id).fetch_one(db).await?;
    Ok(Json(Finders { finders, find_phones }))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Finders>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    finders(&state.db, id).await
}

#[derive(Deserialize)]
struct KeyBody {
    key: String,
    /// Snov.io's API secret, which goes with its user ID in `key`.
    #[serde(default)]
    secret: String,
}

/// Saves the key once the service accepts it. A new finder goes last, and a new key for one already connected keeps its place.
async fn connect(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<KeyBody>,
) -> AppResult<Json<Finders>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let finder = Finder::parse(&name).ok_or_else(|| AppError::BadRequest("Pick an email finder".into()))?;
    let (key, secret) = (body.key.trim(), body.secret.trim());
    if key.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} {}", finder.label(), if matches!(finder, Finder::Snov) { "API user ID" } else { "API key" })));
    }
    let key = match finder {
        Finder::Snov if secret.is_empty() => return Err(AppError::BadRequest("Paste your Snov.io API secret".into())),
        Finder::Snov => format!("{key}:{secret}"),
        _ => key.to_string(),
    };
    finder.check_key(&key).await.map_err(|e| AppError::BadRequest(format!("{} didn't accept that key: {e:#}", finder.label())))?;
    sqlx::query(
        "insert into project_finders (project_id, provider, key_enc, position)
         values ($1, $2, $3, (select coalesce(max(position) + 1, 0) from project_finders where project_id = $1))
         on conflict (project_id, provider) do update set key_enc = excluded.key_enc, error = null, failed_at = null, paused_until = null",
    )
    .bind(id)
    .bind(&name)
    .bind(crypto::encrypt(&state.cfg.app_secret, &key))
    .execute(&state.db)
    .await?;
    finders(&state.db, id).await
}

async fn disconnect(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<Finders>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    sqlx::query("delete from project_finders where project_id = $1 and provider = $2").bind(id).bind(&name).execute(&state.db).await?;
    finders(&state.db, id).await
}

#[derive(Deserialize)]
struct OrderBody {
    providers: Vec<String>,
}

/// Every connected finder, in the order to ask them.
async fn reorder(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<OrderBody>) -> AppResult<Json<Finders>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    let mut connected: Vec<String> = sqlx::query_scalar("select provider from project_finders where project_id = $1").bind(id).fetch_all(&state.db).await?;
    let mut asked = body.providers.clone();
    connected.sort();
    asked.sort();
    if connected != asked {
        return Err(AppError::BadRequest("List every connected finder once".into()));
    }
    set_order(&state.db, id, &body.providers).await?;
    finders(&state.db, id).await
}

pub(crate) async fn set_order(db: &PgPool, project_id: Uuid, providers: &[String]) -> sqlx::Result<()> {
    sqlx::query(
        "update project_finders f set position = o.position - 1
         from unnest($2::text[]) with ordinality as o(provider, position) where f.project_id = $1 and f.provider = o.provider",
    )
    .bind(project_id)
    .bind(providers)
    .execute(db)
    .await?;
    Ok(())
}

#[derive(Deserialize)]
struct PhonesBody {
    on: bool,
}

async fn set_phones(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<PhonesBody>) -> AppResult<Json<Finders>> {
    auth.project(&state.db, id).await?;
    load_or_create(&state.db, id).await?;
    sqlx::query("update project_settings set find_phones = $2 where project_id = $1").bind(id).bind(body.on).execute(&state.db).await?;
    finders(&state.db, id).await
}
