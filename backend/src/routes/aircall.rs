//! Connecting a project's Aircall company (crate::aircall) and the webhook Splee registers there, and starting a call in
//! Aircall from the Calls page. The webhook URL has a per-connection token, and an event is only read once the token in its
//! body is the one Aircall gave for that webhook.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_scope;
use crate::aircall::{self, CONNECTION_COLUMNS, Client, Connection, TOKEN_PURPOSE};
use crate::audit;
use crate::auth::Auth;
use crate::calls;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::models::{Call, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/aircall", get(status).put(connect).delete(disconnect))
        .route("/aircall-webhooks/{token}", post(webhook))
        .route("/calls/{id}/aircall-dial", post(dial))
}

/// The connection as the integrations page shows it. The API token never leaves the server.
#[derive(Debug, Serialize, FromRow)]
pub struct AircallStatus {
    pub last_event_at: Option<Ts>,
    /// Why the last event couldn't be handled, cleared by the next one that is.
    pub error: String,
    /// Calls with the project's leads kept from it.
    pub calls: i64,
    pub created_at: Ts,
}

async fn load(db: &PgPool, project_id: Uuid) -> AppResult<Json<Option<AircallStatus>>> {
    let row = sqlx::query_as(
        "select a.last_event_at, a.error, (select count(*) from calls c where c.project_id = a.project_id and c.aircall_id is not null) as calls,
           a.created_at
         from aircall_connections a where a.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(Json(row))
}

async fn status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<AircallStatus>>> {
    auth.project(&state.db, id).await?;
    load(&state.db, id).await
}

#[derive(Deserialize)]
struct ConnectBody {
    api_id: String,
    api_token: String,
}

/// Checks the API ID and token, then registers Splee's webhook in Aircall. Connecting again keeps the webhook URL, registers
/// a new webhook with the new details and deletes the old one.
async fn connect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ConnectBody>) -> AppResult<Json<Option<AircallStatus>>> {
    auth.project(&state.db, id).await?;
    let (api_id, api_token) = (body.api_id.trim(), body.api_token.trim());
    if api_id.is_empty() || api_token.is_empty() {
        return Err(AppError::BadRequest("Paste the API ID and the API token".into()));
    }
    let credentials = format!("{api_id}:{api_token}");
    let client = Client::new(&state.cfg.aircall_api_url, &credentials)?;
    client.ping().await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let old: Option<(String, String)> = sqlx::query_as("select path_token, webhook_id from aircall_connections where project_id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let path_token = old.as_ref().map_or_else(|| Uuid::new_v4().simple().to_string(), |(token, _)| token.clone());
    let url = format!("{}/api/aircall-webhooks/{path_token}", state.cfg.public_url.trim_end_matches('/'));
    let (webhook_id, token) = client.create_webhook(&url).await?;
    let upsert = sqlx::query(
        "insert into aircall_connections (project_id, org_id, credentials_enc, webhook_id, webhook_token_hash, path_token) values ($1, $2, $3, $4, $5, $6)
         on conflict (project_id) do update set credentials_enc = excluded.credentials_enc, webhook_id = excluded.webhook_id,
           webhook_token_hash = excluded.webhook_token_hash, error = ''",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(crypto::encrypt(&state.cfg.app_secret, &credentials))
    .bind(&webhook_id)
    .bind(crypto::keyed_hash(&state.cfg.app_secret, TOKEN_PURPOSE, &token))
    .bind(&path_token);
    let saved = match upsert.execute(&mut *tx).await {
        Ok(_) => tx.commit().await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        // Otherwise Aircall would keep sending this webhook's events, with a token Splee doesn't have.
        client.delete_webhook(&webhook_id).await;
        return Err(e.into());
    }
    // The old webhook may be in another Aircall company, which these details can't reach. Aircall turns it off on its own
    // once its events keep failing here.
    if let Some((_, old_webhook)) = old.filter(|(_, w)| *w != webhook_id) {
        client.delete_webhook(&old_webhook).await;
    }
    audit::integration(&state.db, &auth, Some(id), "Aircall", true).await;
    load(&state.db, id).await
}

/// Calls already kept stay, with their notes.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<AircallStatus>>> {
    auth.project(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let connection: Option<Connection> =
        sqlx::query_as(&format!("delete from aircall_connections where project_id = $1 returning {CONNECTION_COLUMNS}")).bind(id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    if let Some(c) = connection {
        c.client(&state)?.delete_webhook(&c.webhook_id).await;
        audit::integration(&state.db, &auth, Some(id), "Aircall", false).await;
    }
    load(&state.db, id).await
}

/// Puts the lead's number in the caller's Aircall Workspace, after the checks a call from the browser gets. When the call
/// ends, the webhook brings it back and it closes this one, as any call with the lead does. A refusal is a 409 with the reason.
async fn dial(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let (_, lead) = calls::check(&state.db, auth.org_id, auth.user_id, call.id, None, Utc::now()).await?.map_err(AppError::Conflict)?;
    let connection: Option<Connection> = sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from aircall_connections where project_id = $1"))
        .bind(call.project_id)
        .fetch_optional(&state.db)
        .await?;
    let c = connection.ok_or_else(|| AppError::Conflict("Connect Aircall on the Integrations page to call from it.".into()))?;
    let email: String = sqlx::query_scalar("select email::text from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    c.client(&state)?.dial(&email, &lead.phone).await?;
    // So the call Aircall sends back closes this step, not another lead's with the number or an earlier one. See aircall::record.
    sqlx::query("update calls set aircall_dialed_by = $2, aircall_dialed_at = now() where id = $1 and status = 'open'")
        .bind(call.id)
        .bind(auth.user_id)
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "to": lead.phone })))
}

/// Connecting and disconnecting a project's Aircall go one at a time. Two connects at once would each make a webhook, and the
/// row could end up with one's URL and the other's token, which no event matches. Held while Aircall makes the webhook.
async fn lock(tx: &mut sqlx::PgConnection, project_id: Uuid) -> AppResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('aircall:' || $1::text, 0))").bind(project_id).execute(tx).await?;
    Ok(())
}

/// An event from Aircall. Aircall delivers at least once, and a repeated event changes nothing. One that fails keeps why on
/// the connection and answers an error, so Aircall tries it again.
async fn webhook(State(state): State<AppState>, Path(token): Path<String>, Json(event): Json<Value>) -> AppResult<Json<Value>> {
    let connection: Option<Connection> = sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from aircall_connections where path_token = $1"))
        .bind(&token)
        .fetch_optional(&state.db)
        .await?;
    let c = connection.ok_or(AppError::NotFound)?;
    let given = event["token"].as_str().unwrap_or_default();
    if given.is_empty() || !crypto::verify_keyed_hash(&state.cfg.app_secret, TOKEN_PURPOSE, given, &c.webhook_token_hash) {
        return Err(AppError::Unauthorized);
    }
    let handled = aircall::handle(&state, &c, &event).await;
    let error = handled.as_ref().err().map(|e| e.public_message()).unwrap_or_default();
    sqlx::query("update aircall_connections set last_event_at = now(), error = $2 where project_id = $1").bind(c.project_id).bind(&error).execute(&state.db).await?;
    Ok(Json(json!({ "matched": handled? })))
}
