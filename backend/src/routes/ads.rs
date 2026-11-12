//! Connecting LinkedIn, Google Ads and Meta for ad audiences (ads/), picking the ad account, and turning the hot leads'
//! audience and each campaign's on and off. Signing in works like the CRM sign-ins (routes/crm_oauth.rs): start returns the
//! platform's sign-in URL, and the one callback for all three saves the connection and redirects to the integrations page with
//! how it went. Every answer is the project's ad audiences as the integrations page and the campaign page show them.

use axum::extract::{Path, Query, State};
use axum::response::Redirect;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{Duration, Utc};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_scope;
use crate::ads::{self, Connection, Platform};
use crate::auth::{Auth, MaybeAuth};
use crate::config::Config;
use crate::crypto::{self, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::mail::oauth;
use crate::models::{Segment, Ts};
use crate::state::AppState;

const STATE_MINUTES: i64 = 10;
pub(crate) const UNKNOWN_STATE: &str = "This sign-in expired or was already used. Connect the ad platform again.";

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/ads/oauth/callback", get(callback))
        .route("/projects/{id}/ads", get(project_status))
        .route("/projects/{id}/ads/{platform}", axum::routing::delete(disconnect))
        .route("/projects/{id}/ads/{platform}/oauth", post(start))
        .route("/projects/{id}/ads/{platform}/account", put(pick_account))
        .route("/projects/{id}/ads/{platform}/hot", put(toggle_hot))
        .route("/segments/{id}/audiences", get(campaign_status))
        .route("/segments/{id}/audiences/{platform}", put(toggle_campaign))
}

/// A connection as the pages show it. The tokens never leave the server.
#[derive(Debug, Serialize, FromRow)]
pub struct ConnectionView {
    pub platform: String,
    pub ad_accounts: Value,
    pub ad_account_id: String,
    pub ad_account_name: String,
    pub needs_reconnect: bool,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    /// Only while it's still ahead.
    pub held_until: Option<Ts>,
}

#[derive(Debug, Serialize, FromRow)]
pub struct AudienceView {
    pub platform: String,
    pub segment_id: Option<Uuid>,
    pub name: String,
    /// As the platform last reported it.
    pub size: Option<i64>,
    /// The leads Splee has sent it.
    pub uploaded: i64,
    pub synced_at: Option<Ts>,
    pub error: Option<String>,
}

/// The project's connections, and the hot leads' audiences or, with `segment`, the campaign's.
#[derive(Debug, Serialize)]
pub struct AdsStatus {
    /// The platforms this Splee can connect.
    pub available: Vec<&'static str>,
    pub connections: Vec<ConnectionView>,
    pub audiences: Vec<AudienceView>,
}

async fn status(db: &PgPool, cfg: &Config, project_id: Uuid, segment: Option<Uuid>) -> AppResult<Json<AdsStatus>> {
    let connections = sqlx::query_as(
        "select platform, ad_accounts, ad_account_id, ad_account_name, needs_reconnect, error, failed_at,
           case when held_until > now() then held_until end as held_until
         from ad_connections where project_id = $1 order by array_position(array['linkedin', 'google', 'meta'], platform)",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    let audiences = sqlx::query_as(
        "select c.platform, a.segment_id, a.name, a.size, (select count(*) from ad_audience_members m where m.audience_id = a.id and m.state <> 'removing') as uploaded, a.synced_at, a.error
         from ad_audiences a join ad_connections c on c.id = a.connection_id
         where a.project_id = $1 and (($2::uuid is null and a.kind = 'hot') or a.segment_id = $2)
         order by array_position(array['linkedin', 'google', 'meta'], c.platform)",
    )
    .bind(project_id)
    .bind(segment)
    .fetch_all(db)
    .await?;
    Ok(Json(AdsStatus { available: ads::available(cfg), connections, audiences }))
}

fn platform(name: &str) -> AppResult<Platform> {
    Platform::parse(name).ok_or(AppError::NotFound)
}

/// The project's connection to the platform, which has to be signed in to change its audiences.
async fn connection(db: &PgPool, project_id: Uuid, platform: Platform) -> AppResult<Connection> {
    let label = platform.label();
    let conn = Connection::find(db, project_id, platform).await?.ok_or_else(|| AppError::BadRequest(format!("Connect {label} on the integrations page first.")))?;
    if conn.needs_reconnect {
        return Err(AppError::BadRequest(format!("{label} signed Splee out. Reconnect it on the integrations page.")));
    }
    Ok(conn)
}

async fn project_status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<AdsStatus>> {
    auth.project(&state.db, id).await?;
    status(&state.db, &state.cfg, id, None).await
}

async fn start(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<Value>> {
    let platform = platform(&name)?;
    auth.project(&state.db, id).await?;
    let (nonce, verifier) = (random_token(), random_token());
    let signed = oauth::sign_state(&state.cfg.app_secret, oauth::ADS, &nonce, auth.user_id, auth.org_id);
    let url = ads::authorize_url(&state.cfg, platform, &signed, &verifier).ok_or_else(|| AppError::BadRequest(ads::not_set_up(platform)))?;
    sqlx::query("delete from ad_oauth_states where expires_at < now()").execute(&state.db).await?;
    sqlx::query("insert into ad_oauth_states (nonce_hash, user_id, org_id, project_id, platform, code_verifier_enc, expires_at) values ($1, $2, $3, $4, $5, $6, $7)")
        .bind(sha256_hex(&nonce))
        .bind(auth.user_id)
        .bind(auth.org_id)
        .bind(id)
        .bind(platform.name())
        .bind(crypto::encrypt(&state.cfg.app_secret, &verifier))
        .bind(Utc::now() + Duration::minutes(STATE_MINUTES))
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Deserialize)]
struct CallbackQuery {
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
    /// Meta's, next to its error.
    error_reason: Option<String>,
    error_description: Option<String>,
}

/// Always redirects: to the project's integrations page with `ads` set to the platform it connected, or with `ads_error`, or to
/// the app when the state doesn't say which project.
async fn callback(State(state): State<AppState>, MaybeAuth(auth): MaybeAuth, Query(q): Query<CallbackQuery>) -> Redirect {
    let (page, params) = match finish(&state, auth, q).await {
        Ok((project, platform)) => (format!("/app/p/{project}/integrations"), vec![("ads", platform.name().to_string())]),
        Err((Some(project), error)) => (format!("/app/p/{project}/integrations"), vec![("ads_error", error)]),
        Err((None, error)) => ("/app".to_string(), vec![("ads_error", error)]),
    };
    let url = Url::parse_with_params("http://splee/", &params).expect("a fixed URL");
    Redirect::to(&format!("{page}?{}", url.query().unwrap_or_default()))
}

#[derive(FromRow)]
struct Pending {
    user_id: Uuid,
    org_id: Uuid,
    project_id: Uuid,
    platform: String,
    code_verifier_enc: String,
    expires_at: Ts,
}

/// The project and platform it connected, or what went wrong as the page shows it, with the project when the state named one.
async fn finish(state: &AppState, auth: Option<Auth>, q: CallbackQuery) -> Result<(Uuid, Platform), (Option<Uuid>, String)> {
    let internal = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "ad platform sign-in failed");
        "Something went wrong saving the sign-in. Try again.".to_string()
    };
    let signed = q.state.as_deref().unwrap_or_default();
    let nonce = oauth::state_nonce(signed).ok_or((None, UNKNOWN_STATE.to_string()))?;
    // Deleted before anything else is checked, so a state works once whatever happens.
    let pending = sqlx::query_as::<_, Pending>(
        "delete from ad_oauth_states where nonce_hash = $1 returning user_id, org_id, project_id, platform, code_verifier_enc, expires_at",
    )
    .bind(sha256_hex(nonce))
    .fetch_optional(&state.db)
    .await
    .map_err(|e| (None, internal(&e)))?
    .ok_or((None, UNKNOWN_STATE.to_string()))?;
    let project = pending.project_id;
    let fail = |e: String| (Some(project), e);
    let platform = Platform::parse(&pending.platform).ok_or_else(|| fail(UNKNOWN_STATE.into()))?;
    let label = platform.label();
    let again = || fail(format!("This sign-in expired or was already used. Connect {label} again."));
    if !oauth::verify_state(&state.cfg.app_secret, oauth::ADS, signed, pending.user_id, pending.org_id) || pending.expires_at <= Utc::now() {
        return Err(again());
    }
    if let Some(error) = q.error {
        let denied = matches!(error.as_str(), "access_denied" | "user_cancelled_authorize" | "user_cancelled_login") || q.error_reason.as_deref() == Some("user_denied");
        return Err(fail(match denied {
            true => format!("You didn't allow Splee into {label}, so nothing was connected."),
            false => format!("{label} couldn't sign you in: {}", q.error_description.unwrap_or(error)),
        }));
    }
    let auth = auth.ok_or_else(|| fail(format!("Sign in to Splee, then connect {label} again.")))?;
    if (auth.user_id, auth.org_id) != (pending.user_id, pending.org_id) {
        return Err(fail(format!("You switched organizations while signing in. Connect {label} again from this one.")));
    }
    // Checked again, since access to the project could have gone in the meantime.
    auth.project(&state.db, project).await.map_err(|e| (None, e.public_message()))?;
    let code = q.code.filter(|c| !c.is_empty()).ok_or_else(again)?;
    let verifier = crypto::decrypt(&state.cfg.app_secret, &pending.code_verifier_enc).map_err(|e| fail(internal(&e)))?;
    ads::connect(state, pending.org_id, project, platform, &code, &verifier).await.map_err(fail)?;
    Ok((project, platform))
}

#[derive(Deserialize)]
struct AccountBody {
    account_id: String,
}

/// Picks one of the ad accounts the sign-in listed. Audiences in another account move: they're deleted there and made again
/// in this one.
async fn pick_account(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<AccountBody>,
) -> AppResult<Json<AdsStatus>> {
    let platform = platform(&name)?;
    auth.project(&state.db, id).await?;
    let conn = connection(&state.db, id, platform).await?;
    let account_name: Option<String> = sqlx::query_scalar(
        "select a->>'name' from ad_connections, jsonb_array_elements(ad_accounts) a where id = $1 and a->>'id' = $2",
    )
    .bind(conn.id)
    .bind(body.account_id.trim())
    .fetch_optional(&state.db)
    .await?;
    let account_name = account_name.ok_or_else(|| AppError::BadRequest(format!("That isn't one of your {} ad accounts. Reconnect to read them again.", platform.label())))?;
    ads::pick_account(&state, id, platform, body.account_id.trim(), &account_name).await?;
    status(&state.db, &state.cfg, id, None).await
}

#[derive(Deserialize)]
struct ToggleBody {
    on: bool,
}

/// Makes the audience on the platform, or deletes it there and forgets it.
async fn toggle(state: &AppState, conn: &Connection, segment: Option<&Segment>, on: bool) -> AppResult<()> {
    if on {
        let segment = segment.map(|s| (s.id, s.name.as_str()));
        return ads::enable(state, conn, segment).await.map_err(|e| AppError::BadRequest(e.message(conn.platform())));
    }
    let audience: Option<Uuid> = sqlx::query_scalar("select id from ad_audiences where connection_id = $1 and segment_id is not distinct from $2")
        .bind(conn.id)
        .bind(segment.map(|s| s.id))
        .fetch_optional(&state.db)
        .await?;
    if let Some(audience) = audience {
        ads::disable(state, audience).await?;
    }
    Ok(())
}

async fn toggle_hot(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<ToggleBody>,
) -> AppResult<Json<AdsStatus>> {
    let platform = platform(&name)?;
    auth.project(&state.db, id).await?;
    let conn = connection(&state.db, id, platform).await?;
    toggle(&state, &conn, None, body.on).await?;
    status(&state.db, &state.cfg, id, None).await
}

/// Forgets the sign-in and the project's audiences, which are deleted on the platform where Splee still can.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<AdsStatus>> {
    let platform = platform(&name)?;
    auth.project(&state.db, id).await?;
    if let Some(conn) = Connection::find(&state.db, id, platform).await? {
        ads::disconnect(&state, &conn).await?;
    }
    status(&state.db, &state.cfg, id, None).await
}

async fn campaign_status(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<AdsStatus>> {
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    status(&state.db, &state.cfg, segment.project_id, Some(segment.id)).await
}

async fn toggle_campaign(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<ToggleBody>,
) -> AppResult<Json<AdsStatus>> {
    let platform = platform(&name)?;
    let segment: Segment = util_scope::load(&state.db, "segments", id, auth.org_id).await?;
    let conn = connection(&state.db, segment.project_id, platform).await?;
    toggle(&state, &conn, Some(&segment), body.on).await?;
    status(&state.db, &state.cfg, segment.project_id, Some(segment.id)).await
}
