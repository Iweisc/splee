use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::state::AppState;

pub mod auth;
mod bookings;
mod directory;
mod endpoints;
mod extension;
mod forms;
mod hiring;
mod tech;
pub mod linkedin;
mod calendar;
mod saved_replies;
mod slack;
mod sending_domains;
mod social;
pub mod org;
pub mod projects;
pub mod competitors;
pub mod segments;
pub mod leads;
pub mod emails;
pub mod inbox;
pub mod mailboxes;
mod mailbox_oauth;
mod tumblr;
pub mod settings;
pub mod materials;
pub mod imports;
pub mod suppressions;
pub mod notifications;
pub mod api_keys;
pub mod api_v1;
mod mcp;
pub mod oauth;
pub mod stats;
pub mod demo;
pub mod runs;
pub mod support;
pub mod explorer;
pub mod unsubscribe;
mod variants;
mod util_airtable;
mod util_baserow;
mod util_grist;
mod util_teable;
mod util_nocodb;
mod util_csv;
pub mod util_input;
mod util_lead_import;
mod util_notion;
mod util_pagination;
mod util_scope;
mod util_sheet;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(org::router())
        .merge(projects::router())
        .merge(competitors::router())
        .merge(segments::router())
        .merge(leads::router())
        .merge(hiring::router())
        .merge(tech::router())
        .merge(emails::router())
        .merge(inbox::router())
        .merge(saved_replies::router())
        .merge(endpoints::router())
        .merge(linkedin::router())
        .merge(social::router())
        .merge(mailboxes::router())
        .merge(mailbox_oauth::router())
        .merge(tumblr::router())
        .merge(sending_domains::router())
        .merge(settings::router())
        .merge(materials::router())
        .merge(imports::router())
        .merge(suppressions::router())
        .merge(notifications::router())
        .merge(api_keys::router())
        .merge(api_v1::router())
        .merge(extension::router())
        .merge(mcp::router())
        .merge(oauth::router())
        .merge(stats::router())
        .merge(demo::router())
        .merge(runs::router())
        .merge(support::router())
        .merge(explorer::router())
        .merge(unsubscribe::router())
        .merge(variants::router())
        .merge(bookings::router())
        .merge(forms::router())
        .merge(calendar::router())
        .merge(directory::router())
        .merge(slack::router())
        .route("/health", get(health))
}

/// For uptime monitors and container health checks. Through the frontend's /api proxy it covers the frontend, the API and Postgres.
async fn health(State(state): State<AppState>) -> Result<Json<Value>, StatusCode> {
    // A database that stops answering would otherwise hold this for the pool's 30 s acquire timeout, past most monitors' own.
    let ping = tokio::time::timeout(std::time::Duration::from_secs(5), sqlx::query("select 1").execute(&state.db)).await;
    if !matches!(ping, Ok(Ok(_))) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    Ok(Json(json!({ "ok": true })))
}

#[cfg(test)]
mod tests {
    // axum panics on overlapping routes while building the router, which used to crash the API on boot.
    #[test]
    fn router_builds() {
        let _ = super::router();
    }
}
