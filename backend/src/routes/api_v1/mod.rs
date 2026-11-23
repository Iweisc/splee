mod campaigns;
mod directory;
mod emails;
mod leads;
mod lists;
mod projects;
mod runs;
mod sales;

use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, Request, State};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::{Extension, Router};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::attachments;
use super::util_idempotency;
use crate::auth::{self, KeyAuth};
use crate::company_filters::{Claude, Model};
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::ownership;
use crate::state::AppState;

pub(super) use campaigns::*;
pub(super) use directory::*;
pub(super) use emails::*;
pub(super) use leads::*;
pub(super) use lists::*;
pub(super) use projects::*;
pub(super) use runs::*;
pub(super) use sales::*;

pub fn router(state: &AppState) -> Router<AppState> {
    router_with(state, Arc::new(Claude))
}

/// `model` reads POST /v1/companies/filters' descriptions. Tests pass a fake.
pub fn router_with(state: &AppState, model: Arc<dyn Model>) -> Router<AppState> {
    Router::new()
        .route("/v1/projects", get(list_projects).post(create_project))
        .route("/v1/projects/{id}", get(get_project).patch(update_project))
        .route("/v1/projects/{id}/campaigns", get(list_campaigns).post(create_campaign))
        .route("/v1/campaigns/{id}", get(get_campaign).patch(update_campaign))
        .route("/v1/campaigns/import", post(import_campaign).layer(DefaultBodyLimit::max(IMPORT_BODY_LIMIT)))
        .route("/v1/campaigns/import/{id}", get(campaign_import))
        .route("/v1/campaigns/import/{id}/rows", get(campaign_import_rows))
        .route("/v1/campaigns/{id}/start", post(start_campaign))
        .route("/v1/campaigns/{id}/send-state", get(campaign_send_state))
        .route("/v1/campaigns/{id}/stop", post(stop_campaign))
        .route("/v1/campaigns/{id}/approve", post(approve_campaign))
        .route("/v1/campaigns/{id}/archive", post(archive_campaign))
        .route("/v1/campaigns/{id}/unarchive", post(unarchive_campaign))
        .route("/v1/campaigns/{id}/duplicate", post(duplicate_campaign))
        .route("/v1/campaigns/{id}/my-copy", get(campaign_my_copy).patch(update_campaign_my_copy))
        .route("/v1/campaigns/{id}/my-copy/source", put(set_my_copy_source))
        .route("/v1/campaigns/{id}/my-copy/preview", get(preview_my_copy))
        .route("/v1/projects/{id}/my-copy", get(project_my_copy).patch(update_project_my_copy))
        .route("/v1/hot-leads", get(hot_leads))
        .route("/v1/projects/{id}/inbox/changes", get(inbox_changes))
        .route("/v1/projects/{id}/leads", get(list_leads).post(add_leads))
        .route("/v1/projects/{id}/leads/upsert", post(upsert_lead))
        .route("/v1/projects/{id}/companies", get(list_companies))
        .route("/v1/projects/{id}/enrich/email", post(find_email))
        .route("/v1/projects/{id}/enrich/email/batch", post(find_emails))
        .route("/v1/enrich/email/batch/{id}", get(email_batch))
        .route("/v1/projects/{id}/enrich/phone", post(find_phone))
        .route("/v1/leads", get(find_leads))
        .route("/v1/leads/{id}", get(get_lead).patch(update_lead))
        .route("/v1/leads/{id}/assign", post(assign_lead))
        .route("/v1/leads/{id}/thread", get(lead_thread))
        .route("/v1/leads/{id}/reply", post(reply_to_lead))
        .route("/v1/erased-leads", get(erased_leads))
        .route("/v1/leads/{id}/mark-replied", post(mark_replied))
        .route("/v1/attachments", post(upload_attachment).layer(DefaultBodyLimit::max(attachments::UPLOAD_BODY_LIMIT)))
        .route("/v1/attachments/{id}", get(download_attachment))
        .route("/v1/projects/{id}/replies", get(replies))
        .route("/v1/projects/{id}/sent-emails", get(sent_emails))
        .route("/v1/projects/{id}/email-events", get(email_events))
        .route("/v1/projects/{id}/stats", get(project_stats))
        .route("/v1/projects/{id}/analytics", get(project_analytics))
        .route("/v1/projects/{id}/monthly-cap", get(project_monthly_cap))
        .route("/v1/suppressions", get(list_suppressions).post(add_suppressions))
        .route("/v1/suppression-lists", get(list_suppression_lists))
        .route("/v1/suppression-lists/people", post(add_people_to_list))
        .route("/v1/suppression-lists/companies", post(add_companies_to_list))
        .route("/v1/suppression-lists/{id}", patch(rescope_suppression_list).delete(delete_suppression_list))
        .route("/v1/suppression-lists/{id}/entries", get(suppression_list_entries))
        .route("/v1/dedup/companies", get(list_dedup_lists).post(add_to_dedup_list))
        .route("/v1/dedup/companies/{id}", get(get_dedup_list).patch(rescope_dedup_list).delete(delete_dedup_list))
        .route("/v1/companies", get(search_companies))
        .route("/v1/companies/export", get(export_companies))
        .route("/v1/companies/filters", post(companies_filters))
        .route("/v1/companies/by-domains", post(companies_by_domains))
        .route("/v1/sending-domains", get(list_sending_domains))
        .route("/v1/sending-domains/suggestions", get(suggest_sending_domains))
        .route("/v1/sending-domains/availability", get(check_sending_domains))
        .route("/v1/segments/{id}/companies", post(add_companies))
        .route("/v1/projects/{id}/saved-searches", get(list_saved_searches).post(save_search))
        .route("/v1/saved-searches/{id}", delete(delete_saved_search))
        .route("/v1/saved-searches/{id}/new", get(new_matches))
        .route("/v1/projects/{id}/lookalikes", get(list_lookalikes).post(add_lookalikes))
        .route("/v1/segments/{id}/find-people", post(find_people))
        .route("/v1/local-businesses", get(search_local_businesses))
        .route("/v1/segments/{id}/local-businesses", post(add_local_businesses))
        .route("/v1/segments/{id}/write-emails", post(write_emails))
        .route("/v1/runs/{id}", get(get_run))
        .route("/v1/tasks", get(list_tasks))
        .route("/v1/emails", get(list_emails))
        .route("/v1/emails/{id}/approve", post(approve_email))
        .route("/v1/emails/{id}/skip", post(skip_email))
        .route("/v1/inbox", get(list_inbox))
        .route("/v1/deals", get(list_deals).post(create_deal))
        .route("/v1/deals/{id}/move", post(move_deal))
        .route("/v1/deal-stages", get(list_stages))
        .route("/v1/projects/{id}/calls", get(list_calls))
        .route("/v1/leads/{id}/calls", post(log_call))
        .route("/v1/leads/{id}/meetings", get(list_meetings))
        .route("/v1/projects/{id}/sheets", get(list_sheets))
        .route("/v1/projects/{id}/sheets/{conn}/import", post(import_sheet))
        .route("/v1/projects/{id}/job-changes", get(list_job_changes))
        .route("/v1/projects/{id}/signals", get(list_signals))
        .route("/v1/projects/{id}/research", get(list_research))
        .route("/v1/projects/{id}/research/questions", post(add_research_question))
        .route("/v1/projects/{id}/research/run", post(start_research))
        .route("/v1/projects/{id}/watch-list", post(add_to_watch_list))
        .route("/v1/exports", get(list_exports))
        .route_layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(Extension(model))
}

/// Every /v1 request counts against its organization's rate limit, and a write with an Idempotency-Key runs at most once.
/// A request without a live key goes through untouched, for its handler to answer 401.
async fn guard(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let found = match auth::bearer(req.headers()) {
        Some(key) => KeyAuth::find(&state.db, key).await,
        None => Ok(None),
    };
    let key = match found {
        Ok(Some(key)) => key,
        Ok(None) => return next.run(req).await,
        Err(e) => return AppError::from(e).into_response(),
    };
    let permit = match state.rate_limits.acquire(key.org_id) {
        Ok(permit) => permit,
        Err(refused) => return refused.into_response(),
    };
    let org_id = key.org_id;
    req.extensions_mut().insert(key);
    let mut res = util_idempotency::run(&state.db, org_id, req, next).await;
    permit.quota.stamp(res.headers_mut());
    res
}

/// Room for campaign_imports::MAX_LEADS leads with long fields.
const IMPORT_BODY_LIMIT: usize = 20 * 1024 * 1024;

pub(super) async fn check_project(db: &PgPool, id: Uuid, org_id: Uuid) -> AppResult<()> {
    sqlx::query("select 1 from projects where id = $1 and org_id = $2")
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)?;
    Ok(())
}

/// A PATCH body. A field the route doesn't take is a 422, so a typo never reads as a change that happened.
fn strict<T: DeserializeOwned>(body: Value) -> AppResult<T> {
    serde_json::from_value(body).map_err(|e| AppError::Unprocessable(format!("Invalid body: {e}")))
}

/// An owner filter may name the member by email here, since a CRM or calendar knows people by their address.
async fn owner_by_email(db: &PgPool, org_id: Uuid, owner: Option<String>) -> AppResult<Option<String>> {
    match owner {
        Some(email) if email.contains('@') => Ok(ownership::resolve_owner(db, org_id, &json!(email)).await?.map(|id| id.to_string())),
        other => Ok(other),
    }
}

#[derive(Deserialize)]
pub(super) struct SinceQuery {
    since: Option<Ts>,
}

#[cfg(test)]
mod tests;
