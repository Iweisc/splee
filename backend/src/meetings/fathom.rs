//! Fathom, with an API key from its settings. Its webhook for new meeting content carries the meeting, usually with the
//! transcript and summary, and signs it the Standard Webhooks way.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hmac::Mac;
use serde_json::Value;

use super::{Api, Attendee, Found, Provider, dedupe, fresh, hmac, rfc3339, text};
use crate::error::AppResult;

pub const API: &str = "https://api.fathom.ai/external/v1";

/// webhook-signature is space-separated "v1,{base64 HMAC-SHA256 of "{webhook-id}.{webhook-timestamp}.{body}"}", one per
/// secret while one is rolled. The key is the secret after "whsec_", base64-decoded.
pub fn verify(secret: &str, id: &str, timestamp: &str, header: &str, body: &[u8], now: i64) -> bool {
    let secret = secret.trim();
    let key = secret.strip_prefix("whsec_").and_then(|s| STANDARD.decode(s).ok()).unwrap_or_else(|| secret.as_bytes().to_vec());
    if id.is_empty() || !fresh(timestamp, now) {
        return false;
    }
    let mac = hmac(&key, &[id.as_bytes(), b".", timestamp.trim().as_bytes(), b".", body]);
    header.split_whitespace().filter_map(|s| s.strip_prefix("v1,")).filter_map(|s| STANDARD.decode(s).ok()).any(|sig| mac.clone().verify_slice(&sig).is_ok())
}

pub async fn check(api: &Api, key: &str) -> AppResult<()> {
    let req = api.http().get(api.url(&format!("{}/meetings", api.base), &[("limit", "1")])?).header("x-api-key", key.trim());
    api.send(req, "meetings").await?;
    Ok(())
}

fn lines(transcript: &Value) -> String {
    transcript
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| Some(format!("{}: {}", text(&l["speaker"]["display_name"]).unwrap_or_else(|| "Speaker".into()), text(&l["text"])?)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The meeting in a webhook. A webhook set up without the transcript gets it from the recording.
pub async fn meeting(api: &Api, key: &str, m: &Value) -> AppResult<Option<Found>> {
    let Some(mut found) = parse(m) else { return Ok(None) };
    if found.transcript.is_empty()
        && let Some(id) = text(&m["recording_id"]).filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric()))
    {
        let req = api.http().get(format!("{}/recordings/{id}/transcript", api.base)).header("x-api-key", key.trim());
        found.transcript = lines(&api.send(req, "transcripts").await?["transcript"]);
    }
    Ok(Some(found))
}

/// The person who recorded it is the team, and so are the invitees Fathom doesn't mark external.
pub fn parse(m: &Value) -> Option<Found> {
    let external_id = text(&m["recording_id"]).or_else(|| text(&m["url"]))?;
    let started_at = rfc3339(&m["recording_start_time"]).or_else(|| rfc3339(&m["scheduled_start_time"])).or_else(|| rfc3339(&m["created_at"]))?;
    let ended = rfc3339(&m["recording_end_time"]).or_else(|| rfc3339(&m["scheduled_end_time"]));
    let mut attendees: Vec<Attendee> = m["calendar_invitees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| Attendee::new(text(&a["name"]), text(&a["email"]), a["is_external"] != true))
        .collect();
    attendees.extend(Attendee::new(text(&m["recorded_by"]["name"]), text(&m["recorded_by"]["email"]), true));
    Some(Found {
        external_id,
        title: text(&m["meeting_title"]).or_else(|| text(&m["title"])).unwrap_or_default(),
        started_at,
        duration_secs: ended.map(|e| (e - started_at).num_seconds() as i32).filter(|s| *s > 0),
        attendees: dedupe(attendees),
        summary: text(&m["default_summary"]["markdown_formatted"]).unwrap_or_default(),
        recording_url: text(&m["share_url"]).or_else(|| text(&m["url"])).unwrap_or_default(),
        transcript: lines(&m["transcript"]),
    })
}

pub fn api(fake: &str) -> AppResult<Api> {
    Api::new(Provider::Fathom, fake, API)
}
