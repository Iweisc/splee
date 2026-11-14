//! Referrals: the public page a customer forwards (/r/{token}, which the frontend renders) with its view and sign-up, the
//! Pipeline's Referrals tab, the section in a won deal's window, writing the ask now and dropping it. See agent::referrals.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::demo::client_ip;
use crate::agent::referrals::{self, Public, Referral, SELECT, SignUp};
use crate::auth::Auth;
use crate::deals;
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::state::AppState;
use crate::visitors;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/referral-pages/{token}", get(page))
        .route("/referral-pages/{token}/view", post(view))
        .route("/referral-pages/{token}/signup", post(sign_up))
        .route("/projects/{id}/referrals", get(list))
        .route("/deals/{id}/referral", get(for_deal).post(write_now))
        .route("/referrals/{id}", patch(update))
}

/// Views a minute from one address that get counted, and sign-ups.
const VIEWS_PER_IP: u32 = 30;
const SIGNUPS_PER_IP: u32 = 5;

fn ip_key(state: &AppState, headers: &HeaderMap, peer: SocketAddr, what: &str) -> String {
    let ip = client_ip(headers, peer).parse().ok();
    format!("referral-{what}:{}", ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default())
}

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    referrals::public(&state.db, &state.cfg.public_url, &token).await?.map(Json).ok_or(AppError::NotFound)
}

async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> AppResult<StatusCode> {
    if visitors::allow(&ip_key(&state, &headers, peer, "view"), VIEWS_PER_IP) {
        referrals::viewed(&state.db, &token).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn sign_up(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(form): Json<SignUp>,
) -> AppResult<StatusCode> {
    let email = form.email.trim();
    let valid = email.len() <= 254 && email.split_once('@').is_some_and(|(user, domain)| !user.is_empty() && domain.contains('.') && !domain.contains('@'));
    if form.name.trim().is_empty() || form.name.len() > 120 || !valid || form.company.len() > 120 || form.note.len() > 2000 {
        return Err(AppError::BadRequest("Enter your name and a work email".into()));
    }
    if !visitors::allow(&ip_key(&state, &headers, peer, "signup"), SIGNUPS_PER_IP) {
        return Err(AppError::TooManyRequests("Too many tries. Wait a minute and try again".into()));
    }
    referrals::sign_up(&state.db, &state.cfg.public_url, &token, &form).await?.ok_or(AppError::NotFound)?;
    Ok(StatusCode::NO_CONTENT)
}

/// A lead who came in through a referral page.
#[derive(Serialize, sqlx::FromRow)]
struct Referred {
    lead_id: Uuid,
    full_name: String,
    company_name: String,
    status: String,
    referral_id: Uuid,
    created_at: Ts,
}

#[derive(Serialize)]
struct View {
    enabled: bool,
    items: Vec<Referral>,
    referred: Vec<Referred>,
}

/// The project's referral asks on deals still won, newest first, without dropped ones, and the leads they brought.
async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<View>> {
    auth.project(&state.db, id).await?;
    let enabled: bool = sqlx::query_scalar("select coalesce((select ask_for_referrals from project_settings where project_id = $1), true)")
        .bind(id)
        .fetch_one(&state.db)
        .await?;
    let items = sqlx::query_as::<_, Referral>(&format!(
        "{SELECT} where f.project_id = $1 and f.dismissed_at is null and d.status = 'won' order by f.created_at desc limit 300"
    ))
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    let referred = sqlx::query_as::<_, Referred>(
        "select l.id as lead_id, l.full_name, l.company_name, l.status, l.referral_id, l.created_at from leads l
         join deal_referrals f on f.id = l.referral_id where f.project_id = $1 order by l.created_at desc limit 300",
    )
    .bind(id)
    .fetch_all(&state.db)
    .await?;
    Ok(Json(View { enabled, items, referred }))
}

async fn current(state: &AppState, deal_id: Uuid) -> AppResult<Option<Referral>> {
    Ok(sqlx::query_as::<_, Referral>(&format!("{SELECT} where f.deal_id = $1 and f.dismissed_at is null"))
        .bind(deal_id)
        .fetch_optional(&state.db)
        .await?)
}

/// The won deal's referral ask, or None before one or once it's dropped.
async fn for_deal(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Referral>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    Ok(Json(current(&state, deal.id).await?))
}

/// Has the agent write the ask now.
async fn write_now(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<Referral>>> {
    let deal = deals::get(&state.db, auth.org_id, id).await?;
    if deal.status != "won" {
        return Err(AppError::BadRequest("Only a customer can refer someone".into()));
    }
    let run = referrals::request(&state.db, deal.id, deal.project_id, auth.org_id).await?;
    crate::agent::start_now(&state, run).await?;
    Ok(Json(current(&state, deal.id).await?))
}

#[derive(Deserialize)]
struct Update {
    dismissed: bool,
}

/// Drops the ask from the lists, or brings it back. Its page keeps working.
async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<Update>) -> AppResult<Json<Option<Referral>>> {
    let deal_id: Uuid = sqlx::query_scalar(
        "update deal_referrals f set dismissed_at = case when $3 then coalesce(f.dismissed_at, now()) end
         from projects p where p.id = f.project_id and f.id = $1 and p.org_id = $2 returning f.deal_id",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(body.dismissed)
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    Ok(Json(current(&state, deal_id).await?))
}
