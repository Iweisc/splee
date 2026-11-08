use axum::Router;

use crate::state::AppState;

pub mod auth;
pub mod org;
pub mod billing;
pub mod projects;
pub mod competitors;
pub mod segments;
pub mod leads;
pub mod emails;
pub mod inbox;
pub mod mailboxes;
pub mod settings;
pub mod materials;
pub mod imports;
pub mod suppressions;
pub mod notifications;
pub mod api_keys;
pub mod referral;
pub mod api_v1;
pub mod stats;
pub mod demo;
pub mod runs;
pub mod support;
pub mod explorer;
pub mod unsubscribe;
pub mod util_input;
mod util_lead_import;
mod util_pagination;
mod util_scope;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(auth::router())
        .merge(org::router())
        .merge(billing::router())
        .merge(projects::router())
        .merge(competitors::router())
        .merge(segments::router())
        .merge(leads::router())
        .merge(emails::router())
        .merge(inbox::router())
        .merge(mailboxes::router())
        .merge(settings::router())
        .merge(materials::router())
        .merge(imports::router())
        .merge(suppressions::router())
        .merge(notifications::router())
        .merge(api_keys::router())
        .merge(referral::router())
        .merge(api_v1::router())
        .merge(stats::router())
        .merge(demo::router())
        .merge(runs::router())
        .merge(support::router())
        .merge(explorer::router())
        .merge(unsubscribe::router())
}

#[cfg(test)]
mod tests {
    // axum panics on overlapping routes while building the router, which used to crash the API on boot.
    #[test]
    fn router_builds() {
        let _ = super::router();
    }
}
