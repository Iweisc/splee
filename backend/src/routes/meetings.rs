//! Connecting Fireflies, Fathom, Gong and Zoom (meetings/), their webhooks, a lead's meetings, and accepting or dismissing
//! the deal stage the agent suggested after one. The webhook URL has a per-connection token, and a delivery is only read
//! once its signature matches the connection's secret.

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use super::util_scope;
use crate::auth::Auth;
use crate::crypto;
use crate::deals::{self, Move, NewDeal};
use crate::error::{AppError, AppResult};
use crate::meetings::{self, CONNECTION_COLUMNS, Connection, Found, Provider, fathom, fireflies, gong, zoom};
use crate::models::{Lead, MEETING_COLUMNS, Meeting, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/projects/{id}/meeting-connections", get(list))
        .route("/projects/{id}/meeting-connections/{provider}", put(connect).delete(disconnect))
        .route("/projects/{id}/meeting-connections/{provider}/secret", put(save_secret))
        .route("/meeting-webhooks/{provider}/{token}", post(webhook))
        .route("/leads/{id}/meetings", get(lead_meetings))
        .route("/meetings/{id}/stage", post(decide_stage))
}

/// A connection as the integrations page shows it. Keys and secrets never leave the server.
#[derive(Debug, Serialize, FromRow)]
pub struct MeetingConnection {
    pub provider: String,
    /// The secret in the webhook URL, /api/meeting-webhooks/{provider}/{webhook_token}. Gong has no webhook.
    pub webhook_token: String,
    pub has_secret: bool,
    pub last_event_at: Option<Ts>,
    /// Gong's last read, and why it failed.
    pub synced_at: Option<Ts>,
    pub error: String,
    pub meetings: i64,
    pub created_at: Ts,
}

async fn connections(db: &PgPool, project_id: Uuid) -> AppResult<Json<Vec<MeetingConnection>>> {
    let list = sqlx::query_as(
        "select c.provider, c.webhook_token, c.signing_secret_enc <> '' as has_secret, c.last_event_at, c.synced_at, c.error,
           (select count(*) from meetings m where m.connection_id = c.id) as meetings, c.created_at
         from meeting_connections c where c.project_id = $1 order by c.created_at",
    )
    .bind(project_id)
    .fetch_all(db)
    .await?;
    Ok(Json(list))
}

fn provider(name: &str) -> AppResult<Provider> {
    Provider::parse(name).ok_or_else(|| AppError::BadRequest("Pick Fireflies, Fathom, Gong or Zoom".into()))
}

async fn list(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<MeetingConnection>>> {
    auth.project(&state.db, id).await?;
    connections(&state.db, id).await
}

#[derive(Deserialize, Default)]
struct ConnectBody {
    /// Fireflies and Fathom.
    api_key: Option<String>,
    /// Gong.
    access_key: Option<String>,
    access_secret: Option<String>,
    /// Zoom's Server-to-Server OAuth app, and its secret token for the webhook.
    account_id: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    secret_token: Option<String>,
}

fn required(value: &Option<String>, what: &str) -> AppResult<String> {
    value.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string).ok_or_else(|| AppError::BadRequest(format!("Paste the {what}")))
}

/// Saves the credentials once the provider accepts them. Connecting again replaces them and keeps the webhook URL. Zoom's
/// secret token comes with the rest, since Zoom checks the webhook URL as soon as it's saved there.
async fn connect(
    State(state): State<AppState>,
    auth: Auth,
    Path((id, name)): Path<(Uuid, String)>,
    Json(body): Json<ConnectBody>,
) -> AppResult<Json<Vec<MeetingConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    let fake = &state.cfg.meetings_api_url;
    let (credentials, secret) = match provider {
        Provider::Fireflies => {
            let key = required(&body.api_key, "Fireflies API key")?;
            fireflies::check(&fireflies::api(fake)?, &key).await?;
            (key, None)
        }
        Provider::Fathom => {
            let key = required(&body.api_key, "Fathom API key")?;
            fathom::check(&fathom::api(fake)?, &key).await?;
            (key, None)
        }
        Provider::Gong => {
            let (key, secret) = (required(&body.access_key, "Gong access key")?, required(&body.access_secret, "Gong access key secret")?);
            if key.contains(':') {
                return Err(AppError::BadRequest("Paste the access key and its secret into their own boxes".into()));
            }
            let credentials = format!("{key}:{secret}");
            gong::check(&gong::api(fake)?, &credentials).await?;
            (credentials, None)
        }
        Provider::Zoom => {
            let app = zoom::App {
                account_id: required(&body.account_id, "Zoom account ID")?,
                client_id: required(&body.client_id, "Zoom client ID")?,
                client_secret: required(&body.client_secret, "Zoom client secret")?,
            };
            let token = required(&body.secret_token, "Zoom secret token")?;
            zoom::client(fake)?.token(&app).await?;
            (serde_json::to_string(&app).map_err(|e| AppError::Internal(e.into()))?, Some(token))
        }
    };
    let encrypt = |v: &str| crypto::encrypt(&state.cfg.app_secret, v);
    sqlx::query(
        "insert into meeting_connections (org_id, project_id, provider, credentials_enc, signing_secret_enc) values ($1, $2, $3, $4, coalesce($5, ''))
         on conflict (project_id, provider) do update set credentials_enc = excluded.credentials_enc, error = '', synced_at = null,
           signing_secret_enc = coalesce($5, meeting_connections.signing_secret_enc)",
    )
    .bind(auth.org_id)
    .bind(id)
    .bind(provider.name())
    .bind(encrypt(&credentials))
    .bind(secret.as_deref().map(encrypt))
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
) -> AppResult<Json<Vec<MeetingConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    let secret = body.secret.trim();
    if provider == Provider::Gong {
        return Err(AppError::BadRequest("Gong has no webhook. Splee reads its calls every 15 minutes".into()));
    }
    if secret.is_empty() {
        return Err(AppError::BadRequest("Paste the webhook's secret".into()));
    }
    let saved = sqlx::query("update meeting_connections set signing_secret_enc = $3 where project_id = $1 and provider = $2")
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

/// The meetings already kept stay, with their notes.
async fn disconnect(State(state): State<AppState>, auth: Auth, Path((id, name)): Path<(Uuid, String)>) -> AppResult<Json<Vec<MeetingConnection>>> {
    auth.project(&state.db, id).await?;
    let provider = provider(&name)?;
    sqlx::query("delete from meeting_connections where project_id = $1 and provider = $2").bind(id).bind(provider.name()).execute(&state.db).await?;
    connections(&state.db, id).await
}

#[derive(FromRow)]
struct Hooked {
    #[sqlx(flatten)]
    connection: Connection,
    signing_secret_enc: String,
}

/// A provider's webhook. Zoom's URL check is answered once it's signed. A delivery seen before is answered without doing
/// anything, and one that fails is not marked seen, so the provider's retry gets another go.
async fn webhook(State(state): State<AppState>, Path((name, token)): Path<(String, String)>, headers: HeaderMap, body: Bytes) -> AppResult<Json<Value>> {
    let provider = Provider::parse(&name).filter(|p| *p != Provider::Gong).ok_or(AppError::NotFound)?;
    let hooked: Hooked = sqlx::query_as(&format!(
        "select {CONNECTION_COLUMNS}, signing_secret_enc from meeting_connections where webhook_token = $1 and provider = $2"
    ))
    .bind(&token)
    .bind(provider.name())
    .fetch_optional(&state.db)
    .await?
    .ok_or(AppError::NotFound)?;
    let secret = crypto::decrypt(&state.cfg.app_secret, &hooked.signing_secret_enc)?;
    if secret.is_empty() {
        return Err(AppError::BadRequest("Paste this webhook's secret on Splee's Integrations page first".into()));
    }
    if !provider.verify(&secret, &headers, &body, Utc::now().timestamp()) {
        return Err(AppError::Unauthorized);
    }
    let event: Value = serde_json::from_slice(&body).map_err(|_| AppError::BadRequest("The body isn't JSON".into()))?;
    if provider == Provider::Zoom && event["event"] == zoom::URL_VALIDATION {
        let plain = event["payload"]["plainToken"].as_str().unwrap_or_default();
        return Ok(Json(zoom::validation(&secret, plain)));
    }
    let event_id = provider.event_id(&headers, &event).ok_or_else(|| AppError::BadRequest("The event has no id".into()))?;
    let c = &hooked.connection;
    let seen: bool = sqlx::query_scalar("select exists (select 1 from meeting_events where connection_id = $1 and event_id = $2)")
        .bind(c.id)
        .bind(&event_id)
        .fetch_one(&state.db)
        .await?;
    if seen {
        return Ok(Json(json!({ "duplicate": true })));
    }
    let found = fetch(&state, provider, c, &event).await?;
    let recorded = match &found {
        Some(f) => meetings::record(&state, c, f).await?,
        None => meetings::Recorded::Unmatched,
    };
    sqlx::query("insert into meeting_events (connection_id, event_id) values ($1, $2) on conflict do nothing").bind(c.id).bind(&event_id).execute(&state.db).await?;
    sqlx::query("update meeting_connections set last_event_at = now() where id = $1").bind(c.id).execute(&state.db).await?;
    Ok(Json(json!({ "matched": !matches!(recorded, meetings::Recorded::Unmatched) })))
}

/// The meeting a webhook is about, read from the provider when the event only names it. Events of other kinds have none.
async fn fetch(state: &AppState, provider: Provider, c: &Connection, event: &Value) -> AppResult<Option<Found>> {
    let fake = &state.cfg.meetings_api_url;
    let credentials = c.credentials(state)?;
    match provider {
        Provider::Fireflies => match fireflies::meeting_id(event).filter(|_| fireflies::transcribed(event)) {
            Some(id) => fireflies::meeting(&fireflies::api(fake)?, &credentials, &id).await,
            None => Ok(None),
        },
        Provider::Fathom => fathom::meeting(&fathom::api(fake)?, &credentials, event).await,
        Provider::Zoom if event["event"] == zoom::TRANSCRIPT_COMPLETED => {
            let app: zoom::App = serde_json::from_str(&credentials).map_err(|e| AppError::Internal(e.into()))?;
            zoom::client(fake)?.meeting(&app, event).await
        }
        Provider::Zoom | Provider::Gong => Ok(None),
    }
}

#[derive(Debug, Serialize, FromRow)]
pub struct MeetingRow {
    #[serde(flatten)]
    #[sqlx(flatten)]
    pub meeting: Meeting,
    /// The suggested stage's name.
    pub suggested_stage: Option<String>,
    /// The drafted follow-up's status, like draft until someone approves it.
    pub follow_up_status: Option<String>,
    pub transcript_chars: i32,
}

const ROW_SELECT: &str = "s.name as suggested_stage, e.status as follow_up_status, length(m.transcript) as transcript_chars
     from meetings m left join deal_stages s on s.id = m.suggested_stage_id left join email_messages e on e.id = m.follow_up_email_id";

/// A lead's meetings, oldest first like the rest of their timeline.
pub async fn for_lead(db: &PgPool, lead_id: Uuid) -> sqlx::Result<Vec<MeetingRow>> {
    sqlx::query_as(&format!("select {MEETING_COLUMNS}, {ROW_SELECT} where m.lead_id = $1 order by m.started_at")).bind(lead_id).fetch_all(db).await
}

async fn row(db: &PgPool, org_id: Uuid, id: Uuid) -> AppResult<MeetingRow> {
    sqlx::query_as(&format!("select {MEETING_COLUMNS}, {ROW_SELECT} where m.id = $1 and m.org_id = $2"))
        .bind(id)
        .bind(org_id)
        .fetch_optional(db)
        .await?
        .ok_or(AppError::NotFound)
}

/// For the app and the API: the lead's meetings, when the lead is in the organization.
pub async fn of_lead(db: &PgPool, org_id: Uuid, lead_id: Uuid) -> AppResult<Vec<MeetingRow>> {
    let lead: Lead = util_scope::load(db, "leads", lead_id, org_id).await?;
    Ok(for_lead(db, lead.id).await?)
}

async fn lead_meetings(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Vec<MeetingRow>>> {
    Ok(Json(of_lead(&state.db, auth.org_id, id).await?))
}

#[derive(Deserialize)]
struct StageBody {
    accept: bool,
}

/// Accepting moves the lead's open deal to the suggested stage, or opens one there when they have none. Either way the
/// suggestion is settled once, so a second click changes nothing.
async fn decide_stage(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(body): Json<StageBody>) -> AppResult<Json<MeetingRow>> {
    let db = &state.db;
    let meeting = row(db, auth.org_id, id).await?.meeting;
    let claimed: Option<Option<Uuid>> =
        sqlx::query_scalar("update meetings set stage_status = $2 where id = $1 and stage_status = 'pending' returning suggested_stage_id")
            .bind(meeting.id)
            .bind(if body.accept { "accepted" } else { "dismissed" })
            .fetch_optional(db)
            .await?;
    let Some(stage) = claimed else { return Err(AppError::Conflict("This suggestion was already accepted or dismissed".into())) };
    if body.accept {
        let moved = match stage {
            None => Err(AppError::Conflict("That stage was removed. Move the deal on the Pipeline page.".into())),
            Some(stage) => match deals::for_lead(db, meeting.lead_id).await?.filter(|d| d.status == "open") {
                Some(deal) => deals::move_to(&state, auth.org_id, deal.id, Move { stage_id: Some(stage), ..Move::default() }).await,
                None => {
                    let deal = NewDeal { lead_id: meeting.lead_id, stage_id: Some(stage), ..NewDeal::default() };
                    deals::create(&state, auth.org_id, Some(auth.user_id), "manual", deal).await
                }
            },
        };
        if let Err(e) = moved {
            sqlx::query("update meetings set stage_status = 'pending' where id = $1").bind(meeting.id).execute(db).await?;
            return Err(e);
        }
    }
    Ok(Json(row(db, auth.org_id, id).await?))
}
