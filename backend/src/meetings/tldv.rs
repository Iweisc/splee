//! tl;dv, with an API key from its personal settings. Its webhooks aren't signed, so Splee reads its external meetings every 15
//! minutes instead, like Gong's, from a day before the end of the last read. A meeting with a lead whose transcript tl;dv
//! can't give yet is left for a later read, for up to a week.

use chrono::{Duration, SecondsFormat, Utc};
use serde_json::Value;

use super::{Api, Attendee, Connection, Found, MAX_PAGES, Provider, dedupe, known, match_lead, rfc3339, store, text};
use crate::error::AppResult;
use crate::models::Ts;
use crate::state::AppState;

pub const API: &str = "https://pasta.tldv.io/v1alpha1";
/// How far back each read starts before the end of the last one.
pub const OVERLAP: Duration = Duration::hours(24);
/// How far back the first read goes.
pub const FIRST_LOOK_BACK: Duration = Duration::days(7);
/// Meetings a page, tl;dv's most.
const PAGE: usize = 100;

/// tl;dv turns away requests without a User-Agent of their own.
fn get(api: &Api, url: impl reqwest::IntoUrl, key: &str) -> reqwest::RequestBuilder {
    api.http().get(url).header("x-api-key", key.trim()).header("user-agent", "Splee-tldv-integration/1.0")
}

pub async fn check(api: &Api, key: &str) -> AppResult<()> {
    api.send(get(api, api.url(&format!("{}/meetings", api.base), &[("limit", "1")])?, key), "meetings").await?;
    Ok(())
}

/// A meeting from GET /meetings, without its transcript or notes. The organizer recorded it, so they're the team, and they
/// come first so their entry is the one kept when they're an invitee too.
pub fn parse(m: &Value) -> Option<Found> {
    let organizer = Attendee::new(text(&m["organizer"]["name"]), text(&m["organizer"]["email"]), true);
    let invitees = m["invitees"].as_array().into_iter().flatten().filter_map(|a| Attendee::new(text(&a["name"]), text(&a["email"]), false));
    Some(Found {
        // The id goes into the transcript's path, so only one that can't change the path.
        external_id: text(&m["id"]).filter(|id| id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))?,
        title: text(&m["name"]).unwrap_or_default(),
        started_at: rfc3339(&m["happenedAt"])?,
        duration_secs: m["duration"].as_f64().map(|s| s.round() as i32).filter(|s| *s > 0),
        attendees: dedupe(organizer.into_iter().chain(invitees).collect()),
        summary: String::new(),
        recording_url: text(&m["url"]).unwrap_or_default(),
        transcript: String::new(),
    })
}

/// Each sentence as "Speaker: text".
fn lines(transcript: &Value) -> String {
    transcript["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| Some(format!("{}: {}", text(&l["speaker"]).unwrap_or_else(|| "Speaker".into()), text(&l["text"])?)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// One read: the window a day at a time, and what it leaves for the card and the next read.
struct Read<'a> {
    state: &'a AppState,
    c: &'a Connection,
    api: Api,
    key: String,
    to: Ts,
    /// The oldest meeting with a lead still without its transcript, which the next read has to cover again.
    waiting: Option<Ts>,
    /// Meetings whose transcript tl;dv turned away, and the last reason it gave.
    refused: usize,
    reason: String,
    /// Whether a day had more meetings than MAX_PAGES pages.
    cut: bool,
}

impl Read<'_> {
    async fn day(&mut self, from: Ts, until: Ts) -> AppResult<()> {
        let (from, until) = (from.to_rfc3339_opts(SecondsFormat::Secs, true), until.to_rfc3339_opts(SecondsFormat::Secs, true));
        let limit = PAGE.to_string();
        for page in 1..=MAX_PAGES {
            let page_s = page.to_string();
            let query = [("from", from.as_str()), ("to", until.as_str()), ("meetingType", "external"), ("limit", limit.as_str()), ("page", page_s.as_str())];
            let res = self.api.send(get(&self.api, self.api.url(&format!("{}/meetings", self.api.base), &query)?, &self.key), "meetings").await?;
            for m in res["results"].as_array().into_iter().flatten() {
                self.meeting(m).await?;
            }
            let pages = res["pages"].as_u64().unwrap_or(0);
            if page as u64 >= pages {
                return Ok(());
            }
        }
        self.cut = true;
        Ok(())
    }

    /// Keeps a meeting with a lead once its transcript is done, with tl;dv's notes as the summary. A transcript that isn't
    /// done yet, or that tl;dv turns away, keeps the meeting waiting for a later read without stopping this one.
    async fn meeting(&mut self, m: &Value) -> AppResult<()> {
        let (db, c) = (&self.state.db, self.c);
        let Some(mut found) = parse(m) else { return Ok(()) };
        if known(db, c, &found.external_id).await? {
            return Ok(());
        }
        let Some(lead) = match_lead(db, c.org_id, c.project_id, &found.attendees).await? else { return Ok(()) };
        let meeting = format!("{}/meetings/{}", self.api.base, found.external_id);
        // A 404 is Null: tl;dv gives a transcript only once it's done.
        match self.api.send(get(&self.api, format!("{meeting}/transcript"), &self.key), "transcripts").await {
            Ok(t) => found.transcript = lines(&t),
            Err(e) => {
                self.refused += 1;
                self.reason = e.public_message();
            }
        }
        if found.transcript.is_empty() {
            // Not after a week, so one tl;dv never finishes can't hold every later read open.
            if found.started_at > self.to - FIRST_LOOK_BACK {
                self.waiting = Some(self.waiting.map_or(found.started_at, |w| w.min(found.started_at)));
            }
            return Ok(());
        }
        found.summary = match self.api.send(get(&self.api, format!("{meeting}/notes"), &self.key), "notes").await {
            Ok(notes) => text(&notes["markdownContent"]).unwrap_or_default(),
            Err(e) => {
                tracing::warn!(error = ?e, meeting = found.external_id, "tl;dv notes failed");
                String::new()
            }
        };
        store(self.state, c, lead, &found).await?;
        Ok(())
    }

    /// What the card shows after a read that went through, empty when nothing was left out.
    fn warning(&self) -> String {
        let mut warning = Vec::new();
        match self.refused {
            0 => {}
            1 => warning.push(format!("Couldn't read the transcript of a meeting with your leads, so it waits for a later read. {}", self.reason)),
            n => warning.push(format!("Couldn't read the transcripts of {n} meetings with your leads, so they wait for a later read. {}", self.reason)),
        }
        if self.cut {
            warning.push(format!("tl;dv listed more than {} meetings on one day. Splee read the first {0}.", MAX_PAGES * PAGE));
        }
        warning.join(" ")
    }
}

/// Reads the external meetings from a day before the cursor, or a week back the first time, to now, a day at a time so
/// tl;dv's page cap only ever cuts a single day. The cursor then moves to now, or only up to the oldest meeting still waiting
/// for its transcript, so the next read covers it again. Returns the card's warning about what was left out.
pub async fn poll(state: &AppState, c: &Connection) -> AppResult<String> {
    let to = Utc::now();
    let mut read =
        Read { state, c, api: api(&state.cfg.meetings_api_url)?, key: c.credentials(state)?, to, waiting: None, refused: 0, reason: String::new(), cut: false };
    let mut from = c.sync_cursor.map_or(to - FIRST_LOOK_BACK, |at| at - OVERLAP);
    while from < to {
        let until = (from + Duration::days(1)).min(to);
        read.day(from, until).await?;
        from = until;
    }
    let cursor = read.waiting.map_or(to, |at| at.min(to));
    sqlx::query("update meeting_connections set sync_cursor = $2 where id = $1").bind(c.id).bind(cursor).execute(&state.db).await?;
    Ok(read.warning())
}

pub fn api(fake: &str) -> AppResult<Api> {
    Api::new(Provider::Tldv, fake, API)
}
