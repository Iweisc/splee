//! JustCall, the cloud phone. A project connects its JustCall account with an API key and secret (routes/justcall.rs), and
//! Splee adds its webhook URL to the call events it reads: calls that end, missed calls, voicemails and the AI report with
//! the transcript. JustCall signs each event with the API secret. What a call does with the project's leads is
//! cloud_phone.rs, shared with Aircall and Dialpad.

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use chrono::{Duration, NaiveDateTime};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::FromRow;
use uuid::Uuid;

use crate::cloud_phone::{self, Ended, Phone, Project};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const API: &str = "https://api.justcall.io";
/// The events Splee's URL is added to. JustCall may send them in any order, and each carries the whole call.
pub const EVENTS: [&str; 4] = ["call.completed", "call.missed", "call.voicemail", "jc.call_ai_generated"];

/// A project's connection.
#[derive(Debug, Clone, FromRow)]
pub struct Connection {
    pub project_id: Uuid,
    pub org_id: Uuid,
    pub api_key_enc: String,
    pub api_secret_enc: String,
    pub url_ids: Vec<String>,
}

pub const CONNECTION_COLUMNS: &str = "project_id, org_id, api_key_enc, api_secret_enc, url_ids";

impl Connection {
    pub fn project(&self) -> Project {
        Project { id: self.project_id, org_id: self.org_id }
    }

    pub fn client(&self, state: &AppState) -> AppResult<Client> {
        Client::new(&state.cfg.justcall_api_url, &crate::crypto::decrypt(&state.cfg.app_secret, &self.api_key_enc)?, &self.secret(state)?)
    }

    /// What JustCall signs this account's events with.
    pub fn secret(&self, state: &AppState) -> AppResult<String> {
        Ok(crate::crypto::decrypt(&state.cfg.app_secret, &self.api_secret_enc)?)
    }
}

/// Talks to JustCall with an account's API key and secret, or to the fake under /justcall when Config::justcall_api_url is set.
pub struct Client {
    base: String,
    auth: String,
    http: reqwest::Client,
}

impl Client {
    pub fn new(fake: &str, api_key: &str, api_secret: &str) -> AppResult<Client> {
        let base = if fake.is_empty() { API.to_string() } else { format!("{}/justcall", fake.trim_end_matches('/')) };
        let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Client { base, auth: format!("{api_key}:{api_secret}"), http })
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> AppResult<Value> {
        let res = req.header("authorization", &self.auth).header("accept", "application/json").send().await.map_err(|_| unreachable())?;
        let status = res.status().as_u16();
        let body = res.text().await.unwrap_or_default();
        match status {
            200..=299 => Ok(serde_json::from_str(&body).unwrap_or(Value::Null)),
            401 => Err(AppError::BadRequest(
                "JustCall didn't accept that API key and secret. Copy both from APIs and Webhooks in JustCall's settings.".into(),
            )),
            403 => Err(AppError::BadRequest("JustCall turned the request away. Its API and webhooks come with the Team plan and above.".into())),
            404 => Ok(Value::Null),
            429 => Err(AppError::BadRequest("JustCall asked Splee to slow down. Try again in a minute.".into())),
            _ => Err(AppError::BadRequest(format!("JustCall answered {status}. Try again in a few minutes."))),
        }
    }

    fn url(&self, path: &str) -> String {
        format!("{}/v2.1{path}", self.base)
    }

    /// Lists the account's webhooks, which costs nothing and needs what Splee needs: a working key and webhook access.
    pub async fn ping(&self) -> AppResult<()> {
        self.send(self.http.get(self.url("/webhooks"))).await.map(|_| ())
    }

    /// Adds `url` to each event Splee reads and returns the id JustCall gave it on each. When one fails, the URL is taken off
    /// the events it was already added to.
    pub async fn hook(&self, url: &str) -> AppResult<Vec<String>> {
        let mut ids = vec![];
        for event in EVENTS {
            let added = self.send(self.http.post(self.url("/webhooks")).json(&json!({ "type": event, "webhook_url": url }))).await;
            let found = added.as_ref().ok().and_then(|body| {
                let urls = object(body)["webhook_urls"].as_array()?;
                urls.iter().find(|u| u["webhook_url"] == url).and_then(|u| text(&u["url_id"]))
            });
            match found {
                Some(id) => ids.push(id),
                None => {
                    self.unhook(&ids).await;
                    return Err(added.err().unwrap_or_else(|| AppError::BadRequest("JustCall didn't add the webhook. Try again in a few minutes.".into())));
                }
            }
        }
        Ok(ids)
    }

    /// Best effort, since a URL someone removed in JustCall is gone either way.
    pub async fn unhook(&self, url_ids: &[String]) {
        for id in url_ids.iter().filter(|id| !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())) {
            if let Err(e) = self.send(self.http.delete(self.url(&format!("/webhooks/url/{id}")))).await {
                tracing::warn!(error = ?e, "justcall: removing the webhook URL failed");
            }
        }
    }

    /// The call's recording or voicemail, with its content type. JustCall's own link in the event is for people signed in to
    /// JustCall, so the player gets it through the API instead.
    pub async fn recording(&self, call_id: i64) -> AppResult<(String, Vec<u8>)> {
        let req = self.http.get(self.url(&format!("/calls/{call_id}/recording/download"))).header("authorization", &self.auth);
        let res = req.send().await.map_err(|_| unreachable())?;
        match res.status().as_u16() {
            200 => {
                let kind = res.headers().get("content-type").and_then(|v| v.to_str().ok()).filter(|t| t.starts_with("audio/")).unwrap_or("audio/mpeg");
                let kind = kind.to_string();
                Ok((kind, res.bytes().await.map_err(|_| AppError::BadRequest("JustCall's recording didn't come through. Try again.".into()))?.to_vec()))
            }
            404 => Err(AppError::NotFound),
            status => Err(AppError::BadRequest(format!("JustCall answered {status} for the recording. Try again in a few minutes."))),
        }
    }
}

fn unreachable() -> AppError {
    AppError::BadRequest("Couldn't reach JustCall. Try again in a moment.".into())
}

/// The docs show the object itself, and JustCall's list answers put what they return under `data`, so either is read.
fn object(body: &Value) -> &Value {
    match &body["data"] {
        Value::Object(_) => &body["data"],
        Value::Array(items) if !items.is_empty() => &items[0],
        _ => body,
    }
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// JustCall's ids are numbers, sometimes sent as strings of digits.
fn id(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str()?.trim().parse().ok())
}

fn secs(v: &Value) -> i64 {
    id(v).unwrap_or(0).max(0)
}

/// Whether JustCall signed an event with `secret`: the hex HMAC-SHA256, keyed with the secret, of
/// "{secret}|{the event's webhook_url, percent-encoded}|{its type}|{the timestamp header}". The signature doesn't cover the
/// call itself, so the URL's own token, which the webhook route checks is in that webhook_url, keeps another project's calls out.
pub fn signed(secret: &str, event: &Value, timestamp: &str, signature: &str) -> bool {
    let Ok(signature) = hex::decode(signature.trim()) else { return false };
    let url = event["webhook_url"].as_str().unwrap_or_default();
    let kind = event["type"].as_str().unwrap_or_default();
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    mac.update(format!("{secret}|{}|{kind}|{timestamp}", encode_uri_component(url)).as_bytes());
    mac.verify_slice(&signature).is_ok()
}

/// What JavaScript's encodeURIComponent does, which the docs sign the URL with.
fn encode_uri_component(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A call as its events give it. None for one without a number Splee can read.
pub fn parse(call: &Value) -> Option<Ended> {
    let info = &call["call_info"];
    let duration = &call["call_duration"];
    let at = format!("{} {}", call["call_date"].as_str()?.trim(), call["call_time"].as_str()?.trim());
    let started_at = NaiveDateTime::parse_from_str(&at, "%Y-%m-%d %H:%M:%S").ok()?.and_utc();
    let kind = info["type"].as_str().unwrap_or_default().trim().to_ascii_lowercase();
    let answered = kind == "answered";
    let rang = started_at + Duration::seconds(secs(&duration["ring_time"]));
    let answered_at = answered.then_some(rang);
    let ended_at = answered_at.map_or(rang, |at| at + Duration::seconds(secs(&duration["conversation_time"])));
    // JustCall writes numbers with their country code but without the +.
    let number = text(&call["contact_number"]).and_then(|n| crate::phone::normalize(&format!("+{}", n.trim_start_matches('+')), ""))?;
    Some(Ended {
        id: id(&call["id"])?,
        number,
        inbound: info["direction"].as_str().is_some_and(|d| d.trim().eq_ignore_ascii_case("incoming")),
        started_at,
        answered_at,
        ended_at: Some(ended_at),
        user_email: text(&call["agent_email"]).map(|e| e.to_lowercase()).unwrap_or_default(),
        // JustCall's sample missed call has a recording link too, and a missed call is one without a voicemail.
        asset: text(&info["recording"]).filter(|_| kind != "missed").unwrap_or_default(),
        step: None,
        later_leg: false,
    })
}

/// The AI report's transcript as lines in the order they were said, each named for its speaker when JustCall knows them. It
/// doesn't say which speaker is the team, so the lines don't guess.
pub fn transcript(call: &Value) -> String {
    let mut lines: Vec<&Value> = call["justcall_ai"]["call_transcription"].as_array().into_iter().flatten().collect();
    lines.sort_by_key(|l| secs(&l["timestamp"]["starttime"]));
    let said = |l: &&Value| {
        let speaker = text(&l["speaker_name"]).or_else(|| Some(format!("Speaker {}", id(&l["speaker_id"])?))).unwrap_or_else(|| "Speaker".into());
        Some(format!("{speaker}: {}", text(&l["sentence"])?))
    };
    lines.iter().filter_map(said).collect::<Vec<_>>().join("\n")
}

/// Handles one event, returning whether its call was with one of the project's leads.
pub async fn handle(state: &AppState, c: &Connection, event: &Value) -> AppResult<bool> {
    let kind = event["type"].as_str().unwrap_or_default();
    if !EVENTS.contains(&kind) {
        return Ok(false);
    }
    let Some(e) = parse(&event["data"]) else { return Ok(false) };
    let Some(call) = cloud_phone::record(state, Phone::Justcall, c.project(), &e).await? else { return Ok(false) };
    if kind == "jc.call_ai_generated" {
        cloud_phone::transcribed(state, c.project(), call, &transcript(&event["data"])).await?;
    }
    Ok(true)
}
