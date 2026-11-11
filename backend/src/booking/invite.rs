//! The emails a booking sends both sides, from the page's mailbox, each carrying the call as a calendar invite. A
//! REQUEST puts it in their calendars and a later one with the same UID and a higher SEQUENCE moves it there. A CANCEL
//! takes it out.

use chrono::Utc;
use chrono_tz::Tz;

use crate::models::{Booking, BookingPage, Mailbox, Ts};
use crate::routes::calendar::{fold, stamp, text};
use crate::state::AppState;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    Booked,
    Moved,
    Cancelled,
}

/// Where the emails' links go: the page, and the booking's own page behind its token, which moves or cancels it.
pub struct Links {
    pub page: String,
    pub manage: String,
}

impl Links {
    pub fn new(public_url: &str, page: &BookingPage, token: &str) -> Links {
        Links { page: page.url(public_url), manage: format!("{public_url}/booking/{token}") }
    }

    fn cancel(&self) -> String {
        format!("{}?cancel=1", self.manage)
    }
}

/// Who the call is with: the mailbox's from name, or its address without one.
pub struct Host<'a> {
    pub name: &'a str,
    pub email: &'a str,
}

impl<'a> Host<'a> {
    pub fn of(mb: &'a Mailbox) -> Host<'a> {
        Host { name: if mb.from_name.trim().is_empty() { &mb.email } else { mb.from_name.trim() }, email: &mb.email }
    }
}

/// A parameter value in quotes, which can't hold a quote itself (RFC 5545 3.2).
fn quoted(s: &str) -> String {
    format!("\"{}\"", s.replace(['"', '\r', '\n'], ""))
}

pub fn ics(page: &BookingPage, host: &Host, booking: &Booking, change: Change, links: &Links) -> String {
    let (method, status) = if change == Change::Cancelled { ("CANCEL", "CANCELLED") } else { ("REQUEST", "CONFIRMED") };
    let mut description = Vec::new();
    if !booking.note.is_empty() {
        description.push(format!("Note from {}: {}", booking.name, booking.note));
    }
    if !page.location.is_empty() {
        description.push(page.location.clone());
    }
    if change != Change::Cancelled {
        description.push(format!("Move or cancel: {}", links.manage));
    }
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//Splee//Booking pages//EN".to_string(),
        "CALSCALE:GREGORIAN".to_string(),
        format!("METHOD:{method}"),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}@splee", booking.id),
        format!("SEQUENCE:{}", booking.sequence),
        format!("DTSTAMP:{}", stamp(Utc::now())),
        format!("DTSTART:{}", stamp(booking.starts_at)),
        format!("DTEND:{}", stamp(booking.ends_at)),
        format!("SUMMARY:{}", text(&format!("{}: {} and {}", page.name, booking.name, host.name))),
        format!("DESCRIPTION:{}", text(&description.join("\n\n"))),
    ];
    if !page.location.is_empty() {
        lines.push(format!("LOCATION:{}", text(&page.location)));
    }
    lines.extend([
        format!("ORGANIZER;CN={}:mailto:{}", quoted(host.name), host.email),
        format!("ATTENDEE;CN={};ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:{}", quoted(&booking.name), booking.email),
        format!("ATTENDEE;CN={};ROLE=CHAIR;PARTSTAT=ACCEPTED:mailto:{}", quoted(host.name), host.email),
        format!("STATUS:{status}"),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ]);
    lines.iter().map(|l| fold(l) + "\r\n").collect()
}

/// "Tuesday 10 November, 14:00 to 14:30 (America/New York time)".
pub fn when(start: Ts, end: Ts, tz: Tz) -> String {
    let (start, end) = (start.with_timezone(&tz), end.with_timezone(&tz));
    format!("{}, {} to {} ({} time)", start.format("%A %-d %B"), start.format("%H:%M"), end.format("%H:%M"), tz.name().replace('_', " "))
}

/// "Tue 10 Nov at 14:00", for subjects.
fn short(at: Ts, tz: Tz) -> String {
    at.with_timezone(&tz).format("%a %-d %b at %H:%M").to_string()
}

/// The guest's subject and text, with times in their own time zone.
pub fn to_guest(page: &BookingPage, host: &Host, booking: &Booking, change: Change, links: &Links) -> (String, String) {
    let tz: Tz = booking.timezone.parse().unwrap_or(chrono_tz::UTC);
    let (host, call) = (host.name, page.name.as_str());
    let first = booking.name.split_whitespace().next().unwrap_or(&booking.name);
    let at = when(booking.starts_at, booking.ends_at, tz);
    let (subject, mut body) = match change {
        Change::Booked => (format!("Booked: {call} with {host}, {}", short(booking.starts_at, tz)), format!("Hi {first},\n\nYour {call} with {host} is booked for {at}.")),
        Change::Moved => (format!("Moved: {call} with {host}, {}", short(booking.starts_at, tz)), format!("Hi {first},\n\nYour {call} with {host} has moved to {at}.")),
        Change::Cancelled => (
            format!("Cancelled: {call} with {host}, {}", short(booking.starts_at, tz)),
            format!("Hi {first},\n\nYour {call} with {host} on {at} is cancelled.\n\nTo pick another time: {}", links.page),
        ),
    };
    if change != Change::Cancelled {
        if !page.location.is_empty() {
            body.push_str(&format!("\n\nWhere: {}", page.location));
        }
        body.push_str(&format!(
            "\n\nThe invite is attached, so the call is in your calendar.\n\nNeed another time? {}\nCan't make it? {}",
            links.manage,
            links.cancel()
        ));
    }
    (subject, body)
}

/// The host's subject and text, with times in the page's time zone.
pub fn to_host(page: &BookingPage, booking: &Booking, change: Change, links: &Links) -> (String, String) {
    let tz = page.tz();
    let (guest, call) = (format!("{} ({})", booking.name, booking.email), page.name.as_str());
    let at = when(booking.starts_at, booking.ends_at, tz);
    let (subject, mut body) = match change {
        Change::Booked => (format!("New booking: {}, {}", booking.name, short(booking.starts_at, tz)), format!("{guest} booked a call for {at}: {call}.")),
        Change::Moved => (format!("Moved: {}, {}", booking.name, short(booking.starts_at, tz)), format!("{guest} moved their {call} to {at}.")),
        Change::Cancelled => (format!("Cancelled: {}, {}", booking.name, short(booking.starts_at, tz)), format!("{guest} cancelled their {call} on {at}.")),
    };
    if !booking.note.is_empty() && change != Change::Cancelled {
        body.push_str(&format!("\n\nTheir note: {}", booking.note));
    }
    if change != Change::Cancelled {
        body.push_str(&format!("\n\nMove or cancel it: {}", links.manage));
    }
    (subject, body)
}

/// Emails the guest and the host in the background, so the visitor doesn't wait on SMTP. With SENDING_ENABLED=false
/// nothing goes out, like every other email.
pub fn send(state: &AppState, page: &BookingPage, booking: &Booking, token: &str, change: Change) {
    if !state.cfg.sending_enabled {
        return tracing::warn!(booking = %booking.id, "SENDING_ENABLED=false: booking emails not sent");
    }
    let (state, page, booking) = (state.clone(), page.clone(), booking.clone());
    let links = Links::new(&state.cfg.public_url, &page, token);
    tokio::spawn(async move {
        let mb: Mailbox = match sqlx::query_as("select * from mailboxes where id = $1").bind(page.mailbox_id).fetch_one(&state.db).await {
            Ok(mb) => mb,
            Err(e) => return tracing::error!(error = %e, "couldn't load a booking page's mailbox"),
        };
        let host = Host::of(&mb);
        let ics = ics(&page, &host, &booking, change, &links);
        let method = if change == Change::Cancelled { "CANCEL" } else { "REQUEST" };
        let to_guest = (booking.name.as_str(), booking.email.as_str(), to_guest(&page, &host, &booking, change, &links));
        let to_host = (mb.from_name.as_str(), mb.email.as_str(), to_host(&page, &booking, change, &links));
        for (name, email, (subject, body)) in [to_guest, to_host] {
            if let Err(e) = crate::mail::send_invite(&state, &mb, (name, email), &subject, &body, &ics, method).await {
                tracing::error!(error = %e, booking = %booking.id, "couldn't send a booking email");
            }
        }
    });
}
