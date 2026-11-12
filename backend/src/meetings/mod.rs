//! Sales meetings from Fireflies, Fathom, Gong and Zoom, so what was said on a call moves the deal and gets a follow-up. A
//! project connects a provider in routes/meetings.rs. Fireflies, Fathom and Zoom post a signed webhook when a meeting's
//! transcript is ready, and Gong, whose webhooks need an admin's rules, is polled every 15 minutes (`spawn_worker`). A
//! meeting with one of the project's leads is kept (`record`), linked to the Splee booking it was, and the agent writes its
//! summary, next steps, a follow-up email for approval and a suggested deal stage (agent/meetings.rs).

pub mod fathom;
pub mod fireflies;
pub mod gong;
pub mod zoom;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use reqwest::RequestBuilder;
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::error::{AppError, AppResult};
use crate::models::Ts;
use crate::payments::free_mail;
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Fireflies,
    Fathom,
    Gong,
    Zoom,
}

impl Provider {
    pub const ALL: [Provider; 4] = [Provider::Fireflies, Provider::Fathom, Provider::Gong, Provider::Zoom];

    pub fn name(self) -> &'static str {
        match self {
            Provider::Fireflies => "fireflies",
            Provider::Fathom => "fathom",
            Provider::Gong => "gong",
            Provider::Zoom => "zoom",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Fireflies => "Fireflies",
            Provider::Fathom => "Fathom",
            Provider::Gong => "Gong",
            Provider::Zoom => "Zoom",
        }
    }

    pub fn parse(name: &str) -> Option<Provider> {
        Provider::ALL.into_iter().find(|p| p.name() == name)
    }

    /// Whether the webhook's signature matches the secret. `now` is in Unix seconds.
    pub fn verify(self, secret: &str, headers: &axum::http::HeaderMap, body: &[u8], now: i64) -> bool {
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default();
        // Anyone can sign with an empty key.
        if secret.is_empty() {
            return false;
        }
        match self {
            Provider::Fireflies => fireflies::verify(secret, header("x-hub-signature"), body),
            Provider::Fathom => fathom::verify(secret, header("webhook-id"), header("webhook-timestamp"), header("webhook-signature"), body, now),
            Provider::Zoom => zoom::verify(secret, header("x-zm-request-timestamp"), header("x-zm-signature"), body, now),
            Provider::Gong => false,
        }
    }

    /// What a repeated delivery of the same event shares.
    pub fn event_id(self, headers: &axum::http::HeaderMap, event: &Value) -> Option<String> {
        match self {
            Provider::Fireflies => {
                let id = text(&event["meetingId"]).or_else(|| text(&event["meeting_id"]))?;
                Some(format!("{id}:{}", text(&event["eventType"]).or_else(|| text(&event["event"])).unwrap_or_default()))
            }
            Provider::Fathom => headers.get("webhook-id").and_then(|v| v.to_str().ok()).map(str::to_string).filter(|v| !v.is_empty()),
            Provider::Zoom => Some(format!("{}:{}", text(&event["event"])?, text(&event["payload"]["object"]["uuid"])?)),
            Provider::Gong => None,
        }
    }
}

/// A signed webhook older or newer than this is turned away, so a captured one can't be replayed later.
pub const MAX_SKEW_SECONDS: i64 = 300;
/// What's kept of a transcript. An hour's meeting is about 60,000 characters.
pub const TRANSCRIPT_CHARS: usize = 200_000;
/// A meeting this close to a Splee booking's start, either way, was that booking.
const BOOKING_SLACK_SECS: i64 = 30 * 60;

/// Whether `timestamp` is a Unix time within MAX_SKEW_SECONDS of `now`.
fn fresh(timestamp: &str, now: i64) -> bool {
    timestamp.trim().parse::<i64>().is_ok_and(|ts| (now - ts).abs() <= MAX_SKEW_SECONDS)
}

/// The HMAC-SHA256 of the parts, one after another, keyed with `key`.
fn hmac(key: &[u8], parts: &[&[u8]]) -> Hmac<Sha256> {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes a key of any length");
    parts.iter().for_each(|p| mac.update(p));
    mac
}

/// Whether `sig` is the hex HMAC of the parts. The comparison is constant-time.
fn signed_hex(key: &[u8], parts: &[&[u8]], sig: &str) -> bool {
    let Ok(sig) = hex::decode(sig.trim()) else { return false };
    hmac(key, parts).verify_slice(&sig).is_ok()
}

/// A string field, None when it's missing or blank.
fn text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.trim().to_string()).filter(|s| !s.is_empty()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn rfc3339(v: &Value) -> Option<Ts> {
    v.as_str().and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&chrono::Utc))
}

/// Someone invited to or on the call. `internal` is the provider's word that they're on the team, like the organizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attendee {
    pub name: String,
    pub email: String,
    pub internal: bool,
}

impl Attendee {
    pub fn new(name: Option<String>, email: Option<String>, internal: bool) -> Option<Attendee> {
        let email = email.map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).unwrap_or_default();
        let name = name.unwrap_or_default();
        (!email.is_empty() || !name.is_empty()).then_some(Attendee { name, email, internal })
    }
}

/// Keeps the first of each address, and everyone without one.
fn dedupe(attendees: Vec<Attendee>) -> Vec<Attendee> {
    let mut seen = std::collections::HashSet::new();
    attendees.into_iter().filter(|a| a.email.is_empty() || seen.insert(a.email.clone())).collect()
}

/// A meeting as the provider reports it.
#[derive(Debug, Clone)]
pub struct Found {
    pub external_id: String,
    pub title: String,
    pub started_at: Ts,
    pub duration_secs: Option<i32>,
    pub attendees: Vec<Attendee>,
    /// The provider's own summary, or empty.
    pub summary: String,
    /// The meeting's page at the provider.
    pub recording_url: String,
    /// "Speaker: text" lines.
    pub transcript: String,
}

/// The first TRANSCRIPT_CHARS characters.
pub fn cap(transcript: &str) -> String {
    match transcript.char_indices().nth(TRANSCRIPT_CHARS) {
        Some((at, _)) => transcript[..at].to_string(),
        None => transcript.to_string(),
    }
}

/// How many times a request is tried when the provider says to slow down, and the longest wait between tries.
const TRIES: u32 = 4;
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Pages a poll may read.
pub const MAX_PAGES: usize = 50;

/// Talks to one provider. `base` is its API, or the fake's path for it when Config::meetings_api_url is set.
pub struct Api {
    pub provider: Provider,
    pub base: String,
    http: reqwest::Client,
}

impl Api {
    pub fn new(provider: Provider, fake: &str, real: &str) -> AppResult<Api> {
        let base = if fake.is_empty() { real.to_string() } else { format!("{}/{}", fake.trim_end_matches('/'), provider.name()) };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(60)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Api { provider, base, http })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// `url` with the query parameters, since reqwest here is built without its own query support.
    pub fn url(&self, url: &str, query: &[(&str, &str)]) -> AppResult<reqwest::Url> {
        reqwest::Url::parse_with_params(url, query).map_err(|e| AppError::Internal(e.into()))
    }

    /// Sends the request, waiting and trying again on 429 or 503 for as long as Retry-After says, or else a little longer
    /// each time. A 404 is Null, for something that's gone. `what` is what the key needs to read, for a 403.
    pub async fn send(&self, req: RequestBuilder, what: &str) -> AppResult<Value> {
        let label = self.provider.label();
        for attempt in 0..TRIES {
            let try_req = req.try_clone().ok_or_else(|| AppError::Internal(anyhow::anyhow!("request body can't be retried")))?;
            let res = try_req.send().await.map_err(|_| AppError::BadRequest(format!("Couldn't reach {label}. Try again in a moment.")))?;
            let status = res.status().as_u16();
            let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            let body = res.text().await.unwrap_or_default();
            match status {
                200..=299 => return Ok(serde_json::from_str(&body).unwrap_or(Value::String(body))),
                429 | 503 if attempt + 1 < TRIES => {
                    tokio::time::sleep(retry_after.map_or(Duration::from_secs(2 << attempt), Duration::from_secs).min(MAX_WAIT)).await;
                }
                401 => return Err(AppError::BadRequest(format!("{label} didn't accept those details. Copy them again from {label}."))),
                403 => return Err(AppError::BadRequest(format!("Those {label} details can't read {what}. Give them access and try again."))),
                404 => return Ok(Value::Null),
                _ => return Err(AppError::BadRequest(format!("{label} answered {status}. Try again in a few minutes."))),
            }
        }
        unreachable!("the last try returns")
    }
}

/// A project's connection to a provider.
#[derive(Debug, Clone, FromRow)]
pub struct Connection {
    pub id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub provider: String,
    pub credentials_enc: String,
    pub sync_cursor: Option<Ts>,
}

pub const CONNECTION_COLUMNS: &str = "id, org_id, project_id, provider, credentials_enc, sync_cursor";

impl Connection {
    pub fn credentials(&self, state: &AppState) -> AppResult<String> {
        Ok(crate::crypto::decrypt(&state.cfg.app_secret, &self.credentials_enc)?)
    }
}

/// The team's addresses and domains, which are never the lead: everyone in the organization and its mailboxes.
async fn team(db: &PgPool, org_id: Uuid) -> sqlx::Result<(Vec<String>, Vec<String>)> {
    let emails: Vec<String> = sqlx::query_scalar(
        "select lower(u.email::text) from memberships m join users u on u.id = m.user_id where m.org_id = $1
         union select lower(email::text) from mailboxes where org_id = $1",
    )
    .bind(org_id)
    .fetch_all(db)
    .await?;
    let domains = emails.iter().filter_map(|e| e.rsplit_once('@').map(|(_, d)| d.to_string())).collect();
    Ok((emails, domains))
}

/// The project's lead on the call: one whose address is an attendee's, else one whose company domain, without www., is an
/// attendee's domain. Free mail domains like gmail.com and the team's own domains never match by domain. A lead with an
/// open deal comes first, then the one emailed last.
pub async fn match_lead(db: &PgPool, org_id: Uuid, project_id: Uuid, attendees: &[Attendee]) -> sqlx::Result<Option<Uuid>> {
    let (team_emails, team_domains) = team(db, org_id).await?;
    let guests: Vec<String> = attendees
        .iter()
        .filter(|a| !a.internal && a.email.contains('@'))
        .map(|a| a.email.trim().to_lowercase())
        .filter(|e| !team_emails.contains(e))
        .collect();
    if guests.is_empty() {
        return Ok(None);
    }
    let order = "order by exists (select 1 from deals d where d.lead_id = l.id and d.status = 'open') desc, l.last_contacted_at desc nulls last,
                   l.created_at desc limit 1";
    let exact: Option<Uuid> = sqlx::query_scalar(&format!("select l.id from leads l where l.project_id = $1 and lower(l.email::text) = any($2) {order}"))
        .bind(project_id)
        .bind(&guests)
        .fetch_optional(db)
        .await?;
    if exact.is_some() {
        return Ok(exact);
    }
    let mut domains: Vec<String> = guests.iter().filter_map(|e| e.rsplit_once('@').map(|(_, d)| d.to_string())).collect();
    domains.retain(|d| !d.is_empty() && !free_mail(d) && !team_domains.contains(d));
    if domains.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar(&format!(
        "select l.id from leads l where l.project_id = $1 and lower(regexp_replace(l.company_domain::text, '^www\\.', '')) = any($2) {order}"
    ))
    .bind(project_id)
    .bind(&domains)
    .fetch_optional(db)
    .await
}

/// The Splee booking the meeting was: one on the organization's booking pages, not cancelled, made by the lead, starting
/// within 30 minutes of the meeting. Only the lead's own address counts, since another attendee may be a different lead with
/// a booking of their own.
async fn booking_for(db: &PgPool, org_id: Uuid, lead_id: Uuid, f: &Found) -> sqlx::Result<Option<Uuid>> {
    let lead_email: Option<String> = sqlx::query_scalar("select lower(email::text) from leads where id = $1").bind(lead_id).fetch_one(db).await?;
    let Some(email) = lead_email.filter(|e| !e.is_empty()) else { return Ok(None) };
    let emails = vec![email];
    sqlx::query_scalar(
        "select b.id from bookings b join booking_pages p on p.id = b.page_id
         where p.org_id = $1 and b.cancelled_at is null and lower(b.email::text) = any($2)
           and abs(extract(epoch from b.starts_at - $3)) <= $4
         order by abs(extract(epoch from b.starts_at - $3)) limit 1",
    )
    .bind(org_id)
    .bind(&emails)
    .bind(f.started_at)
    .bind(BOOKING_SLACK_SECS as f64)
    .fetch_optional(db)
    .await
}

/// Whether the project already has this provider's meeting.
pub async fn known(db: &PgPool, c: &Connection, external_id: &str) -> sqlx::Result<bool> {
    sqlx::query_scalar("select exists (select 1 from meetings where project_id = $1 and provider = $2 and external_id = $3)")
        .bind(c.project_id)
        .bind(&c.provider)
        .bind(external_id)
        .fetch_one(db)
        .await
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Nobody on it is one of the project's leads.
    Unmatched,
    /// Kept already, from an earlier delivery or poll.
    Known,
    New(Uuid),
}

/// Keeps a meeting with one of the project's leads and queues the agent's notes on it, once: a second delivery or an
/// overlapping poll finds it kept.
pub async fn record(state: &AppState, c: &Connection, f: &Found) -> AppResult<Recorded> {
    if known(&state.db, c, &f.external_id).await? {
        return Ok(Recorded::Known);
    }
    let Some(lead_id) = match_lead(&state.db, c.org_id, c.project_id, &f.attendees).await? else { return Ok(Recorded::Unmatched) };
    store(state, c, lead_id, f).await
}

/// Keeps the meeting against `lead_id`, with the team left out of its attendees.
pub async fn store(state: &AppState, c: &Connection, lead_id: Uuid, f: &Found) -> AppResult<Recorded> {
    let db = &state.db;
    let booking = booking_for(db, c.org_id, lead_id, f).await?;
    let (team_emails, _) = team(db, c.org_id).await?;
    let attendees: Vec<Value> = f
        .attendees
        .iter()
        .filter(|a| !a.internal && !team_emails.contains(&a.email))
        .map(|a| json!({ "name": a.name, "email": a.email }))
        .collect();
    let mut tx = db.begin().await?;
    let id: Option<Uuid> = sqlx::query_scalar(
        "insert into meetings (org_id, project_id, lead_id, connection_id, provider, external_id, booking_id, title, started_at, duration_secs,
                               attendees, provider_summary, recording_url, transcript)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
         on conflict (project_id, provider, external_id) do nothing returning id",
    )
    .bind(c.org_id)
    .bind(c.project_id)
    .bind(lead_id)
    .bind(c.id)
    .bind(&c.provider)
    .bind(&f.external_id)
    .bind(booking)
    .bind(f.title.chars().take(300).collect::<String>())
    .bind(f.started_at)
    .bind(f.duration_secs)
    .bind(Value::Array(attendees))
    .bind(f.summary.chars().take(20_000).collect::<String>())
    .bind(&f.recording_url)
    .bind(cap(&f.transcript))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(id) = id else { return Ok(Recorded::Known) };
    agent::enqueue(&mut *tx, kind::MEETING_NOTES, Some(c.project_id), Some(c.org_id), json!({ "meeting_id": id })).await?;
    tx.commit().await?;
    Ok(Recorded::New(id))
}

/// How often each Gong connection is read.
pub const POLL_EVERY: Duration = Duration::from_secs(15 * 60);

/// Reads each Gong connection every 15 minutes, a new one right away. Fireflies, Fathom and Zoom post webhooks instead.
pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = poll_due(&state).await {
                tracing::warn!(error = ?e, "meetings: poll failed");
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
    });
}

/// Polls the Gong connections not read in the last 15 minutes, one at a time, and keeps why one failed for the card.
pub async fn poll_due(state: &AppState) -> anyhow::Result<()> {
    let due: Vec<Connection> = sqlx::query_as(&format!(
        "select {CONNECTION_COLUMNS} from meeting_connections
         where provider = 'gong' and (synced_at is null or synced_at < now() - make_interval(secs => $1)) order by synced_at nulls first"
    ))
    .bind(POLL_EVERY.as_secs_f64())
    .fetch_all(&state.db)
    .await?;
    for c in due {
        let error = match gong::poll(state, &c).await {
            Ok(_) => String::new(),
            Err(e) => {
                tracing::warn!(error = ?e, connection = %c.id, "gong poll failed");
                e.public_message()
            }
        };
        sqlx::query("update meeting_connections set synced_at = now(), error = $2 where id = $1").bind(c.id).bind(error).execute(&state.db).await?;
    }
    Ok(())
}
