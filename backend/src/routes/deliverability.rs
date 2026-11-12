//! The Deliverability page's API: the report, a check run now, and Connect Google Postmaster, which signs in through the
//! mailbox sign-in's state and callback (routes/mailbox_oauth.rs). The checks and verdicts are in crate::deliverability.

use axum::extract::State;
use axum::response::Redirect;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};

use super::mailbox_oauth::{self, CallbackQuery, POSTMASTER, SignedIn};
use crate::auth::Auth;
use crate::crypto;
use crate::deliverability::{self, Report, postmaster};
use crate::error::AppResult;
use crate::mail::oauth::Provider;
use crate::models::Ts;
use crate::state::AppState;

const PAGE: &str = "/app/deliverability";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/deliverability", get(page))
        .route("/deliverability/check", post(check))
        .route("/deliverability/postmaster/oauth", post(connect))
        .route("/deliverability/postmaster", axum::routing::delete(disconnect))
}

#[derive(Serialize, sqlx::FromRow)]
struct Connection {
    email: String,
    /// Verified in Postmaster Tools, as of the last sync.
    domains: Vec<String>,
    error: String,
    synced_at: Option<Ts>,
    /// Google refused the sign-in, so it waits to be connected again.
    signed_out: bool,
}

#[derive(Serialize)]
struct Postmaster {
    /// This Splee has a Google app to sign in with.
    available: bool,
    connection: Option<Connection>,
}

#[derive(Serialize)]
struct Page {
    #[serde(flatten)]
    report: Report,
    postmaster: Postmaster,
}

async fn with_postmaster(state: &AppState, auth: &Auth, report: Report) -> AppResult<Json<Page>> {
    let connection = sqlx::query_as::<_, Connection>(
        "select email, domains, error, synced_at, refresh_token_enc = '' as signed_out from postmaster_connections where org_id = $1",
    )
    .bind(auth.org_id)
    .fetch_optional(&state.db)
    .await?;
    Ok(Json(Page { report, postmaster: Postmaster { available: state.cfg.google.is_some(), connection } }))
}

async fn page(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Page>> {
    let report = deliverability::report(&state.db, auth.org_id, Utc::now().date_naive()).await?;
    with_postmaster(&state, &auth, report).await
}

/// Checks the blocklists and syncs Postmaster now instead of waiting for the next 6-hourly check, then judges the
/// mailboxes, which can hold one. A Postmaster failure is saved for the page, not returned.
async fn check(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Page>> {
    auth.require_admin()?;
    let resolver = crate::mail::dns::resolver().map_err(crate::error::AppError::Internal)?;
    deliverability::check_blocklists(&state.db, &resolver, auth.org_id).await.map_err(crate::error::AppError::Internal)?;
    let connected: bool = sqlx::query_scalar("select exists (select 1 from postmaster_connections where org_id = $1 and refresh_token_enc <> '')")
        .bind(auth.org_id)
        .fetch_one(&state.db)
        .await?;
    if connected && let Err(e) = postmaster::sync(&state, auth.org_id).await {
        tracing::info!(error = %e, "Postmaster Tools sync failed");
    }
    let report = deliverability::evaluate(&state, auth.org_id).await.map_err(crate::error::AppError::Internal)?;
    with_postmaster(&state, &auth, report).await
}

async fn connect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    let url = mailbox_oauth::start_sign_in(&state, &auth, Provider::Google, &POSTMASTER, None, postmaster::SCOPE, None).await?;
    Ok(Json(json!({ "url": url })))
}

async fn disconnect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let mut tx = state.db.begin().await?;
    sqlx::query("delete from postmaster_connections where org_id = $1").bind(auth.org_id).execute(&mut *tx).await?;
    sqlx::query("delete from postmaster_days where org_id = $1").bind(auth.org_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

/// Saves the org's Postmaster sign-in and syncs it, then sends the browser to the Deliverability page with
/// `postmaster=connected` or `postmaster_error`.
pub(super) async fn postmaster_callback(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Redirect {
    let params = match save(state, auth, q).await {
        Ok(()) => vec![("postmaster", "connected".to_string())],
        Err(error) => vec![("postmaster_error", error)],
    };
    mailbox_oauth::redirect(PAGE, &params)
}

async fn save(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<(), String> {
    let SignedIn { auth, tokens, account, .. } = mailbox_oauth::signed_in(state, auth, q, &POSTMASTER).await?;
    let refresh = tokens.refresh_token.as_deref().filter(|t| !t.is_empty()).ok_or("Google didn't let Splee stay signed in. Connect Google Postmaster again.")?;
    let secret = &state.cfg.app_secret;
    // Another Google account may not see the same domains, so what the last one saw goes. The sync below fills it back in.
    let saved = async {
        let mut tx = state.db.begin().await?;
        sqlx::query("delete from postmaster_days where org_id = $1").bind(auth.org_id).execute(&mut *tx).await?;
        sqlx::query(
            "insert into postmaster_connections (org_id, email, refresh_token_enc, access_token_enc, expires_at) values ($1, $2, $3, $4, $5)
             on conflict (org_id) do update set email = excluded.email, refresh_token_enc = excluded.refresh_token_enc,
               access_token_enc = excluded.access_token_enc, expires_at = excluded.expires_at, domains = '{}', error = '', synced_at = null",
        )
        .bind(auth.org_id)
        .bind(&account.email)
        .bind(crypto::encrypt(secret, refresh))
        .bind(crypto::encrypt(secret, &tokens.access_token))
        .bind(tokens.expires_at())
        .execute(&mut *tx)
        .await?;
        tx.commit().await
    };
    saved.await.map_err(|e: sqlx::Error| {
        tracing::error!(error = %e, "saving the Postmaster sign-in failed");
        "Something went wrong saving Google Postmaster. Try again.".to_string()
    })?;
    // The page shows how the first sync went, so its error isn't the sign-in's.
    if let Err(e) = postmaster::sync(state, auth.org_id).await {
        tracing::info!(error = %e, "first Postmaster Tools sync failed");
    }
    Ok(())
}
