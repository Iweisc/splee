//! A private iCalendar feed of a project's lead reminders, so Google Calendar, Outlook or Apple Calendar shows them next
//! to the user's meetings. The secret in the URL picks the project, like the booking webhook's.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use chrono::Utc;
use sqlx::FromRow;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::Ts;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/calendar/{file}", get(feed))
}

#[derive(FromRow)]
struct Reminder {
    id: Uuid,
    full_name: String,
    email: Option<String>,
    company_name: String,
    notes: String,
    remind_at: Ts,
}

/// The URL ends in .ics because some calendar apps only accept feed links that look like files.
async fn feed(State(state): State<AppState>, Path(file): Path<String>) -> AppResult<Response> {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    let (project_id, name): (Uuid, String) =
        sqlx::query_as("select p.id, coalesce(nullif(p.name, ''), p.domain::text, '') from project_settings s join projects p on p.id = s.project_id where s.calendar_token = $1")
            .bind(token)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
    let reminders: Vec<Reminder> = sqlx::query_as(
        "select id, full_name, email::text, company_name, notes, remind_at from leads where project_id = $1 and remind_at is not null order by remind_at",
    )
    .bind(project_id)
    .fetch_all(&state.db)
    .await?;
    let body = ics(&name, &reminders, |id| hooks::lead_url(&state.cfg.public_url, project_id, id));
    Ok(([(header::CONTENT_TYPE, "text/calendar; charset=utf-8"), (header::CACHE_CONTROL, "no-cache")], body).into_response())
}

fn ics(project: &str, reminders: &[Reminder], link: impl Fn(Uuid) -> String) -> String {
    let now = stamp(Utc::now());
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Splee//Lead reminders//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        format!("X-WR-CALNAME:{}", text(&format!("Splee reminders: {project}"))),
    ];
    for r in reminders {
        let who = [r.full_name.as_str(), r.email.as_deref().unwrap_or("")].into_iter().find(|s| !s.is_empty()).unwrap_or("a lead");
        let summary = match r.company_name.as_str() {
            "" => format!("Follow up with {who}"),
            company => format!("Follow up with {who} ({company})"),
        };
        let url = link(r.id);
        let description = if r.notes.trim().is_empty() { url.clone() } else { format!("{}\n\n{url}", r.notes.trim()) };
        lines.extend([
            "BEGIN:VEVENT".to_string(),
            // The lead's id, so moving a reminder moves the event instead of adding a second one.
            format!("UID:{}@splee", r.id),
            format!("DTSTAMP:{now}"),
            format!("DTSTART:{}", stamp(r.remind_at)),
            "DURATION:PT15M".to_string(),
            format!("SUMMARY:{}", text(&summary)),
            format!("DESCRIPTION:{}", text(&description)),
            format!("URL:{url}"),
            "BEGIN:VALARM".to_string(),
            "ACTION:DISPLAY".to_string(),
            format!("DESCRIPTION:{}", text(&summary)),
            "TRIGGER:PT0M".to_string(),
            "END:VALARM".to_string(),
            "END:VEVENT".to_string(),
        ]);
    }
    lines.push("END:VCALENDAR".to_string());
    lines.iter().map(|l| fold(l) + "\r\n").collect()
}

fn stamp(at: Ts) -> String {
    at.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Escapes an iCalendar TEXT value (RFC 5545 3.3.11).
fn text(s: &str) -> String {
    s.replace('\\', "\\\\").replace(';', "\\;").replace(',', "\\,").replace("\r\n", "\\n").replace(['\r', '\n'], "\\n")
}

/// Splits a content line into lines of at most 75 bytes, each continuation starting with a space (RFC 5545 3.1).
/// It never splits inside a UTF-8 character.
fn fold(line: &str) -> String {
    let mut out = String::with_capacity(line.len() + line.len() / 70 * 3);
    let mut used = 0;
    for c in line.chars() {
        if used + c.len_utf8() > 75 {
            out.push_str("\r\n ");
            used = 1;
        }
        out.push(c);
        used += c.len_utf8();
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use chrono::Utc;
    use uuid::Uuid;

    use super::{Reminder, fold, ics, text};

    #[test]
    fn escapes_text_values() {
        assert_eq!(text("a, b; c\\d\nnext\r\nline"), "a\\, b\\; c\\\\d\\nnext\\nline");
    }

    #[test]
    fn folds_long_lines_on_character_boundaries() {
        let line = format!("SUMMARY:{}", "é".repeat(60));
        let folded = fold(&line);
        assert!(folded.split("\r\n").all(|l| l.len() <= 75));
        assert_eq!(folded.replace("\r\n ", ""), line);
        assert_eq!(fold("SHORT:x"), "SHORT:x");
    }

    #[test]
    fn writes_one_event_per_reminder() {
        let id = Uuid::nil();
        let reminders = [Reminder {
            id,
            full_name: "Sarah Lee".into(),
            email: Some("sarah@brightline.com".into()),
            company_name: "Brightline, Inc".into(),
            notes: "Asked about pricing".into(),
            remind_at: Utc.with_ymd_and_hms(2026, 11, 12, 9, 30, 0).unwrap(),
        }];
        let out = ics("Acme", &reminders, |id| format!("https://app/lead/{id}"));
        assert!(out.starts_with("BEGIN:VCALENDAR\r\n") && out.ends_with("END:VCALENDAR\r\n"));
        assert!(out.contains(&format!("UID:{id}@splee\r\n")));
        assert!(out.contains("DTSTART:20261112T093000Z\r\n"));
        assert!(out.contains("SUMMARY:Follow up with Sarah Lee (Brightline\\, Inc)\r\n"));
        assert!(out.contains("DESCRIPTION:Asked about pricing\\n\\nhttps://app/lead/"));
        assert_eq!(out.matches("BEGIN:VEVENT").count(), 1);
    }
}
