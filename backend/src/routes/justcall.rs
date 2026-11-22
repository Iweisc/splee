//! Connecting a project's JustCall account (crate::justcall) and the webhook URL Splee adds to its call events. The URL has a
//! per-connection token, and an event is only read once JustCall's signature with the account's API secret checks out.

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::audit;
use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::justcall::{self, CONNECTION_COLUMNS, Client, Connection};
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/projects/{id}/justcall", get(status).put(connect).delete(disconnect)).route("/justcall-webhooks/{token}", post(webhook))
}

/// The connection as the integrations page shows it. The API key and secret never leave the server.
#[derive(Debug, Serialize, FromRow)]
pub struct JustcallStatus {
    pub last_event_at: Option<Ts>,
    /// Why the last event couldn't be handled, cleared by the next one that is.
    pub error: String,
    /// Calls with the project's leads kept from it.
    pub calls: i64,
    pub created_at: Ts,
}

async fn load(db: &PgPool, project_id: Uuid) -> AppResult<Json<Option<JustcallStatus>>> {
    let row = sqlx::query_as(
        "select j.last_event_at, j.error, (select count(*) from calls c where c.project_id = j.project_id and c.justcall_id is not null) as calls,
           j.created_at
         from justcall_connections j where j.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(Json(row))
}

async fn status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<JustcallStatus>>> {
    auth.project(&state.db, id).await?;
    load(&state.db, id).await
}

#[derive(Deserialize)]
struct ConnectBody {
    api_key: String,
    api_secret: String,
}

/// Checks the key and secret, then adds Splee's webhook URL to the call events in JustCall. Connecting again adds a new URL
/// and removes the old one with the old key, so the old account stops sending.
async fn connect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ConnectBody>) -> AppResult<Json<Option<JustcallStatus>>> {
    auth.project(&state.db, id).await?;
    let (api_key, api_secret) = (body.api_key.trim(), body.api_secret.trim());
    if api_key.is_empty() || api_secret.is_empty() {
        return Err(AppError::BadRequest("Paste the API key and the API secret".into()));
    }
    let client = Client::new(&state.cfg.justcall_api_url, api_key, api_secret)?;
    client.ping().await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let old: Option<Connection> =
        sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from justcall_connections where project_id = $1")).bind(id).fetch_optional(&mut *tx).await?;
    // A new token each time, since JustCall won't take the same URL on an event twice and the old one comes off afterwards.
    let path_token = Uuid::new_v4().simple().to_string();
    let url = format!("{}/api/justcall-webhooks/{path_token}", state.cfg.public_url.trim_end_matches('/'));
    let url_ids = client.hook(&url).await?;
    let upsert = sqlx::query(
        "insert into justcall_connections (project_id, org_id, api_key_enc, api_secret_enc, url_ids, path_token)
         values ($1, $2, $3, $4, $5, $6)
         on conflict (project_id) do update set api_key_enc = excluded.api_key_enc, api_secret_enc = excluded.api_secret_enc,
           url_ids = excluded.url_ids, path_token = excluded.path_token, error = ''",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(crypto::encrypt(&state.cfg.app_secret, api_key))
    .bind(crypto::encrypt(&state.cfg.app_secret, api_secret))
    .bind(&url_ids)
    .bind(&path_token);
    let saved = match upsert.execute(&mut *tx).await {
        Ok(_) => tx.commit().await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        // Otherwise JustCall would keep sending to a URL Splee doesn't know.
        client.unhook(&url_ids).await;
        return Err(e.into());
    }
    // The old URL may be on another JustCall account, which only the old key reaches.
    if let Some(old) = old {
        match old.client(&state) {
            Ok(old_client) => old_client.unhook(&old.url_ids).await,
            Err(e) => tracing::warn!(error = ?e, "justcall: the old key couldn't remove the old webhook URL"),
        }
    }
    audit::integration(&state.db, &auth, Some(id), "JustCall", true).await;
    load(&state.db, id).await
}

/// Calls already kept stay, with their notes.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<JustcallStatus>>> {
    auth.project(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let connection: Option<Connection> =
        sqlx::query_as(&format!("delete from justcall_connections where project_id = $1 returning {CONNECTION_COLUMNS}")).bind(id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    if let Some(c) = connection {
        c.client(&state)?.unhook(&c.url_ids).await;
        audit::integration(&state.db, &auth, Some(id), "JustCall", false).await;
    }
    load(&state.db, id).await
}

/// Connecting and disconnecting a project's JustCall go one at a time, so two connects at once can't leave a URL in JustCall
/// that no row knows. Held while JustCall adds the URL.
async fn lock(tx: &mut sqlx::PgConnection, project_id: Uuid) -> AppResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('justcall:' || $1::text, 0))").bind(project_id).execute(tx).await?;
    Ok(())
}

/// An event from JustCall. A repeated event changes nothing. One that fails keeps why on the connection and answers an error,
/// so JustCall tries it again.
async fn webhook(State(state): State<AppState>, Path(token): Path<String>, headers: HeaderMap, body: String) -> AppResult<Json<Value>> {
    // Read whatever the content type says, since a 415 would read to JustCall as a URL that doesn't work.
    let event: Value = serde_json::from_str(&body).map_err(|_| AppError::BadRequest("Not JSON".into()))?;
    // JustCall checks a URL before adding it to an event with a post that has no call, and only adds it on a 200. That comes
    // while Splee waits on JustCall to add it, before its token is saved, and a post without a call does nothing anyway.
    if !event["data"].is_object() {
        return Ok(Json(json!({ "matched": false })));
    }
    let connection: Option<Connection> = sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from justcall_connections where path_token = $1"))
        .bind(&token)
        .fetch_optional(&state.db)
        .await?;
    let c = connection.ok_or(AppError::NotFound)?;
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default();
    // Projects on one JustCall account share its secret, so a signature only counts for the URL it was sent to.
    let ours = event["webhook_url"].as_str().is_some_and(|url| url.ends_with(&format!("/api/justcall-webhooks/{token}")));
    if !ours || !justcall::signed(&c.secret(&state)?, &event, header("x-justcall-request-timestamp"), header("x-justcall-signature")) {
        return Err(AppError::Unauthorized);
    }
    let handled = justcall::handle(&state, &c, &event).await;
    let error = handled.as_ref().err().map(|e| e.public_message()).unwrap_or_default();
    sqlx::query("update justcall_connections set last_event_at = now(), error = $2 where project_id = $1").bind(c.project_id).bind(&error).execute(&state.db).await?;
    Ok(Json(json!({ "matched": handled? })))
}
