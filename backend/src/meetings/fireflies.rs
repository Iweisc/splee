//! Fireflies, with an API key from its Integrations page. Its webhook says a meeting's transcript is ready, and the meeting
//! is read through its GraphQL API.

use chrono::DateTime;
use serde_json::{Value, json};

use super::{Api, Attendee, Found, Provider, dedupe, signed_hex, text};
use crate::error::{AppError, AppResult};

pub const API: &str = "https://api.fireflies.ai";

const TRANSCRIPT: &str = "query Transcript($id: String!) {
  transcript(id: $id) {
    id title date duration transcript_url organizer_email participants
    meeting_attendees { displayName email name }
    sentences { speaker_name text }
    summary { overview action_items }
  }
}";

/// x-hub-signature is the hex HMAC-SHA256 of the body with the webhook secret, sometimes after "sha256=".
pub fn verify(secret: &str, header: &str, body: &[u8]) -> bool {
    let sig = header.trim();
    signed_hex(secret.as_bytes(), &[body], sig.strip_prefix("sha256=").unwrap_or(sig))
}

/// Whether the event says a transcript is ready: "Transcription completed" from the first webhooks, "meeting.transcribed"
/// from the newer ones.
pub fn transcribed(event: &Value) -> bool {
    let kind = text(&event["eventType"]).or_else(|| text(&event["event"])).unwrap_or_default().to_lowercase();
    kind == "transcription completed" || kind == "meeting.transcribed"
}

pub fn meeting_id(event: &Value) -> Option<String> {
    text(&event["meetingId"]).or_else(|| text(&event["meeting_id"]))
}

async fn graphql(api: &Api, key: &str, query: &str, variables: Value) -> AppResult<Value> {
    let req = api.http().post(format!("{}/graphql", api.base)).bearer_auth(key.trim()).json(&json!({ "query": query, "variables": variables }));
    let res = api.send(req, "transcripts").await?;
    // GraphQL answers 200 with its errors in the body.
    if let Some(error) = res["errors"].as_array().and_then(|e| e.first()) {
        let code = text(&error["extensions"]["code"]).or_else(|| text(&error["code"])).unwrap_or_default();
        if code.contains("auth") || code.contains("forbidden") {
            return Err(AppError::BadRequest("Fireflies didn't accept that API key. Copy it again from Fireflies.".into()));
        }
        if code.contains("not_found") {
            return Ok(Value::Null);
        }
        return Err(AppError::BadRequest(format!("Fireflies said: {}", text(&error["message"]).unwrap_or_else(|| "something went wrong".into()))));
    }
    Ok(res["data"].clone())
}

pub async fn check(api: &Api, key: &str) -> AppResult<()> {
    let data = graphql(api, key, "query { user { email } }", json!({})).await?;
    if data["user"].is_null() {
        return Err(AppError::BadRequest("Fireflies didn't accept that API key. Copy it again from Fireflies.".into()));
    }
    Ok(())
}

/// The meeting, or None when Fireflies doesn't have it any more.
pub async fn meeting(api: &Api, key: &str, id: &str) -> AppResult<Option<Found>> {
    let data = graphql(api, key, TRANSCRIPT, json!({ "id": id })).await?;
    Ok(parse(&data["transcript"]))
}

/// `date` is milliseconds since 1970 and `duration` minutes. The organizer is the team, and so are the attendees Fireflies
/// only knows from the participant list when they share the organizer's address.
pub fn parse(t: &Value) -> Option<Found> {
    let external_id = text(&t["id"])?;
    let started_at = t["date"].as_f64().and_then(|ms| DateTime::from_timestamp_millis(ms as i64))?;
    let organizer = text(&t["organizer_email"]).map(|e| e.to_lowercase()).unwrap_or_default();
    let mut attendees: Vec<Attendee> = t["meeting_attendees"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let email = text(&a["email"]);
            let internal = email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(&organizer));
            Attendee::new(text(&a["displayName"]).or_else(|| text(&a["name"])), email, internal)
        })
        .collect();
    attendees.extend(t["participants"].as_array().into_iter().flatten().filter_map(|p| {
        let email = text(p)?;
        let internal = email.eq_ignore_ascii_case(&organizer);
        Attendee::new(None, Some(email), internal)
    }));
    attendees.extend(Attendee::new(None, Some(organizer), true));
    let transcript = t["sentences"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| Some(format!("{}: {}", text(&s["speaker_name"]).unwrap_or_else(|| "Speaker".into()), text(&s["text"])?)))
        .collect::<Vec<_>>()
        .join("\n");
    let summary = [text(&t["summary"]["overview"]), text(&t["summary"]["action_items"]).map(|a| format!("Action items:\n{a}"))]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n\n");
    Some(Found {
        external_id,
        title: text(&t["title"]).unwrap_or_default(),
        started_at,
        duration_secs: t["duration"].as_f64().map(|m| (m * 60.0).round() as i32),
        attendees: dedupe(attendees),
        summary,
        recording_url: text(&t["transcript_url"]).unwrap_or_default(),
        transcript,
    })
}

pub fn api(fake: &str) -> AppResult<Api> {
    Api::new(Provider::Fireflies, fake, API)
}
