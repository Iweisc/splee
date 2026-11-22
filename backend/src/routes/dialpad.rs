//! Connecting a project's Dialpad company (crate::dialpad) and the webhook Splee registers there, and starting a call in
//! Dialpad from the Calls page. The webhook URL has a per-connection token, and an event is only read once its JWT is signed
//! with the secret Splee gave that webhook.

use axum::extract::{Path, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_scope;
use crate::audit;
use crate::auth::Auth;
use crate::calls;
use crate::crypto;
use crate::dialpad::{self, CONNECTION_COLUMNS, Client, Connection};
use crate::error::{AppError, AppResult};
use crate::models::{Call, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/dialpad", get(status).put(connect).delete(disconnect))
        .route("/dialpad-webhooks/{token}", post(webhook))
        .route("/calls/{id}/dialpad-dial", post(dial))
}

/// The connection as the integrations page shows it. The API key never leaves the server.
#[derive(Debug, Serialize, FromRow)]
pub struct DialpadStatus {
    pub last_event_at: Option<Ts>,
    /// Why the last event couldn't be handled, cleared by the next one that is.
    pub error: String,
    /// Calls with the project's leads kept from it.
    pub calls: i64,
    pub created_at: Ts,
}

async fn load(db: &PgPool, project_id: Uuid) -> AppResult<Json<Option<DialpadStatus>>> {
    let row = sqlx::query_as(
        "select d.last_event_at, d.error, (select count(*) from calls c where c.project_id = d.project_id and c.dialpad_id is not null) as calls,
           d.created_at
         from dialpad_connections d where d.project_id = $1",
    )
    .bind(project_id)
    .fetch_optional(db)
    .await?;
    Ok(Json(row))
}

async fn status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<DialpadStatus>>> {
    auth.project(&state.db, id).await?;
    load(&state.db, id).await
}

#[derive(Deserialize)]
struct ConnectBody {
    api_key: String,
}

/// Checks the API key, then registers Splee's webhook and its call subscription in Dialpad. Connecting again keeps the webhook
/// URL, registers a new webhook with a new secret and deletes the old one.
async fn connect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<ConnectBody>) -> AppResult<Json<Option<DialpadStatus>>> {
    auth.project(&state.db, id).await?;
    let api_key = body.api_key.trim();
    if api_key.is_empty() {
        return Err(AppError::BadRequest("Paste the API key".into()));
    }
    let client = Client::new(&state.cfg.dialpad_api_url, api_key)?;
    client.ping().await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let old: Option<(String, String, String, String)> =
        sqlx::query_as("select path_token, webhook_id, subscription_id, api_key_enc from dialpad_connections where project_id = $1")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let path_token = old.as_ref().map_or_else(|| Uuid::new_v4().simple().to_string(), |(token, ..)| token.clone());
    let url = format!("{}/api/dialpad-webhooks/{path_token}", state.cfg.public_url.trim_end_matches('/'));
    let secret = crypto::random_token();
    let (webhook_id, subscription_id) = client.hook(&url, &secret).await?;
    let upsert = sqlx::query(
        "insert into dialpad_connections (project_id, org_id, api_key_enc, webhook_id, subscription_id, secret_enc, path_token)
         values ($1, $2, $3, $4, $5, $6, $7)
         on conflict (project_id) do update set api_key_enc = excluded.api_key_enc, webhook_id = excluded.webhook_id,
           subscription_id = excluded.subscription_id, secret_enc = excluded.secret_enc, error = ''",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(crypto::encrypt(&state.cfg.app_secret, api_key))
    .bind(&webhook_id)
    .bind(&subscription_id)
    .bind(crypto::encrypt(&state.cfg.app_secret, &secret))
    .bind(&path_token);
    let saved = match upsert.execute(&mut *tx).await {
        Ok(_) => tx.commit().await,
        Err(e) => Err(e),
    };
    if let Err(e) = saved {
        // Otherwise Dialpad would keep sending this webhook's events, signed with a secret Splee doesn't have.
        client.unhook(&webhook_id, &subscription_id).await;
        return Err(e.into());
    }
    // The old webhook may be in another Dialpad company, which only the old key reaches.
    if let Some((_, old_webhook, old_subscription, old_key)) = old.filter(|(_, w, ..)| *w != webhook_id) {
        match crypto::decrypt(&state.cfg.app_secret, &old_key).map_err(AppError::from).and_then(|key| Client::new(&state.cfg.dialpad_api_url, &key)) {
            Ok(old_client) => old_client.unhook(&old_webhook, &old_subscription).await,
            Err(e) => tracing::warn!(error = ?e, "dialpad: the old key couldn't delete the old webhook"),
        }
    }
    audit::integration(&state.db, &auth, Some(id), "Dialpad", true).await;
    load(&state.db, id).await
}

/// Calls already kept stay, with their notes.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<DialpadStatus>>> {
    auth.project(&state.db, id).await?;
    let mut tx = state.db.begin().await?;
    lock(&mut tx, id).await?;
    let connection: Option<Connection> =
        sqlx::query_as(&format!("delete from dialpad_connections where project_id = $1 returning {CONNECTION_COLUMNS}")).bind(id).fetch_optional(&mut *tx).await?;
    tx.commit().await?;
    if let Some(c) = connection {
        c.client(&state)?.unhook(&c.webhook_id, &c.subscription_id).await;
        audit::integration(&state.db, &auth, Some(id), "Dialpad", false).await;
    }
    load(&state.db, id).await
}

/// Calls the lead from the caller's Dialpad app, after the checks a call from the browser gets. The call carries this step's
/// id, so when it ends the webhook brings it back and it closes this step. A refusal is a 409 with the reason.
async fn dial(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let call: Call = util_scope::load(&state.db, "calls", id, auth.org_id).await?;
    let (_, lead) = calls::check(&state.db, auth.org_id, auth.user_id, call.id, None, Utc::now()).await?.map_err(AppError::Conflict)?;
    let connection: Option<Connection> = sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from dialpad_connections where project_id = $1"))
        .bind(call.project_id)
        .fetch_optional(&state.db)
        .await?;
    let c = connection.ok_or_else(|| AppError::Conflict("Connect Dialpad on the Integrations page to call from it.".into()))?;
    let email: String = sqlx::query_scalar("select email::text from users where id = $1").bind(auth.user_id).fetch_one(&state.db).await?;
    c.client(&state)?.dial(&email, &lead.phone, call.id).await?;
    Ok(Json(json!({ "to": lead.phone })))
}

/// Connecting and disconnecting a project's Dialpad go one at a time, so two connects at once can't leave the row with one
/// webhook's id and the other's secret. Held while Dialpad makes the webhook.
async fn lock(tx: &mut sqlx::PgConnection, project_id: Uuid) -> AppResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('dialpad:' || $1::text, 0))").bind(project_id).execute(tx).await?;
    Ok(())
}

/// An event from Dialpad, a JWT signed with the webhook's secret. A repeated event changes nothing. One that fails keeps why
/// on the connection and answers an error, so Dialpad tries it again.
async fn webhook(State(state): State<AppState>, Path(token): Path<String>, body: String) -> AppResult<Json<Value>> {
    let connection: Option<Connection> = sqlx::query_as(&format!("select {CONNECTION_COLUMNS} from dialpad_connections where path_token = $1"))
        .bind(&token)
        .fetch_optional(&state.db)
        .await?;
    let c = connection.ok_or(AppError::NotFound)?;
    let event = crypto::verify_hs256_jwt(c.secret(&state)?.as_bytes(), &body).ok_or(AppError::Unauthorized)?;
    let handled = dialpad::handle(&state, &c, &event).await;
    let error = handled.as_ref().err().map(|e| e.public_message()).unwrap_or_default();
    sqlx::query("update dialpad_connections set last_event_at = now(), error = $2 where project_id = $1").bind(c.project_id).bind(&error).execute(&state.db).await?;
    Ok(Json(json!({ "matched": handled? })))
}
