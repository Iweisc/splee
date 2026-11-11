//! Connecting Stripe, Paddle and Lemon Squeezy (payments/), and their webhooks. Connecting checks the key and queues a look
//! back over 90 days. The webhook URL has a per-connection token, and a delivery is only read once its signature matches
//! the signing secret the user pasted from the provider.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::auth::Auth;
use crate::crypto;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::payments::{self, Client, Connection, Provider};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/payments", get(list))
        .route("/projects/{id}/payments/{provider}", put(connect).delete(disconnect))
        .route("/projects/{id}/payments/{provider}/secret", put(save_secret))
        .route("/payments/{provider}/{token}", post(webhook))
}

/// A connection as the integrations page shows it. The key and the signing secret never leave the server.
#[derive(Debug, Serialize, FromRow)]
pub struct PaymentConnection {
    pub provider: String,
    /// The secret in the webhook URL, /api/payments/{provider}/{webhook_token}.
    pub webhook_token: String,
    pub has_secret: bool,
    pub backfill_status: String,
    pub backfill_error: String,
    pub backfill_found: i32,
    pub backfill_matched: i32,
    pub backfilled_at: Option<Ts>,
    pub last_event_at: Option<Ts>,
    pub created_at: Ts,
}

async fn connections(db: &sqlx::PgPool, project_id: Uuid) -> AppResult<Json<Vec<PaymentConnection>>> {
    let list = sqlx::query_as(
        "select provider, webhook_token, signing_secret_enc <> '' as has_secret, backfill_status, backfill_error, backfill_found, backfill_matched,
           backfilled_at, last_event_at, created_at
         from payment_connections where project_id = $1 order by created_at",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    Ok(Json(list))
}

fn provider(name: &str) -> AppResult<Provider> {
    Provider::parse(name).ok_or_else(|| AppError::BadRequest("Pick Stripe, Paddle or Lemon Squeezy".into()))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<PaymentConnection>>> {
    auth.project(&state.db, id).await?;
    connections(&state.db, id).await
}

#[derive(Deserialize)]
struct KeyBody {
    api_key: String,
}

/// Saves the key once the provider accepts it, and queues the look back. Connecting again with a new key keeps the webhook
/// URL and signing secret.
async fn connect(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<KeyBody>,
) -> AppResult<Json<Vec<PaymentConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    let key = body.api_key.trim();
    if key.is_empty() {
        return Err(AppError::BadRequest(format!("Paste your {} API key", provider.label())));
    }
    // A secret key can move money. Splee only reads, so it asks for a key that can do no more.
    if provider == Provider::Stripe && !key.starts_with("rk_") {
        return Err(AppError::BadRequest("Use a restricted key, which starts with rk_, with read access to customers, charges and refunds".into()));
    }
    Client::new(provider, &state.cfg.payments_api_url, key)?.check().await?;
    sqlx::query(
        "insert into payment_connections (org_id, project_id, provider, api_key_enc) values ($1, $2, $3, $4)
         on conflict (project_id, provider) do update set api_key_enc = excluded.api_key_enc, backfill_error = '',
           backfill_status = case when payment_connections.backfill_status = 'running' then 'running' else 'queued' end",
    )
    .bind(auth.org_id)
    .bind(id)
    .bind(provider.name())
    .bind(crypto::encrypt(&state.cfg.app_secret, key))
    .execute(&state.db)
    .await?;
    connections(&state.db, id).await
}

#[derive(Deserialize)]
struct SecretBody {
    secret: String,
}

async fn save_secret(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<SecretBody>,
) -> AppResult<Json<Vec<PaymentConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    let secret = body.secret.trim();
    if secret.is_empty() {
        return Err(AppError::BadRequest("Paste the webhook's signing secret".into()));
    }
    if provider == Provider::Stripe && !secret.starts_with("whsec_") {
        return Err(AppError::BadRequest("Stripe's signing secret starts with whsec_. Copy it from the webhook's page in Stripe.".into()));
    }
    let saved = sqlx::query("update payment_connections set signing_secret_enc = $3 where project_id = $1 and provider = $2")
        .bind(id)
        .bind(provider.name())
        .bind(crypto::encrypt(&state.cfg.app_secret, secret))
        .execute(&state.db)
        .await?;
    if saved.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    connections(&state.db, id).await
}

/// The payments already kept stay, since they were still paid.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<Vec<PaymentConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    sqlx::query("delete from payment_connections where project_id = $1 and provider = $2").bind(id).bind(provider.name()).execute(&state.db).await?;
    connections(&state.db, id).await
}

#[derive(FromRow)]
struct Hooked {
    #[sqlx(flatten)]
    connection: Connection,
    signing_secret_enc: String,
}

/// A provider's webhook. A delivery seen before is answered without doing anything, and one that fails is not marked seen,
/// so the provider's retry gets another go.
async fn webhook(State(state): State<AppState>, Path((name, token)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> AppResult<Json<Value>> {
    let provider = Provider::parse(&name).ok_or(AppError::NotFound)?;
    let hooked: Hooked = sqlx::query_as(
        "select id, org_id, project_id, provider, api_key_enc, signing_secret_enc from payment_connections where webhook_token = $1 and provider = $2",
    )
    .bind(&token)
    .bind(provider.name())
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    let secret = crypto::decrypt(&state.cfg.app_secret, &hooked.signing_secret_enc)?;
    if secret.is_empty() {
        return Err(AppError::BadRequest("Paste this webhook's signing secret on Splee's Integrations page first".into()));
    }
    if !provider.verify(&secret, &headers, &body, Utc::now().timestamp()) {
        return Err(AppError::Unauthorized);
    }
    let event: Value = serde_json::from_slice(&body).map_err(|_| AppError::BadRequest("The body isn't JSON".into()))?;
    let event_id = provider.event_id(&event, &body).ok_or_else(|| AppError::BadRequest("The event has no id".into()))?;
    let c = &hooked.connection;
    let seen: bool = sqlx::query_scalar("select exists (select 1 from payment_events where connection_id = $1 and event_id = $2)")
        .bind(c.id)
        .bind(&event_id)
        .fetch_one(&state.db)
        .await?;
    if seen {
        return Ok(Json(json!({ "duplicate": true })));
    }
    let charges = provider.charges(&event);
    let client = c.client(&state)?;
    let mut matched = 0;
    for charge in &charges {
        if payments::record(&state, c, &client, charge).await? != payments::Recorded::Unmatched {
            matched += 1;
        }
    }
    sqlx::query("insert into payment_events (connection_id, event_id) values ($1, $2) on conflict do nothing").bind(c.id).bind(&event_id).execute(&state.db).await?;
    sqlx::query("update payment_connections set last_event_at = now() where id = $1").bind(c.id).execute(&state.db).await?;
    Ok(Json(json!({ "matched": matched })))
}
