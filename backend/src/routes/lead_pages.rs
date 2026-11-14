//! Lead pages' routes: the public page at /for/{token}, which the frontend renders, the view its script reports, and the
//! lead's page for the app. See crate::lead_pages.

use std::net::SocketAddr;

use axum::extract::{ConnectInfo, Path, State};
use axum::http::header::USER_AGENT;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::Utc;

use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::error::{AppError, AppResult};
use crate::lead_pages::{self, ForLead, Public, View};
use crate::models::Lead;
use crate::state::AppState;
use crate::visitors::{self, Kind};

pub fn router() -> Router<AppState> {
    Router::new().route("/pages/{token}", get(page)).route("/pages/{token}/view", post(view)).route("/leads/{id}/page", get(for_lead))
}

/// Views a minute from one address that get stored. Past it the page still says 204, so a person never sees an error.
const PER_IP: u32 = 30;

async fn page(State(state): State<AppState>, Path(token): Path<String>) -> AppResult<Json<Public>> {
    lead_pages::public(&state.db, &state.cfg.public_url, &token).await?.map(Json).ok_or(AppError::NotFound)
}

async fn view(State(state): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, headers: HeaderMap, Path(token): Path<String>) -> StatusCode {
    let ip = client_ip(&headers, peer).parse().ok();
    let view = View {
        user_agent: headers.get(USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string(),
        ip,
        ip_hash: ip.map(|ip| visitors::ip_hash(&state.cfg.app_secret, ip)).unwrap_or_default(),
        at: Utc::now(),
    };
    if !visitors::allow(&format!("page:{}", view.ip_hash), PER_IP) {
        return StatusCode::NO_CONTENT;
    }
    // In the background, so the page never waits on the network lookup.
    tokio::spawn(async move {
        let result = async {
            let network = match view.ip {
                Some(ip) => visitors::network_kind(&state, ip).await.unwrap_or(Kind::Unknown),
                None => Kind::Unknown,
            };
            if let Some(first) = lead_pages::record(&state.db, &token, &view, network).await? {
                lead_pages::announce(&state.db, &state.cfg.public_url, &first).await?;
            }
            anyhow::Ok(())
        }
        .await;
        if let Err(e) = result {
            tracing::warn!(error = ?e, "couldn't record a lead page view");
        }
    });
    StatusCode::NO_CONTENT
}

async fn for_lead(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Option<ForLead>>> {
    let lead: Lead = super::util_scope::load(&state.db, "leads", id, auth.org_id).await?;
    Ok(Json(lead_pages::for_lead(&state.db, &state.cfg.public_url, lead.id).await?))
}
