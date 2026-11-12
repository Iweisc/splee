//! Gong, with an access key and secret from its API page. Gong's webhooks need an admin to write automation rules, so Splee
//! reads its calls every 15 minutes instead, from a few hours before the end of the last read, since Gong lists a call
//! only once it has processed it. The overlap finds calls already kept, which `record` skips.

use std::collections::HashMap;

use chrono::{Duration, Utc};
use serde_json::{Value, json};

use super::{Api, Attendee, Connection, Found, MAX_PAGES, Provider, Recorded, dedupe, known, match_lead, rfc3339, store, text};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

pub const API: &str = "https://api.gong.io";
/// How far back each read starts before the end of the last one.
pub const OVERLAP: Duration = Duration::hours(6);
/// How far back the first read goes.
pub const FIRST_LOOK_BACK: Duration = Duration::days(7);

/// The access key and secret, kept as "{key}:{secret}".
fn basic(credentials: &str) -> AppResult<(&str, &str)> {
    credentials.split_once(':').ok_or_else(|| AppError::Internal(anyhow::anyhow!("gong credentials without a secret")))
}

pub async fn check(api: &Api, credentials: &str) -> AppResult<()> {
    let (key, secret) = basic(credentials)?;
    api.send(api.http().get(format!("{}/v2/workspaces", api.base)).basic_auth(key, Some(secret)), "workspaces").await?;
    Ok(())
}

/// A call from /v2/calls/extensive, without its transcript. Parties Gong calls Internal are the team.
pub fn parse(call: &Value) -> Option<Found> {
    let meta = &call["metaData"];
    let attendees = call["parties"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Attendee::new(text(&p["name"]), text(&p["emailAddress"]), p["affiliation"] == "Internal"))
        .collect();
    let summary = [text(&call["content"]["brief"]), key_points(&call["content"]["keyPoints"])].into_iter().flatten().collect::<Vec<_>>().join("\n\n");
    Some(Found {
        external_id: text(&meta["id"])?,
        title: text(&meta["title"]).unwrap_or_default(),
        started_at: rfc3339(&meta["started"])?,
        duration_secs: meta["duration"].as_f64().map(|s| s.round() as i32),
        attendees: dedupe(attendees),
        summary,
        recording_url: text(&meta["url"]).unwrap_or_default(),
        transcript: String::new(),
    })
}

fn key_points(points: &Value) -> Option<String> {
    let points: Vec<String> = points.as_array()?.iter().filter_map(|p| text(&p["text"])).map(|p| format!("- {p}")).collect();
    (!points.is_empty()).then(|| format!("Key points:\n{}", points.join("\n")))
}

/// Each monologue as "Name: sentences", the name from the call's parties by speaker id.
async fn transcript(api: &Api, credentials: &str, call: &Value) -> AppResult<String> {
    let (key, secret) = basic(credentials)?;
    let id = text(&call["metaData"]["id"]).unwrap_or_default();
    let req = api.http().post(format!("{}/v2/calls/transcript", api.base)).basic_auth(key, Some(secret)).json(&json!({ "filter": { "callIds": [id] } }));
    let res = api.send(req, "transcripts").await?;
    let names: HashMap<String, String> =
        call["parties"].as_array().into_iter().flatten().filter_map(|p| Some((text(&p["speakerId"])?, text(&p["name"])?))).collect();
    let monologues = res["callTranscripts"].as_array().and_then(|t| t.first()).map(|t| t["transcript"].clone()).unwrap_or_default();
    Ok(monologues
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let said: Vec<String> = m["sentences"].as_array()?.iter().filter_map(|s| text(&s["text"])).collect();
            let who = text(&m["speakerId"]).and_then(|id| names.get(&id).cloned()).unwrap_or_else(|| "Speaker".into());
            (!said.is_empty()).then(|| format!("{who}: {}", said.join(" ")))
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Reads the calls since the last read, a page at a time by Gong's cursor, and keeps the ones with a lead. The transcript is
/// only fetched for those. Returns how many calls Gong listed and how many were new meetings.
pub async fn poll(state: &AppState, c: &Connection) -> AppResult<(usize, usize)> {
    let credentials = c.credentials(state)?;
    let (key, secret) = basic(&credentials)?;
    let api = api(&state.cfg.meetings_api_url)?;
    let to = Utc::now();
    let from = c.sync_cursor.map_or(to - FIRST_LOOK_BACK, |at| at - OVERLAP);
    let (mut listed, mut kept) = (0, 0);
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut body = json!({
            "filter": { "fromDateTime": from.to_rfc3339(), "toDateTime": to.to_rfc3339() },
            "contentSelector": { "context": "Extended", "exposedFields": { "parties": true, "content": { "brief": true, "keyPoints": true } } },
        });
        if let Some(cursor) = &cursor {
            body["cursor"] = json!(cursor);
        }
        let req = api.http().post(format!("{}/v2/calls/extensive", api.base)).basic_auth(key, Some(secret)).json(&body);
        // Gong answers 404 when no calls fall in the window.
        let page = api.send(req, "calls").await?;
        for call in page["calls"].as_array().into_iter().flatten() {
            listed += 1;
            let Some(mut found) = parse(call) else { continue };
            if known(&state.db, c, &found.external_id).await? {
                continue;
            }
            let Some(lead) = match_lead(&state.db, c.org_id, c.project_id, &found.attendees).await? else { continue };
            found.transcript = transcript(&api, &credentials, call).await?;
            if matches!(store(state, c, lead, &found).await?, Recorded::New(_)) {
                kept += 1;
            }
        }
        cursor = text(&page["records"]["cursor"]);
        if cursor.is_none() {
            break;
        }
    }
    sqlx::query("update meeting_connections set sync_cursor = $2 where id = $1").bind(c.id).bind(to).execute(&state.db).await?;
    Ok((listed, kept))
}

pub fn api(fake: &str) -> AppResult<Api> {
    Api::new(Provider::Gong, fake, API)
}
