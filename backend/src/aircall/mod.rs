//! Aircall, the cloud phone. A project connects its Aircall company with an API ID and token (routes/aircall.rs), and Splee
//! registers a webhook for the calls that end, the recordings that come after them and the transcripts Aircall's AI writes.
//! A call with one of the project's leads, by number, closes the lead's open call step or is kept as a call of its own, and
//! its transcript goes to the agent for a summary like a Twilio call's (calls.rs).

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::DateTime;
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::agent::{self, kind};
use crate::error::{AppError, AppResult};
use crate::models::{Call, Lead, Ts};
use crate::state::AppState;

pub const API: &str = "https://api.aircall.io";
/// What Splee's webhook listens to. A recording that wasn't ready when the call ended comes in call.comm_assets_generated.
pub const EVENTS: [&str; 3] = ["call.ended", "call.comm_assets_generated", "transcription.created"];
/// What the webhook token's keyed hash is for.
pub const TOKEN_PURPOSE: &str = "aircall-webhook";

/// A project's connection.
#[derive(Debug, Clone, FromRow)]
pub struct Connection {
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub credentials_enc: String,
    pub webhook_id: String,
    pub webhook_token_hash: String,
}

pub const CONNECTION_COLUMNS: &str = "project_id, org_id, credentials_enc, webhook_id, webhook_token_hash";

impl Connection {
    pub fn client(&self, state: &AppState) -> AppResult<Client> {
        Client::new(&state.cfg.aircall_api_url, &crate::crypto::decrypt(&state.cfg.app_secret, &self.credentials_enc)?)
    }
}

/// Talks to Aircall with a company's API ID and token, or to the fake under /aircall when Config::aircall_api_url is set. Paths
/// start with the API's version, since Aircall has V1 and V2 side by side.
pub struct Client {
    base: String,
    auth: String,
    http: reqwest::Client,
}

impl Client {
    /// `credentials` is "{api_id}:{api_token}".
    pub fn new(fake: &str, credentials: &str) -> AppResult<Client> {
        let base = if fake.is_empty() { API.to_string() } else { format!("{}/aircall", fake.trim_end_matches('/')) };
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Client { base, auth: format!("Basic {}", STANDARD.encode(credentials)), http })
    }

    /// A 404 is Null, for something Aircall doesn't have, like a call without a transcript.
    async fn send(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        self.send_or(req, |_| None).await
    }

    /// Like send, with `refused` naming the statuses only this request answers, as the error to give for each.
    async fn send_or(&self, req: reqwest::RequestBuilder, refused: impl Fn(u16) -> Option<AppError>) -> AppResult<Value> {
        let res = req.header("authorization", &self.auth).send().await.map_err(|_| AppError::BadRequest("Couldn't reach Aircall. Try again in a moment.".into()))?;
        let status = res.status().as_u16();
        if let Some(e) = refused(status) {
            return Err(e);
        }
        let body = res.text().await.unwrap_or_default();
        match status {
            200..=299 => Ok(serde_json::from_str(&body).unwrap_or(Value::Null)),
            // Aircall answers wrong credentials with a 403, and sometimes a 401.
            401 | 403 => Err(AppError::BadRequest("Aircall didn't accept that API ID and token. Copy them again from Aircall.".into())),
            404 => Ok(Value::Null),
            429 => Err(AppError::BadRequest("Aircall asked Splee to slow down. Try again in a minute.".into())),
            _ => Err(AppError::BadRequest(format!("Aircall answered {status}. Try again in a few minutes."))),
        }
    }

    pub async fn ping(&self) -> AppResult<()> {
        self.send(self.http.get(format!("{}/v1/ping", self.base))).await.map(|_| ())
    }

    /// Registers Splee's webhook at `url`, returning its id and the token Aircall puts in each of its events.
    pub async fn create_webhook(&self, url: &str) -> AppResult<(String, String)> {
        let body = json!({ "custom_name": "Splee", "url": url, "events": EVENTS });
        let res = self.send(self.http.post(format!("{}/v1/webhooks", self.base)).json(&body)).await?;
        match (text(&res["webhook"]["webhook_id"]), text(&res["webhook"]["token"])) {
            (Some(id), Some(token)) => Ok((id, token)),
            _ => Err(AppError::BadRequest("Aircall didn't create the webhook. Try again in a few minutes.".into())),
        }
    }

    /// Best effort, since a webhook someone deleted in Aircall is gone either way.
    pub async fn delete_webhook(&self, id: &str) {
        if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return;
        }
        if let Err(e) = self.send(self.http.delete(format!("{}/v1/webhooks/{id}", self.base))).await {
            tracing::warn!(error = ?e, "aircall: deleting the webhook failed");
        }
    }

    pub async fn call(&self, id: i64) -> AppResult<Value> {
        Ok(self.send(self.http.get(format!("{}/v1/calls/{id}", self.base))).await?["call"].clone())
    }

    /// A link to the call's recording, or the voicemail the lead left, that works for an hour. None when there's neither.
    pub async fn recording(&self, id: i64) -> AppResult<Option<String>> {
        let call = self.call(id).await?;
        Ok(text(&call["recording"]).or_else(|| text(&call["voicemail"])).filter(|url| url.starts_with("https://")))
    }

    /// Puts `to` in the Aircall Workspace of whoever has `email` in Aircall, ready to call from a number they pick. Aircall
    /// only does this in Workspace on a desktop, for someone available and not on a call.
    pub async fn dial(&self, email: &str, to: &str) -> AppResult<()> {
        // User V2 finds someone by email as well as by id, and has no dial of its own yet, so dialing stays on V1.
        let mut url = reqwest::Url::parse(&format!("{}/v2/users", self.base)).map_err(|e| AppError::Internal(e.into()))?;
        url.path_segments_mut().map_err(|_| AppError::Internal(anyhow::anyhow!("Aircall's URL can't take a path")))?.push(email);
        let found = self.send(self.http.get(url)).await?;
        let Some(id) = found["user"]["id"].as_i64() else {
            return Err(AppError::Conflict(format!("Nobody in your Aircall has the email {email}. Use the same email in Aircall as in Splee.")));
        };
        let refused = |status| match status {
            405 => Some(AppError::Conflict("Aircall has you unavailable or on a call. Set yourself available in Workspace on your desktop.".into())),
            400 => Some(AppError::Conflict(format!("Aircall couldn't dial {to}."))),
            _ => None,
        };
        self.send_or(self.http.post(format!("{}/v1/users/{id}/dial", self.base)).json(&json!({ "to": to })), refused).await.map(|_| ())
    }

    /// The transcript as "Us: …" and "Lead: …" lines, or empty without one.
    pub async fn transcript(&self, id: i64) -> AppResult<String> {
        let res = self.send(self.http.get(format!("{}/v1/calls/{id}/transcription", self.base))).await?;
        Ok(lines(&res["transcription"]["content"]["utterances"]))
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

fn lines(utterances: &Value) -> String {
    utterances
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|u| Some(format!("{}: {}", if u["participant_type"] == "internal" { "Us" } else { "Lead" }, text(&u["text"])?)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// A call as Aircall's events and GET /calls/{id} give it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub id: i64,
    /// The other side's number in E.164.
    pub number: String,
    pub inbound: bool,
    pub started_at: Ts,
    pub answered_at: Option<Ts>,
    pub ended_at: Option<Ts>,
    /// The team member who made or took it, lowercased.
    pub user_email: String,
    /// The recording's page in Aircall, which only the team can open, or empty while there's no recording or voicemail.
    pub asset: String,
}

/// None for a call without a number Splee can read, like an anonymous one.
pub fn parse(call: &Value) -> Option<Ended> {
    let at = |v: &Value| v.as_i64().and_then(|s| DateTime::from_timestamp(s, 0));
    let recorded = text(&call["recording"]).is_some() || text(&call["voicemail"]).is_some();
    Some(Ended {
        id: call["id"].as_i64()?,
        number: call["raw_digits"].as_str().and_then(|n| crate::phone::normalize(n, ""))?,
        inbound: call["direction"] == "inbound",
        started_at: at(&call["started_at"])?,
        answered_at: at(&call["answered_at"]),
        ended_at: at(&call["ended_at"]),
        user_email: text(&call["user"]["email"]).map(|e| e.to_lowercase()).unwrap_or_default(),
        asset: text(&call["asset"]).filter(|_| recorded).unwrap_or_default(),
    })
}

impl Ended {
    /// The outcome the Calls page knows, and a note for a call the lead made.
    fn outcome(&self) -> (&'static str, &'static str) {
        match (self.answered_at.is_some(), self.inbound) {
            (true, false) => ("connected", ""),
            (true, true) => ("connected", "They called in."),
            (false, false) => ("no_answer", ""),
            (false, true) if !self.asset.is_empty() => ("no_answer", "They called and left a voicemail."),
            (false, true) => ("no_answer", "They called and nobody answered."),
        }
    }

    /// How long they talked, without the ringing.
    fn talk_secs(&self) -> Option<i32> {
        Some((self.ended_at? - self.answered_at?).num_seconds() as i32).filter(|s| *s >= 0)
    }
}

/// Keeps a call with one of the project's leads, once: it closes the lead's open call step, like one logged on the Calls page,
/// or is kept as a call of its own. A repeated event, or the recording arriving later, only adds a recording the call didn't
/// have. Returns the call, or None when no lead in the project has that number.
///
/// The call and what it does to the lead are kept together, since a retry finds the call and goes no further. Aircall's calls
/// are only connected or not answered, and of what calls::apply_outcome does, that only moves last_contacted_at.
pub async fn record(state: &AppState, c: &Connection, e: &Ended) -> AppResult<Option<Uuid>> {
    let mut tx = state.db.begin().await?;
    let kept: Option<Uuid> = sqlx::query_scalar(
        "update calls set recording_url = case when $3 <> '' then $3 else recording_url end where project_id = $1 and aircall_id = $2 returning id",
    )
    .bind(c.project_id)
    .bind(e.id)
    .bind(&e.asset)
    .fetch_optional(&mut *tx)
    .await?;
    if kept.is_some() {
        tx.commit().await?;
        return Ok(kept);
    }
    let lead: Option<Lead> =
        sqlx::query_as("select * from leads where project_id = $1 and phone = $2 order by last_contacted_at desc nulls last, created_at desc limit 1")
            .bind(c.project_id)
            .bind(&e.number)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(lead) = lead else { return Ok(None) };
    let user: Option<Uuid> =
        sqlx::query_scalar("select u.id from users u join memberships m on m.user_id = u.id where m.org_id = $1 and lower(u.email::text) = $2")
            .bind(c.org_id)
            .bind(&e.user_email)
            .fetch_optional(&mut *tx)
            .await?;
    let (outcome, notes) = e.outcome();
    let (call_status, ended) = (if e.answered_at.is_some() { "completed" } else { "no-answer" }, e.ended_at.unwrap_or(e.started_at));
    let open: Option<Call> = sqlx::query_as(
        "update calls set status = 'done', outcome = $2, notes = $3, closed_at = $4, called_by = coalesce(called_by, $5), called_at = $6,
           aircall_id = $7, to_number = $8, call_status = $9, duration_secs = $10, recording_url = $11
         where id = (select id from calls where lead_id = $1 and status = 'open' order by due_at limit 1) and status = 'open' returning *",
    )
    .bind(lead.id)
    .bind(outcome)
    .bind(notes)
    .bind(ended)
    .bind(user)
    .bind(e.started_at)
    .bind(e.id)
    .bind(&e.number)
    .bind(call_status)
    .bind(e.talk_secs())
    .bind(&e.asset)
    .fetch_optional(&mut *tx)
    .await?;
    let call: Option<Call> = match open {
        Some(call) => Some(call),
        // A delivery of the same call at the same moment keeps it first, and this one finds it below.
        None => sqlx::query_as(
            "insert into calls (project_id, lead_id, segment_id, kind, owner_id, status, to_number, called_by, called_at, closed_at, outcome, notes,
                                aircall_id, call_status, duration_secs, recording_url)
             values ($1, $2, $3, 'manual', $4, 'done', $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             on conflict (project_id, aircall_id) where aircall_id is not null do nothing returning *",
        )
        .bind(lead.project_id)
        .bind(lead.id)
        .bind(lead.segment_id)
        .bind(lead.owner_id)
        .bind(&e.number)
        .bind(user)
        .bind(e.started_at)
        .bind(ended)
        .bind(outcome)
        .bind(notes)
        .bind(e.id)
        .bind(call_status)
        .bind(e.talk_secs())
        .bind(&e.asset)
        .fetch_optional(&mut *tx)
        .await?,
    };
    let Some(call) = call else {
        tx.commit().await?;
        let kept = sqlx::query_scalar("select id from calls where project_id = $1 and aircall_id = $2").bind(c.project_id).bind(e.id);
        return Ok(kept.fetch_optional(&state.db).await?);
    };
    if outcome == "connected" {
        sqlx::query("update leads set last_contacted_at = greatest(last_contacted_at, $2) where id = $1")
            .bind(lead.id)
            .bind(ended)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Some(call.id))
}

/// Puts Aircall's transcript on the call and asks the agent for a summary, once. A transcript can come before the call's own
/// event, so a call Splee hasn't seen is read from Aircall and kept first.
pub async fn transcribe(state: &AppState, c: &Connection, client: &Client, aircall_id: i64) -> AppResult<bool> {
    let db = &state.db;
    let mut call: Option<Uuid> =
        sqlx::query_scalar("select id from calls where project_id = $1 and aircall_id = $2").bind(c.project_id).bind(aircall_id).fetch_optional(db).await?;
    if call.is_none()
        && let Some(e) = parse(&client.call(aircall_id).await?)
    {
        call = record(state, c, &e).await?;
    }
    let Some(call) = call else { return Ok(false) };
    let transcript = client.transcript(aircall_id).await?;
    if transcript.is_empty() {
        return Ok(true);
    }
    // Saved with its summary run, since a retry finds the transcript and doesn't ask again.
    let mut tx = db.begin().await?;
    let saved: Option<Uuid> = sqlx::query_scalar("update calls set transcript = $2, transcribed_at = now() where id = $1 and transcribed_at is null returning id")
        .bind(call)
        .bind(&transcript)
        .fetch_optional(&mut *tx)
        .await?;
    if saved.is_some() {
        agent::enqueue(&mut *tx, kind::SUMMARIZE_CALL, Some(c.project_id), Some(c.org_id), json!({ "call_id": call })).await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Handles one webhook event, returning whether its call was with one of the project's leads.
pub async fn handle(state: &AppState, c: &Connection, event: &Value) -> AppResult<bool> {
    match event["event"].as_str().unwrap_or_default() {
        "call.ended" | "call.comm_assets_generated" => match parse(&event["data"]) {
            Some(e) => Ok(record(state, c, &e).await?.is_some()),
            None => Ok(false),
        },
        "transcription.created" => {
            // Aircall's docs give call_id as a string, and its calls' ids are numbers.
            let id = &event["data"]["call_id"];
            match id.as_i64().or_else(|| id.as_str().and_then(|s| s.trim().parse().ok())) {
                Some(id) => transcribe(state, c, &c.client(state)?, id).await,
                None => Ok(false),
            }
        }
        _ => Ok(false),
    }
}
