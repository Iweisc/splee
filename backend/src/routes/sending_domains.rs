use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use reqwest::Url;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_input;
use crate::auth::Auth;
use crate::crypto::{decrypt, encrypt};
use crate::domains::{self, Order, Setup};
use crate::error::{AppError, AppResult};
use crate::mail;
use crate::models::{RegistrarAccount, SendingDomain};
use crate::porkbun::{Availability, Porkbun};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/registrar", get(registrar).put(connect).delete(disconnect))
        .route("/sending-domains", get(list).post(buy))
        .route("/sending-domains/suggestions", get(suggestions))
        .route("/sending-domains/{id}", axum::routing::delete(remove))
        .route("/sending-domains/{id}/confirm", post(confirm))
        .route("/sending-domains/{id}/dns", post(set_dns))
        .route("/sending-domains/{id}/records", post(add_record))
        .route("/sending-domains/{id}/check-dns", post(check_dns))
}

async fn account(state: &AppState, org_id: Uuid) -> AppResult<Option<RegistrarAccount>> {
    Ok(sqlx::query_as("select * from registrar_accounts where org_id = $1").bind(org_id).fetch_optional(&state.db).await?)
}

async fn porkbun(state: &AppState, org_id: Uuid) -> AppResult<Porkbun> {
    let a = account(state, org_id).await?.ok_or_else(|| AppError::BadRequest("Connect your Porkbun account first".into()))?;
    let secret = &state.cfg.app_secret;
    Ok(Porkbun::new(&state.cfg.porkbun_api_url, &decrypt(secret, &a.api_key_enc)?, &decrypt(secret, &a.secret_enc)?))
}

async fn load(state: &AppState, id: Uuid, org_id: Uuid) -> AppResult<SendingDomain> {
    domains::load(&state.db, id, org_id).await?.ok_or(AppError::NotFound)
}

async fn registrar(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Option<RegistrarAccount>>> {
    Ok(Json(account(&state, auth.org_id).await?))
}

#[derive(Deserialize)]
struct ConnectBody {
    api_key: String,
    secret: String,
}

async fn connect(State(state): State<AppState>, auth: Auth, Json(body): Json<ConnectBody>) -> AppResult<Json<RegistrarAccount>> {
    auth.require_admin()?;
    let (key, secret) = (body.api_key.trim(), body.secret.trim());
    if key.is_empty() || secret.is_empty() {
        return Err(AppError::BadRequest("Paste both the API key and the secret".into()));
    }
    let name = Porkbun::new(&state.cfg.porkbun_api_url, key, secret).ping().await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let account = sqlx::query_as::<_, RegistrarAccount>(
        "insert into registrar_accounts (org_id, account, api_key_enc, secret_enc) values ($1, $2, $3, $4)
         on conflict (org_id) do update set account = excluded.account, api_key_enc = excluded.api_key_enc, secret_enc = excluded.secret_enc, created_at = now()
         returning *",
    )
    .bind(auth.org_id)
    .bind(name)
    .bind(encrypt(&state.cfg.app_secret, key))
    .bind(encrypt(&state.cfg.app_secret, secret))
    .fetch_one(&state.db)
    .await?;
    Ok(Json(account))
}

async fn disconnect(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    sqlx::query("delete from registrar_accounts where org_id = $1").bind(auth.org_id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<SendingDomain>>> {
    Ok(Json(domains::list(&state.db, auth.org_id).await?))
}

#[derive(Deserialize)]
struct SuggestQuery {
    domain: String,
}

async fn suggestions(State(state): State<AppState>, auth: Auth, Query(q): Query<SuggestQuery>) -> AppResult<Json<Vec<Availability>>> {
    let names = domains::suggestions(&q.domain);
    if names.is_empty() {
        return Err(AppError::BadRequest("Type your company's domain, like acme.com".into()));
    }
    let pb = porkbun(&state, auth.org_id).await?;
    let mut found = pb.check(&names).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    // Buyable first, each group in the order suggested.
    found.sort_by_key(|a| a.available != Some(true) || a.premium);
    Ok(Json(found))
}

#[derive(Deserialize)]
struct BuyItem {
    domain: String,
    price_micros: i64,
}

#[derive(Deserialize)]
struct BuyBody {
    domains: Vec<BuyItem>,
    provider: String,
    #[serde(default)]
    forward_to: String,
    #[serde(default)]
    dmarc_email: String,
}

/// Queues the domains and buys them in the background, since each create waits on its registry. The list shows progress.
async fn buy(State(state): State<AppState>, auth: Auth, Json(body): Json<BuyBody>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    if body.domains.is_empty() || body.domains.len() > 10 {
        return Err(AppError::BadRequest("Pick between 1 and 10 domains".into()));
    }
    util_input::one_of("provider", &body.provider, domains::PROVIDERS)?;
    let mut orders = vec![];
    for item in &body.domains {
        let domain = util_input::domain(&item.domain).filter(|d| *d == item.domain.trim()).ok_or_else(|| AppError::BadRequest(format!("{} isn't a domain", item.domain)))?;
        // Porkbun takes whole cents and refuses single orders over $100.
        if item.price_micros <= 0 || item.price_micros % 10_000 != 0 || item.price_micros > 100_000_000 {
            return Err(AppError::BadRequest(format!("Search again for the price of {domain}")));
        }
        orders.push(Order { domain, price_micros: item.price_micros });
    }
    let forward_to = body.forward_to.trim();
    let forward_to = if forward_to.is_empty() || forward_to.contains("://") { forward_to.to_string() } else { format!("https://{forward_to}") };
    if !forward_to.is_empty() && !Url::parse(&forward_to).is_ok_and(|u| matches!(u.scheme(), "http" | "https") && u.host_str().is_some_and(|h| h.contains('.'))) {
        return Err(AppError::BadRequest("The forward needs a website address, like https://acme.com".into()));
    }
    let dmarc_email = match body.dmarc_email.trim() {
        "" => String::new(),
        raw => util_input::email(raw).ok_or_else(|| AppError::BadRequest("The DMARC report address isn't an email address".into()))?,
    };
    let pb = porkbun(&state, auth.org_id).await?;
    let setup = Setup { provider: body.provider, forward_to, dmarc_email };
    let ids = domains::queue(&state.db, auth.org_id, &orders, &setup).await?;
    let queued = domains::list(&state.db, auth.org_id).await?;
    let skipped: Vec<&str> = orders.iter().map(|o| o.domain.as_str()).filter(|d| !queued.iter().any(|q| ids.contains(&q.id) && q.domain == *d)).collect();
    let (db, tracking) = (state.db.clone(), crate::tracking::cname_target(&state.cfg.public_url));
    tokio::spawn(async move { domains::buy(&db, &pb, &ids, tracking.as_deref()).await });
    Ok(Json(json!({ "domains": queued, "skipped": skipped })))
}

async fn confirm(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SendingDomain>> {
    let d = load(&state, id, auth.org_id).await?;
    if d.status != "uncertain" {
        return Err(AppError::BadRequest("Only a purchase that ended unclear needs checking".into()));
    }
    let pb = porkbun(&state, auth.org_id).await?;
    let tracking = crate::tracking::cname_target(&state.cfg.public_url);
    domains::confirm(&state.db, &pb, &d, tracking.as_deref()).await.map_err(|e| AppError::BadRequest(e.to_string()))?;
    Ok(Json(load(&state, id, auth.org_id).await?))
}

async fn set_dns(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SendingDomain>> {
    let d = load(&state, id, auth.org_id).await?;
    if d.status != "registered" {
        return Err(AppError::BadRequest("Splee sets DNS once the domain is bought".into()));
    }
    let pb = porkbun(&state, auth.org_id).await?;
    domains::set_dns(&state.db, &pb, id, crate::tracking::cname_target(&state.cfg.public_url).as_deref()).await?;
    Ok(Json(load(&state, id, auth.org_id).await?))
}

#[derive(Deserialize)]
struct RecordBody {
    #[serde(rename = "type")]
    kind: String,
    name: String,
    content: String,
}

async fn add_record(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<RecordBody>) -> AppResult<Json<SendingDomain>> {
    util_input::one_of("type", &body.kind, &["TXT", "CNAME"])?;
    let d = load(&state, id, auth.org_id).await?;
    if d.status != "registered" {
        return Err(AppError::BadRequest("Add records once the domain is bought".into()));
    }
    let pb = porkbun(&state, auth.org_id).await?;
    domains::add_record(&state.db, &pb, &d, &body.kind, &body.name, &body.content).await.map_err(AppError::BadRequest)?;
    Ok(Json(load(&state, id, auth.org_id).await?))
}

/// The mailbox DNS check, run on the domain itself so it can be checked before any mailbox is connected.
async fn check_dns(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<SendingDomain>> {
    let d = load(&state, id, auth.org_id).await?;
    let selector = domains::dkim_selector(&d);
    let dns = mail::check_dns(&d.domain, selector.as_deref()).await.map_err(|e| AppError::BadRequest(format!("DNS check failed: {e:#}")))?;
    sqlx::query("update sending_domains set dns = $2 where id = $1").bind(id).bind(dns).execute(&state.db).await?;
    Ok(Json(load(&state, id, auth.org_id).await?))
}

/// Only Splee forgets the domain. It stays in the Porkbun account.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    auth.require_admin()?;
    let d = load(&state, id, auth.org_id).await?;
    if matches!(d.status.as_str(), "queued" | "registering") {
        return Err(AppError::Conflict("Wait until Splee has finished buying it".into()));
    }
    // Removing it would let it be bought again while Porkbun may already have bought it.
    if d.status == "uncertain" {
        return Err(AppError::Conflict("Check Porkbun first. It may have bought it".into()));
    }
    sqlx::query("delete from sending_domains where id = $1").bind(id).execute(&state.db).await?;
    Ok(Json(json!({ "ok": true })))
}
