//! Dialpad, the cloud phone. A project connects its Dialpad company with an API key (routes/dialpad.rs), and Splee registers a
//! webhook, signed with a secret of its own, and a subscription to the calls that hang up, the recordings that come after them
//! and the transcripts Dialpad's AI writes. A call started from the Calls page carries its call step's id in `custom_data`,
//! which Dialpad hands back in the call's events. What a call does with the project's leads is cloud_phone.rs, shared with
//! Aircall.

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use chrono::DateTime;
use serde_json::{Value, json};
use sqlx::FromRow;
use uuid::Uuid;

use crate::cloud_phone::{self, Ended, Phone, Project};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const API: &str = "https://dialpad.com";
/// The call states Splee's subscription asks for. A recording that wasn't ready when the call hung up comes in `recording`.
pub const STATES: [&str; 3] = ["hangup", "recording", "call_transcription"];

/// A project's connection.
#[derive(Debug, Clone, FromRow)]
pub struct Connection {
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub api_key_enc: String,
    pub webhook_id: String,
    pub subscription_id: String,
    pub secret_enc: String,
}

pub const CONNECTION_COLUMNS: &str = "project_id, org_id, api_key_enc, webhook_id, subscription_id, secret_enc";

impl Connection {
    pub fn project(&self) -> Project {
        Project { id: self.project_id, org_id: self.org_id }
    }

    pub fn client(&self, state: &AppState) -> AppResult<Client> {
        Client::new(&state.cfg.dialpad_api_url, &crate::crypto::decrypt(&state.cfg.app_secret, &self.api_key_enc)?)
    }

    /// What Dialpad signs this webhook's events with.
    pub fn secret(&self, state: &AppState) -> AppResult<String> {
        Ok(crate::crypto::decrypt(&state.cfg.app_secret, &self.secret_enc)?)
    }
}

/// Talks to Dialpad with a company's API key, or to the fake under /dialpad when Config::dialpad_api_url is set.
pub struct Client {
    base: String,
    auth: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(fake: &str, api_key: &str) -> AppResult<Client> {
        let base = if fake.is_empty() { API.to_string() } else { format!("{}/dialpad", fake.trim_end_matches('/')) };
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Client { base, auth: format!("Bearer {api_key}"), http })
    }

    /// A 404 is Null, for something Dialpad doesn't have, like a call without a transcript.
    async fn send(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        self.send_or(req, |_| None).await
    }

    /// Like send, with `refused` naming the statuses only this request answers, as the error to give for each.
    async fn send_or(&self, req: reqwest::RequestBuilder, refused: impl Fn(u16) -> Option<AppError>) -> AppResult<Value> {
        let res = req.header("authorization", &self.auth).send().await.map_err(|_| AppError::BadRequest("Couldn't reach Dialpad. Try again in a moment.".into()))?;
        let status = res.status().as_u16();
        if let Some(e) = refused(status) {
            return Err(e);
        }
        let body = res.text().await.unwrap_or_default();
        match status {
            200..=299 => Ok(serde_json::from_str(&body).unwrap_or(Value::Null)),
            401 | 403 => {
                Err(AppError::BadRequest("Dialpad didn't accept that API key. Make a company API key in Dialpad's admin settings and paste it here.".into()))
            }
            404 => Ok(Value::Null),
            429 => Err(AppError::BadRequest("Dialpad asked Splee to slow down. Try again in a minute.".into())),
            _ => Err(AppError::BadRequest(format!("Dialpad answered {status}. Try again in a few minutes."))),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/api/v2{path}", self.base)
    }

    pub async fn ping(&self) -> AppResult<()> {
        self.send(self.http.get(self.url("/company"))).await.map(|_| ())
    }

    /// Registers Splee's webhook at `url`, signed with `secret`, and subscribes it to the call states Splee reads. Returns the
    /// webhook's id and the subscription's. A webhook whose subscription failed is deleted again.
    pub async fn hook(&self, url: &str, secret: &str) -> AppResult<(String, String)> {
        let failed = || AppError::BadRequest("Dialpad didn't create the webhook. Try again in a few minutes.".into());
        let webhook = self.send(self.http.post(self.url("/webhooks")).json(&json!({ "hook_url": url, "secret": secret }))).await?;
        let webhook = id(&webhook["id"]).ok_or_else(failed)?;
        let body = json!({ "endpoint_id": webhook, "call_states": STATES, "enabled": true });
        let subscription = match self.send(self.http.post(self.url("/subscriptions/call")).json(&body)).await.map(|s| id(&s["id"])) {
            Ok(Some(subscription)) => subscription,
            failure => {
                self.unhook(&webhook.to_string(), "").await;
                return Err(failure.err().unwrap_or_else(failed));
            }
        };
        Ok((webhook.to_string(), subscription.to_string()))
    }

    /// Best effort, since a webhook or subscription someone deleted in Dialpad is gone either way.
    pub async fn unhook(&self, webhook_id: &str, subscription_id: &str) {
        for (what, id) in [("subscriptions/call", subscription_id), ("webhooks", webhook_id)] {
            if id.is_empty() || !id.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            if let Err(e) = self.send(self.http.delete(self.url(&format!("/{what}/{id}")))).await {
                tracing::warn!(error = ?e, what, "dialpad: deleting the webhook failed");
            }
        }
    }

    /// Calls `to` from the Dialpad app of whoever has `email` in Dialpad, on their computer or in their browser, with `step`
    /// in the call's custom_data so its events say which call step it was.
    pub async fn dial(&self, email: &str, to: &str, step: Uuid) -> AppResult<()> {
        let mut url = reqwest::Url::parse(&self.url("/users")).map_err(|e| AppError::Internal(e.into()))?;
        url.query_pairs_mut().append_pair("email", email);
        let found = self.send(self.http.get(url)).await?;
        let Some(user) = found["items"].as_array().and_then(|users| users.first()).and_then(|u| id(&u["id"])) else {
            return Err(AppError::Conflict(format!("Nobody in your Dialpad has the email {email}. Use the same email in Dialpad as in Splee.")));
        };
        // Dialpad doesn't say why it couldn't, and the usual reason is no Dialpad app open to ring.
        let refused = |status| match status {
            400 => Some(AppError::Conflict(format!("Dialpad couldn't call {to}. Open Dialpad on your computer or in your browser and try again."))),
            429 => Some(AppError::Conflict("Dialpad starts 5 calls a minute for you at most. Try again in a minute.".into())),
            _ => None,
        };
        let body = json!({ "phone_number": to, "custom_data": step.to_string() });
        self.send_or(self.http.post(self.url(&format!("/users/{user}/initiate_call"))).json(&body), refused).await.map(|_| ())
    }

    /// The transcript as "Us: …" and "Lead: …" lines in the order they were said, or empty without one.
    pub async fn transcript(&self, call_id: i64) -> AppResult<String> {
        let res = self.send(self.http.get(self.url(&format!("/transcripts/{call_id}")))).await?;
        let mut lines: Vec<&Value> = res["lines"].as_array().into_iter().flatten().filter(|l| l["type"] == "transcript").collect();
        lines.sort_by_key(|l| l["time"].as_str().unwrap_or_default().to_string());
        let said = |l: &&Value| Some(format!("{}: {}", if l["user_id"].is_null() { "Lead" } else { "Us" }, text(&l["content"])?));
        Ok(lines.iter().filter_map(said).collect::<Vec<_>>().join("\n"))
    }

    /// The recording at `url`, from a call's event, as MP3. Only a URL on Dialpad gets the API key.
    pub async fn recording(&self, url: &str) -> AppResult<Vec<u8>> {
        if !url.starts_with(&format!("{}/", self.base)) {
            return Err(AppError::NotFound);
        }
        let unreachable = |_| AppError::BadRequest("Couldn't reach Dialpad. Try again in a moment.".into());
        let res = self.http.get(url).header("authorization", &self.auth).send().await.map_err(unreachable)?;
        match res.status().as_u16() {
            200 => Ok(res.bytes().await.map_err(|_| AppError::BadRequest("Dialpad's recording didn't come through. Try again.".into()))?.to_vec()),
            404 => Err(AppError::NotFound),
            status => Err(AppError::BadRequest(format!("Dialpad answered {status} for the recording. Try again in a few minutes."))),
        }
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// Dialpad's ids are numbers, sometimes sent as strings of digits.
fn id(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

/// A call as its events give it. None for one without a number Splee can read, or that hasn't started.
pub fn parse(event: &Value) -> Option<Ended> {
    let at = |v: &Value| v.as_f64().or_else(|| v.as_str()?.trim().parse().ok()).and_then(|ms| DateTime::from_timestamp_millis(ms as i64));
    let target = &event["target"];
    let recordings = event["recording_details"].as_array().into_iter().flatten();
    let recording = recordings.filter(|r| r["recording_type"] == "callrecording" || r["recording_type"] == "voicemail").find_map(|r| text(&r["url"]));
    Some(Ended {
        // Each leg of a transferred call is a call of its own, as it's a conversation with someone else on the team, with
        // its own outcome and transcript.
        id: id(&event["call_id"])?,
        number: text(&event["external_number"]).or_else(|| text(&event["contact"]["phone"])).and_then(|n| crate::phone::normalize(&n, ""))?,
        inbound: event["direction"] == "inbound",
        started_at: at(&event["date_started"])?,
        answered_at: at(&event["date_connected"]),
        ended_at: at(&event["date_ended"]),
        user_email: text(&target["email"]).filter(|_| target["type"] == "user").map(|e| e.to_lowercase()).unwrap_or_default(),
        asset: recording.unwrap_or_default(),
        step: text(&event["custom_data"]).and_then(|data| data.parse().ok()),
        later_leg: id(&event["master_call_id"]).is_some_and(|first| Some(first) != id(&event["call_id"])),
    })
}

/// Handles one webhook event, returning whether its call was with one of the project's leads. Dialpad may send a call's
/// events in any order, and each carries the whole call, so any of them keeps it.
pub async fn handle(state: &AppState, c: &Connection, event: &Value) -> AppResult<bool> {
    if !STATES.contains(&event["state"].as_str().unwrap_or_default()) {
        return Ok(false);
    }
    let Some(e) = parse(event) else { return Ok(false) };
    let Some(call) = cloud_phone::record(state, Phone::Dialpad, c.project(), &e).await? else { return Ok(false) };
    if event["state"] == "call_transcription" {
        cloud_phone::transcribed(state, c.project(), call, &c.client(state)?.transcript(e.id).await?).await?;
    }
    Ok(true)
}
