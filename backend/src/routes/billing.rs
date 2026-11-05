use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::Sha256;
use uuid::Uuid;

use crate::auth::Auth;
use crate::billing;
use crate::error::{AppError, AppResult};
use crate::models::{BillingProfile, LedgerEntry};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/billing", get(summary))
        .route("/billing/profile", get(get_profile).put(update_profile))
        .route("/billing/topup", post(topup))
        .route("/billing/stripe-webhook", post(stripe_webhook))
}

#[derive(Serialize)]
struct Summary {
    balance_micros: i64,
    spent_today_micros: i64,
    total_spent_micros: i64,
    emails_sent: i64,
    ledger: Vec<LedgerEntry>,
    stripe_enabled: bool,
    payments_enabled: bool,
}

async fn summary(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Summary>> {
    let (total_spent_micros, emails_sent): (i64, i64) = sqlx::query_as(
        "select coalesce(-sum(amount_micros), 0)::bigint, count(*) from credit_ledger
         where org_id = $1 and kind = 'email_sent'",
    )
    .bind(auth.org_id)
    .fetch_one(&state.db)
    .await?;
    let ledger = sqlx::query_as::<_, LedgerEntry>(
        "select * from credit_ledger where org_id = $1 order by created_at desc limit 100",
    )
    .bind(auth.org_id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(Summary {
        balance_micros: billing::balance(&state.db, auth.org_id).await?,
        spent_today_micros: billing::spent_today(&state.db, auth.org_id).await?,
        total_spent_micros,
        emails_sent,
        ledger,
        stripe_enabled: state.cfg.stripe_secret_key.is_some(),
        payments_enabled: state.cfg.payments_enabled,
    }))
}

async fn get_profile(State(state): State<AppState>, auth: Auth) -> AppResult<Json<BillingProfile>> {
    let profile = sqlx::query_as::<_, BillingProfile>(
        "insert into billing_profiles (org_id) values ($1)
         on conflict (org_id) do update set org_id = excluded.org_id returning *",
    )
    .bind(auth.org_id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(profile))
}

#[derive(Deserialize)]
struct ProfileBody {
    company_name: Option<String>,
    address_line1: Option<String>,
    address_line2: Option<String>,
    city: Option<String>,
    postal_code: Option<String>,
    country: Option<String>,
    vat_id: Option<String>,
}

async fn update_profile(State(state): State<AppState>, auth: Auth, Json(body): Json<ProfileBody>) -> AppResult<Json<BillingProfile>> {
    auth.require_admin()?;
    let profile = sqlx::query_as::<_, BillingProfile>(
        "insert into billing_profiles (org_id, company_name, address_line1, address_line2, city, postal_code, country, vat_id)
         values ($1, coalesce($2, ''), coalesce($3, ''), coalesce($4, ''), coalesce($5, ''), coalesce($6, ''), coalesce($7, ''), coalesce($8, ''))
         on conflict (org_id) do update set
           company_name = coalesce($2, billing_profiles.company_name),
           address_line1 = coalesce($3, billing_profiles.address_line1),
           address_line2 = coalesce($4, billing_profiles.address_line2),
           city = coalesce($5, billing_profiles.city),
           postal_code = coalesce($6, billing_profiles.postal_code),
           country = coalesce($7, billing_profiles.country),
           vat_id = coalesce($8, billing_profiles.vat_id),
           updated_at = now()
         returning *",
    )
    .bind(auth.org_id)
    .bind(body.company_name.as_deref().map(str::trim))
    .bind(body.address_line1.as_deref().map(str::trim))
    .bind(body.address_line2.as_deref().map(str::trim))
    .bind(body.city.as_deref().map(str::trim))
    .bind(body.postal_code.as_deref().map(str::trim))
    .bind(body.country.as_deref().map(str::trim))
    .bind(body.vat_id.as_deref().map(str::trim))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(profile))
}

#[derive(Deserialize)]
struct TopupBody {
    amount_cents: i64,
}

async fn topup(State(state): State<AppState>, auth: Auth, Json(body): Json<TopupBody>) -> AppResult<Json<Value>> {
    if !state.cfg.payments_enabled {
        return Err(AppError::NotConfigured("Payments are disabled on this server (PAYMENTS_ENABLED=false).".into()));
    }
    let key = state.cfg.stripe_secret_key.as_deref().ok_or_else(|| {
        AppError::NotConfigured("Stripe is not configured. Set STRIPE_SECRET_KEY to enable top-ups.".into())
    })?;
    if body.amount_cents < 1000 {
        return Err(AppError::BadRequest("The minimum top-up is $10".into()));
    }
    let org_id = auth.org_id.to_string();
    let amount = body.amount_cents.to_string();
    let success_url = format!("{}/app/billing?topup=success", state.cfg.public_url);
    let cancel_url = format!("{}/app/billing?topup=cancelled", state.cfg.public_url);
    let form = [
        ("mode", "payment"),
        ("client_reference_id", org_id.as_str()),
        ("metadata[org_id]", org_id.as_str()),
        ("success_url", success_url.as_str()),
        ("cancel_url", cancel_url.as_str()),
        ("line_items[0][quantity]", "1"),
        ("line_items[0][price_data][currency]", "usd"),
        ("line_items[0][price_data][unit_amount]", amount.as_str()),
        ("line_items[0][price_data][product_data][name]", "Splee credits"),
    ];
    let res = state
        .http
        .post("https://api.stripe.com/v1/checkout/sessions")
        .bearer_auth(key)
        .form(&form)
        .send()
        .await
        .map_err(anyhow::Error::from)?;
    let status = res.status();
    let session: Value = res.json().await.map_err(anyhow::Error::from)?;
    if !status.is_success() {
        let message = session["error"]["message"].as_str().unwrap_or("Stripe rejected the request");
        return Err(AppError::BadRequest(format!("Stripe: {message}")));
    }
    let checkout_url = session["url"].as_str().ok_or_else(|| anyhow::anyhow!("Stripe response has no url"))?;
    Ok(Json(json!({ "checkout_url": checkout_url })))
}

const SIGNATURE_TOLERANCE_SECS: i64 = 300;

fn verify_stripe_signature(secret: &str, header: &str, payload: &[u8]) -> bool {
    let mut timestamp = None;
    let mut signatures = Vec::new();
    for part in header.split(',') {
        match part.split_once('=') {
            Some(("t", t)) => timestamp = t.parse::<i64>().ok(),
            Some(("v1", sig)) => signatures.extend(hex::decode(sig).ok()),
            _ => {}
        }
    }
    let Some(timestamp) = timestamp else { return false };
    if (chrono::Utc::now().timestamp() - timestamp).abs() > SIGNATURE_TOLERANCE_SECS {
        return false;
    }
    signatures.iter().any(|sig| {
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
        mac.update(timestamp.to_string().as_bytes());
        mac.update(b".");
        mac.update(payload);
        mac.verify_slice(sig).is_ok()
    })
}

async fn stripe_webhook(State(state): State<AppState>, headers: HeaderMap, payload: Bytes) -> AppResult<Json<Value>> {
    let secret = state
        .cfg
        .stripe_webhook_secret
        .as_deref()
        .ok_or_else(|| AppError::NotConfigured("STRIPE_WEBHOOK_SECRET is not set".into()))?;
    let header = headers.get("stripe-signature").and_then(|v| v.to_str().ok()).unwrap_or("");
    if !verify_stripe_signature(secret, header, &payload) {
        return Err(AppError::BadRequest("Invalid Stripe signature".into()));
    }
    let event: Value = serde_json::from_slice(&payload).map_err(|_| AppError::BadRequest("Invalid JSON".into()))?;
    let session = &event["data"]["object"];
    if event["type"] != "checkout.session.completed" || session["payment_status"] != "paid" {
        return Ok(Json(json!({ "received": true })));
    }

    let org_id = session["metadata"]["org_id"]
        .as_str()
        .or(session["client_reference_id"].as_str())
        .and_then(|id| Uuid::parse_str(id).ok())
        .ok_or_else(|| AppError::BadRequest("Checkout session has no org id".into()))?;
    let session_id = session["id"].as_str().ok_or_else(|| AppError::BadRequest("Checkout session has no id".into()))?;
    let amount_cents = session["amount_total"].as_i64().unwrap_or(0);
    let note = format!("Stripe checkout {session_id}");

    // Stripe retries webhooks, sometimes concurrently. The lock makes the exists-check and insert atomic per session.
    let mut tx = state.db.begin().await?;
    sqlx::query("select pg_advisory_xact_lock(hashtext($1))").bind(&note).execute(&mut *tx).await?;
    let already: bool = sqlx::query_scalar("select exists(select 1 from credit_ledger where kind = 'topup' and note = $1)")
        .bind(&note)
        .fetch_one(&mut *tx)
        .await?;
    if !already {
        billing::add_entry(&mut *tx, org_id, amount_cents * 10_000, "topup", None, &note).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "received": true })))
}
