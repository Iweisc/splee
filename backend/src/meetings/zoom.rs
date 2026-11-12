//! Zoom, through a Server-to-Server OAuth app the account's admin makes in Zoom's App Marketplace, with the cloud recording
//! and past meeting scopes. Its recording.transcript_completed webhook names the meeting and the transcript file. Splee
//! downloads the transcript and asks for the participants, since the webhook only has the host.

use std::fmt::Write;

use hmac::Mac;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Api, Attendee, Found, Provider, dedupe, fresh, hmac, rfc3339, signed_hex, text};
use crate::error::{AppError, AppResult};

pub const API: &str = "https://api.zoom.us/v2";
pub const OAUTH: &str = "https://zoom.us";
pub const TRANSCRIPT_COMPLETED: &str = "recording.transcript_completed";
pub const URL_VALIDATION: &str = "endpoint.url_validation";

/// The app's credentials, kept as JSON.
#[derive(Debug, Serialize, Deserialize)]
pub struct App {
    pub account_id: String,
    pub client_id: String,
    pub client_secret: String,
}

/// x-zm-signature is "v0={hex HMAC-SHA256 of "v0:{x-zm-request-timestamp}:{body}"}" keyed with the app's secret token.
pub fn verify(secret: &str, timestamp: &str, header: &str, body: &[u8], now: i64) -> bool {
    let Some(sig) = header.trim().strip_prefix("v0=") else { return false };
    fresh(timestamp, now) && signed_hex(secret.as_bytes(), &[b"v0:", timestamp.trim().as_bytes(), b":", body], sig)
}

/// The answer to the challenge Zoom sends when the webhook URL is saved, which proves Splee has the secret token. Only a
/// signed challenge gets one, so nobody else can have Splee sign what they like.
pub fn validation(secret: &str, plain_token: &str) -> Value {
    json!({ "plainToken": plain_token, "encryptedToken": hex::encode(hmac(secret.as_bytes(), &[plain_token.as_bytes()]).finalize().into_bytes()) })
}

/// The OAuth host and the API, or the fake's paths for them.
pub struct Zoom {
    pub api: Api,
    pub oauth: String,
}

pub fn client(fake: &str) -> AppResult<Zoom> {
    let api = Api::new(Provider::Zoom, fake, API)?;
    let oauth = if fake.is_empty() { OAUTH.to_string() } else { api.base.clone() };
    let api = if fake.is_empty() { api } else { Api { base: format!("{}/v2", api.base), ..api } };
    Ok(Zoom { api, oauth })
}

impl Zoom {
    /// An account token from the app's credentials. It lasts an hour, and each webhook asks for a new one.
    pub async fn token(&self, app: &App) -> AppResult<String> {
        let url = self.api.url(&format!("{}/oauth/token", self.oauth), &[("grant_type", "account_credentials"), ("account_id", app.account_id.trim())])?;
        let req = self.api.http().post(url).basic_auth(app.client_id.trim(), Some(app.client_secret.trim()));
        let res = self.api.send(req, "recordings").await?;
        text(&res["access_token"]).ok_or_else(|| AppError::BadRequest("Zoom didn't accept those details. Copy them again from Zoom.".into()))
    }

    /// Whether `url` is one of Zoom's, so a webhook can't send Splee's token elsewhere.
    fn zooms(&self, url: &str) -> bool {
        if !self.oauth.starts_with(OAUTH) {
            return url.starts_with(&self.oauth);
        }
        reqwest::Url::parse(url).is_ok_and(|u| u.scheme() == "https" && u.host_str().is_some_and(|h| h == "zoom.us" || h.ends_with(".zoom.us")))
    }

    /// The meeting in a recording.transcript_completed event, with the transcript and the participants.
    pub async fn meeting(&self, app: &App, event: &Value) -> AppResult<Option<Found>> {
        let m = &event["payload"]["object"];
        let (Some(uuid), Some(started_at)) = (text(&m["uuid"]), rfc3339(&m["start_time"])) else { return Ok(None) };
        let token = self.token(app).await?;
        let file = m["recording_files"].as_array().into_iter().flatten().find(|f| f["file_type"] == "TRANSCRIPT" && text(&f["download_url"]).is_some());
        let transcript = match file.and_then(|f| text(&f["download_url"])).filter(|u| self.zooms(u)) {
            Some(url) => {
                let bearer = text(&event["download_token"]).unwrap_or_else(|| token.clone());
                match self.api.send(self.api.http().get(url).bearer_auth(bearer), "recordings").await? {
                    Value::String(vtt) => vtt_lines(&vtt),
                    _ => String::new(),
                }
            }
            None => String::new(),
        };
        let url = self.api.url(&format!("{}/past_meetings/{}/participants", self.api.base, meeting_path(&uuid)), &[("page_size", "300")])?;
        let req = self.api.http().get(url).bearer_auth(&token);
        // A plan without past meeting reports answers 400 or 403, and the host and transcript are still worth keeping.
        let people = self.api.send(req, "past meetings").await.unwrap_or(Value::Null);
        let host = text(&m["host_email"]).map(|e| e.to_lowercase()).unwrap_or_default();
        let mut attendees: Vec<Attendee> = people["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| {
                let email = text(&p["user_email"]).or_else(|| text(&p["email"]));
                let internal = email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&host));
                Attendee::new(text(&p["name"]), email, internal)
            })
            .collect();
        attendees.extend(Attendee::new(None, Some(host), true));
        Ok(Some(Found {
            external_id: uuid,
            title: text(&m["topic"]).unwrap_or_default(),
            started_at,
            duration_secs: m["duration"].as_f64().map(|min| (min * 60.0).round() as i32),
            attendees: dedupe(attendees),
            summary: String::new(),
            recording_url: text(&m["share_url"]).unwrap_or_default(),
            transcript,
        }))
    }
}

fn percent(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// A meeting UUID in a URL path. Zoom wants one that starts with "/" or has "//" in it encoded twice.
pub fn meeting_path(uuid: &str) -> String {
    if uuid.starts_with('/') || uuid.contains("//") { percent(&percent(uuid)) } else { percent(uuid) }
}

/// The spoken lines of a WebVTT transcript, which Zoom writes as "Name: text" under each cue's times.
pub fn vtt_lines(vtt: &str) -> String {
    vtt.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != "WEBVTT" && !l.contains("-->") && !l.chars().all(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>()
        .join("\n")
}
