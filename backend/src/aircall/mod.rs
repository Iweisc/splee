//! Aircall, the cloud phone. A project connects its Aircall company with an API ID and token (routes/aircall.rs), and Splee
//! registers a webhook for the calls that end, the recordings that come after them and the transcripts Aircall's AI writes.
//! What a call does with the project's leads is cloud_phone.rs, shared with Dialpad.

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

use crate::cloud_phone::{self, Ended, Phone, Project};
use crate::error::{AppError, AppResult};
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
    pub fn project(&self) -> Project {
        Project { id: self.project_id, org_id: self.org_id }
    }

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

/// A call as Aircall's events and GET /calls/{id} give it. None for a call without a number Splee can read, like an anonymous
/// one.
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
        // Aircall's page for the call, which only the team can open.
        asset: text(&call["asset"]).filter(|_| recorded).unwrap_or_default(),
        step: None,
    })
}

/// Puts Aircall's transcript on the call for the agent to sum up. A transcript can come before the call's own event, so a call
/// Splee hasn't seen is read from Aircall and kept first.
pub async fn transcribe(state: &AppState, c: &Connection, client: &Client, aircall_id: i64) -> AppResult<bool> {
    let mut call = cloud_phone::kept(state, Phone::Aircall, c.project(), aircall_id).await?;
    if call.is_none()
        && let Some(e) = parse(&client.call(aircall_id).await?)
    {
        call = cloud_phone::record(state, Phone::Aircall, c.project(), &e).await?;
    }
    let Some(call) = call else { return Ok(false) };
    cloud_phone::transcribed(state, c.project(), call, &client.transcript(aircall_id).await?).await?;
    Ok(true)
}

/// Handles one webhook event, returning whether its call was with one of the project's leads.
pub async fn handle(state: &AppState, c: &Connection, event: &Value) -> AppResult<bool> {
    match event["event"].as_str().unwrap_or_default() {
        "call.ended" | "call.comm_assets_generated" => match parse(&event["data"]) {
            Some(e) => Ok(cloud_phone::record(state, Phone::Aircall, c.project(), &e).await?.is_some()),
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
