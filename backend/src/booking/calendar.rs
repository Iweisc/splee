//! Busy times from the secret iCal address of someone's calendar: Google Calendar's "Secret address in iCal format",
//! an Outlook calendar published as ICS, or an iCloud public calendar. The feed is fetched through hooks::public_client
//! and kept for a few minutes, so a busy booking page doesn't fetch it on every visit.
//!
//! The reader is deliberately lenient: a line or event it can't make sense of is skipped, not the whole calendar.
//! Recurring events are expanded with the rrule crate, minus their EXDATEs and the occurrences that were moved or
//! cancelled on their own (a second VEVENT with the same UID and a RECURRENCE-ID).

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration as StdDuration, Instant};

use anyhow::Context;
use chrono::{DateTime, Days, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use reqwest::Url;

use crate::models::Ts;

const FRESH_FOR: StdDuration = StdDuration::from_secs(5 * 60);
/// A feed that fails to load keeps its last busy times this long, so a calendar host's hiccup doesn't open up the week.
const STALE_FOR: StdDuration = StdDuration::from_secs(60 * 60);
/// Google's feed for a calendar with years of history runs to a few megabytes.
const MAX_BYTES: usize = 16 << 20;
/// More occurrences than any calendar has in a booking window, to stop a rule like FREQ=MINUTELY.
const MAX_OCCURRENCES: u16 = 2000;

#[derive(Debug, Clone, PartialEq)]
enum When {
    Utc(Ts),
    /// A local time, with the time zone its TZID named when Splee knows it. Without one it's in the page's time zone.
    Local(NaiveDateTime, Option<Tz>),
    Date(NaiveDate),
}

impl When {
    fn parse(value: &str, params: &[(String, String)]) -> Option<When> {
        let value = value.trim();
        let tzid = params.iter().find(|(k, _)| k == "TZID").and_then(|(_, v)| zone(v));
        if param(params, "VALUE") == Some("DATE") || value.len() == 8 {
            return NaiveDate::parse_from_str(value, "%Y%m%d").ok().map(When::Date);
        }
        if let Some(utc) = value.strip_suffix('Z') {
            return NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok().map(|t| When::Utc(t.and_utc()));
        }
        NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok().map(|t| When::Local(t, tzid))
    }

    /// The instant, in its own time zone, or `fallback` for a floating time or a date. A UTC time stays in UTC, so a rule
    /// on it repeats at the same UTC time. A local time the clocks skipped becomes the hour after.
    fn resolve(&self, fallback: Tz) -> Option<DateTime<Tz>> {
        match self {
            When::Utc(t) => Some(t.with_timezone(&chrono_tz::UTC)),
            When::Local(t, tz) => local(tz.unwrap_or(fallback), *t),
            When::Date(d) => local(fallback, d.and_hms_opt(0, 0, 0)?),
        }
    }
}

fn local(tz: Tz, t: NaiveDateTime) -> Option<DateTime<Tz>> {
    tz.from_local_datetime(&t).earliest().or_else(|| tz.from_local_datetime(&(t + Duration::hours(1))).earliest())
}

#[derive(Debug, Clone, PartialEq)]
enum Length {
    Exact(Duration),
    /// An all-day event lasts whole days, which aren't always 24 hours.
    Days(u64),
}

#[derive(Debug, Clone)]
pub struct Event {
    uid: String,
    start: When,
    length: Length,
    rrule: Option<String>,
    exdates: Vec<When>,
    recurrence_id: Option<When>,
    /// Cancelled, or marked free (TRANSP:TRANSPARENT), so it blocks nothing. A cancelled override still removes the
    /// occurrence it replaces.
    free: bool,
}

fn param<'a>(params: &'a [(String, String)], key: &str) -> Option<&'a str> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// A content line as (NAME, [(PARAM, value)], value).
type Line = (String, Vec<(String, String)>, String);

/// The content lines of an iCalendar file, unfolded. Names and parameter names are upper case, quotes are taken off
/// parameter values.
fn content_lines(ics: &str) -> Vec<Line> {
    let mut unfolded: Vec<String> = Vec::new();
    for line in ics.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l)) {
        match (line.strip_prefix([' ', '\t']), unfolded.last_mut()) {
            (Some(rest), Some(last)) => last.push_str(rest),
            _ if !line.trim().is_empty() => unfolded.push(line.to_string()),
            _ => {}
        }
    }
    unfolded.iter().filter_map(|line| split_line(line)).collect()
}

fn split_line(line: &str) -> Option<Line> {
    // The value starts at the first colon outside quotes. A TZID can be quoted and hold a colon.
    let mut quoted = false;
    let colon = line.char_indices().find_map(|(i, c)| match c {
        '"' => {
            quoted = !quoted;
            None
        }
        ':' if !quoted => Some(i),
        _ => None,
    })?;
    let (head, value) = (&line[..colon], &line[colon + 1..]);
    let mut parts = Vec::new();
    let (mut part, mut quoted) = (String::new(), false);
    for c in head.chars() {
        match c {
            '"' => quoted = !quoted,
            ';' if !quoted => parts.push(std::mem::take(&mut part)),
            c => part.push(c),
        }
    }
    parts.push(part);
    let name = parts.remove(0).trim().to_ascii_uppercase();
    let params = parts.into_iter().filter_map(|p| p.split_once('=').map(|(k, v)| (k.trim().to_ascii_uppercase(), v.to_string()))).collect();
    Some((name, params, value.to_string()))
}

/// The events in a calendar file. Anything that isn't a VEVENT, like a to-do or a time zone definition, is skipped, and so
/// is an event without a start Splee can read.
pub fn parse(ics: &str) -> Vec<Event> {
    let mut events = Vec::new();
    // Inside a VEVENT, how many components deep below it (an alarm is one), so an alarm's lines don't count as the event's.
    let mut inside: Option<usize> = None;
    let mut props: Vec<Line> = Vec::new();
    for (name, params, value) in content_lines(ics) {
        match (name.as_str(), inside) {
            ("BEGIN", None) if value.trim().eq_ignore_ascii_case("VEVENT") => {
                inside = Some(0);
                props.clear();
            }
            ("BEGIN", Some(depth)) => inside = Some(depth + 1),
            ("END", Some(0)) => {
                inside = None;
                events.extend(event(&props));
            }
            ("END", Some(depth)) => inside = Some(depth - 1),
            (_, Some(0)) => props.push((name, params, value)),
            _ => {}
        }
    }
    events
}

fn event(props: &[Line]) -> Option<Event> {
    let get = |key: &str| props.iter().find(|(name, ..)| name == key);
    // Some calendars leave DTSTART off a cancelled occurrence. Its RECURRENCE-ID still says which one it frees.
    let (_, start_params, start_value) = get("DTSTART").or_else(|| get("RECURRENCE-ID"))?;
    let start = When::parse(start_value, start_params)?;
    let end = get("DTEND").and_then(|(_, params, value)| When::parse(value, params));
    let length = match (&start, end) {
        (When::Date(from), Some(When::Date(to))) => Length::Days(u64::try_from((to - *from).num_days()).ok()?.max(1)),
        (When::Date(_), _) => Length::Days(get("DURATION").and_then(|(.., v)| duration(v)).map_or(1, |d| d.num_days().max(1) as u64)),
        // Both ends in the same zone, or UTC, so their difference doesn't depend on the page's time zone.
        (_, Some(end)) => Length::Exact(end.resolve(chrono_tz::UTC)? - start.resolve(chrono_tz::UTC)?),
        (_, None) => Length::Exact(get("DURATION").and_then(|(.., v)| duration(v)).unwrap_or_default()),
    };
    let text = |key: &str| get(key).map(|(.., v)| v.trim().to_ascii_uppercase());
    let exdates = props
        .iter()
        .filter(|(name, ..)| name == "EXDATE")
        .flat_map(|(_, params, value)| value.split(',').filter_map(|v| When::parse(v, params)).collect::<Vec<_>>())
        .collect();
    Some(Event {
        uid: get("UID").map(|(.., v)| v.trim().to_string()).unwrap_or_default(),
        start,
        length,
        rrule: get("RRULE").map(|(.., v)| v.trim().to_string()),
        exdates,
        recurrence_id: get("RECURRENCE-ID").and_then(|(_, params, value)| When::parse(value, params)),
        free: text("STATUS").as_deref() == Some("CANCELLED") || text("TRANSP").as_deref() == Some("TRANSPARENT"),
    })
}

/// An iCalendar DURATION like PT30M, P1D or P1W. Negative ones aren't lengths, so they're None.
fn duration(value: &str) -> Option<Duration> {
    let rest = value.trim().strip_prefix('+').unwrap_or(value.trim()).strip_prefix('P')?;
    let (mut total, mut number, mut in_time) = (Duration::zero(), String::new(), false);
    for c in rest.chars() {
        let n = || number.parse::<i64>().ok();
        match c {
            '0'..='9' => {
                number.push(c);
                continue;
            }
            'T' => in_time = true,
            'W' => total += Duration::weeks(n()?),
            'D' => total += Duration::days(n()?),
            'H' if in_time => total += Duration::hours(n()?),
            'M' if in_time => total += Duration::minutes(n()?),
            'S' if in_time => total += Duration::seconds(n()?),
            _ => return None,
        }
        number.clear();
    }
    Some(total)
}

/// The IANA zone for a TZID. Google and Apple use IANA names, some exporters prefix them with a path like
/// /mozilla.org/20050126_1/, and Outlook uses Windows names.
fn zone(tzid: &str) -> Option<Tz> {
    let tzid = tzid.trim().trim_matches('"');
    let parts: Vec<&str> = tzid.split('/').filter(|p| !p.is_empty()).collect();
    (0..parts.len()).find_map(|i| parts[i..].join("/").parse().ok()).or_else(|| windows_zone(tzid))
}

fn windows_zone(name: &str) -> Option<Tz> {
    let iana = match name {
        "UTC" | "Coordinated Universal Time" => "UTC",
        "GMT Standard Time" => "Europe/London",
        "Greenwich Standard Time" => "Atlantic/Reykjavik",
        "W. Europe Standard Time" => "Europe/Berlin",
        "Romance Standard Time" => "Europe/Paris",
        "Central Europe Standard Time" => "Europe/Budapest",
        "Central European Standard Time" => "Europe/Warsaw",
        "E. Europe Standard Time" => "Europe/Chisinau",
        "FLE Standard Time" => "Europe/Kiev",
        "GTB Standard Time" => "Europe/Bucharest",
        "Russian Standard Time" => "Europe/Moscow",
        "Turkey Standard Time" => "Europe/Istanbul",
        "Israel Standard Time" => "Asia/Jerusalem",
        "South Africa Standard Time" => "Africa/Johannesburg",
        "Arabian Standard Time" => "Asia/Dubai",
        "India Standard Time" => "Asia/Kolkata",
        "China Standard Time" => "Asia/Shanghai",
        "Singapore Standard Time" => "Asia/Singapore",
        "Tokyo Standard Time" => "Asia/Tokyo",
        "Korea Standard Time" => "Asia/Seoul",
        "AUS Eastern Standard Time" => "Australia/Sydney",
        "E. Australia Standard Time" => "Australia/Brisbane",
        "W. Australia Standard Time" => "Australia/Perth",
        "New Zealand Standard Time" => "Pacific/Auckland",
        "Eastern Standard Time" => "America/New_York",
        "Central Standard Time" => "America/Chicago",
        "Mountain Standard Time" => "America/Denver",
        "US Mountain Standard Time" => "America/Phoenix",
        "Pacific Standard Time" => "America/Los_Angeles",
        "Alaskan Standard Time" => "America/Anchorage",
        "Hawaiian Standard Time" => "Pacific/Honolulu",
        "Atlantic Standard Time" => "America/Halifax",
        "Canada Central Standard Time" => "America/Regina",
        "Central Standard Time (Mexico)" => "America/Mexico_City",
        "SA Pacific Standard Time" => "America/Bogota",
        "E. South America Standard Time" => "America/Sao_Paulo",
        "Argentina Standard Time" => "America/Buenos_Aires",
        _ => return None,
    };
    iana.parse().ok()
}

/// The busy stretches of `events` that overlap `from`..`to`. `tz` is the page's time zone, for all-day events and times
/// without a zone.
pub fn busy(events: &[Event], tz: Tz, from: Ts, to: Ts) -> Vec<(Ts, Ts)> {
    // Occurrences that a separate VEVENT moved or cancelled, by UID.
    let mut replaced: HashMap<&str, Vec<&When>> = HashMap::new();
    for e in events {
        if let Some(id) = &e.recurrence_id {
            replaced.entry(e.uid.as_str()).or_default().push(id);
        }
    }
    let mut out = Vec::new();
    for e in events.iter().filter(|e| !e.free) {
        let Some(start) = e.start.resolve(tz) else { continue };
        let starts = match (&e.rrule, &e.recurrence_id) {
            (Some(rule), None) => {
                let skip: Vec<&When> = e.exdates.iter().chain(replaced.get(e.uid.as_str()).into_iter().flatten().copied()).collect();
                occurrences(rule, start, &skip, from - longest(&e.length), to).unwrap_or_else(|| vec![start])
            }
            _ => vec![start],
        };
        for s in starts {
            let end = match e.length {
                Length::Exact(d) => s + d,
                Length::Days(n) => s.date_naive().checked_add_days(Days::new(n)).and_then(|d| local(s.timezone(), d.and_hms_opt(0, 0, 0)?)).unwrap_or(s),
            };
            let (s, end) = (s.with_timezone(&Utc), end.with_timezone(&Utc));
            if s < to && end > from && end > s {
                out.push((s, end));
            }
        }
    }
    out.sort();
    out
}

/// An upper bound on an event's length, so occurrences that started before the window but run into it are found.
fn longest(length: &Length) -> Duration {
    match length {
        Length::Exact(d) => *d,
        Length::Days(n) => Duration::days(*n as i64 + 1),
    }
}

/// The starts of a recurring event between `from` and `to`. None when the rule can't be read, and the event then counts
/// once, at its start.
fn occurrences(rule: &str, start: DateTime<Tz>, skip: &[&When], from: Ts, to: Ts) -> Option<Vec<DateTime<Tz>>> {
    let zone = rrule::Tz::Tz(start.timezone());
    // UNTIL is read here rather than by rrule, which wants it in UTC whenever DTSTART has a zone, and calendars also
    // write it as a date or a local time.
    let mut until = None;
    let mut parts = Vec::new();
    for part in rule.split(';').filter(|p| !p.is_empty()) {
        match part.split_once('=') {
            Some((key, value)) if key.eq_ignore_ascii_case("UNTIL") => {
                let end = match When::parse(value, &[])? {
                    When::Date(d) => local(start.timezone(), d.checked_add_days(Days::new(1))?.and_hms_opt(0, 0, 0)?)?.with_timezone(&Utc),
                    when => when.resolve(start.timezone())?.with_timezone(&Utc),
                };
                until = Some(end);
            }
            _ => parts.push(part),
        }
    }
    let mut parsed: rrule::RRule<rrule::Unvalidated> = parts.join(";").parse().ok()?;
    if let Some(until) = until {
        parsed = parsed.until(until.with_timezone(&rrule::Tz::UTC));
    }
    let as_rrule = |t: DateTime<Tz>| t.with_timezone(&zone);
    let exdates: Vec<_> = skip.iter().filter_map(|w| w.resolve(start.timezone())).map(|t| t.with_timezone(&zone)).collect();
    let set = parsed.build(as_rrule(start)).ok()?.set_exdates(exdates).after(from.with_timezone(&zone)).before(to.with_timezone(&zone));
    Some(set.all(MAX_OCCURRENCES).dates.into_iter().map(|t| t.with_timezone(&start.timezone())).collect())
}

/// Tests and local sandboxes serve their calendar from this machine, which public_client refuses. Only a debug build
/// reads the switch, so a release build can't be talked into fetching private addresses.
pub fn private_addresses_allowed() -> bool {
    cfg!(debug_assertions) && std::env::var("BOOKING_PRIVATE_CALENDARS").is_ok_and(|v| v == "true")
}

/// The calendar file at a pasted address. webcal:// is iCloud's way of writing https://. Each redirect hop is checked to
/// be a public address before it's fetched.
pub async fn fetch(raw: &str, public_only: bool) -> anyhow::Result<String> {
    let raw = raw.trim();
    let raw = match raw.get(..9) {
        Some(scheme) if scheme.eq_ignore_ascii_case("webcal://") => format!("https://{}", &raw[9..]),
        _ => raw.to_string(),
    };
    let mut url = Url::parse(&raw).context("that isn't a web address")?;
    for _ in 0..4 {
        let client = if public_only {
            crate::hooks::public_client(&url).await?
        } else {
            reqwest::Client::builder().timeout(StdDuration::from_secs(20)).redirect(reqwest::redirect::Policy::none()).build()?
        };
        let mut res = client.get(url.clone()).send().await.map_err(reqwest::Error::without_url)?;
        if res.status().is_redirection() {
            let location = res.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).context("a redirect without a location")?;
            url = url.join(location)?;
            continue;
        }
        anyhow::ensure!(res.status().is_success(), "the calendar answered {}", res.status());
        let mut body = Vec::new();
        while let Some(chunk) = res.chunk().await.map_err(reqwest::Error::without_url)? {
            body.extend_from_slice(&chunk);
            anyhow::ensure!(body.len() <= MAX_BYTES, "the calendar is larger than {} MB", MAX_BYTES >> 20);
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        anyhow::ensure!(text.trim_start().to_ascii_uppercase().starts_with("BEGIN:VCALENDAR"), "that address doesn't serve a calendar file");
        return Ok(text);
    }
    anyhow::bail!("the calendar redirected too many times")
}

type Cached = (Instant, Arc<Vec<Event>>);
static CACHE: LazyLock<Mutex<HashMap<String, Cached>>> = LazyLock::new(Default::default);

/// The events at a calendar address, fetched at most every few minutes. A failed fetch falls back to the last good copy
/// for up to an hour, and the error comes back with it so the page can say the calendar isn't being read.
pub async fn events(url: &str) -> (Arc<Vec<Event>>, Option<anyhow::Error>) {
    let cached = CACHE.lock().expect("calendar cache lock").get(url).cloned();
    if let Some((at, events)) = &cached
        && at.elapsed() < FRESH_FOR
    {
        return (events.clone(), None);
    }
    match fetch(url, !private_addresses_allowed()).await {
        Ok(ics) => {
            let events = Arc::new(parse(&ics));
            let mut cache = CACHE.lock().expect("calendar cache lock");
            cache.retain(|_, (at, _)| at.elapsed() < STALE_FOR);
            cache.insert(url.to_string(), (Instant::now(), events.clone()));
            (events, None)
        }
        Err(e) => {
            let stale = cached.filter(|(at, _)| at.elapsed() < STALE_FOR).map(|(_, events)| events);
            (stale.unwrap_or_default(), Some(e))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> Ts {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn feed(events: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{events}END:VCALENDAR\r\n")
    }

    const LONDON: Tz = chrono_tz::Europe::London;

    #[test]
    fn reads_one_off_all_day_and_free_events() {
        let ics = feed(concat!(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20261110T100000Z\r\nDTEND:20261110T110000Z\r\nSUMMARY:Standup\r\n",
            "BEGIN:VALARM\r\nTRIGGER:-PT10M\r\nDTSTART:20300101T000000Z\r\nEND:VALARM\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20261111\r\nDTEND;VALUE=DATE:20261112\r\nSUMMARY:Offsite\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:c\r\nDTSTART:20261112T100000Z\r\nDURATION:PT45M\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:d\r\nDTSTART;TZID=\"Europe/Berlin\":20261112T090000\r\nDURATION:PT30M\r\nEND:VEVENT\r\n",
        ));
        let busy = busy(&parse(&ics), LONDON, at("2026-11-09T00:00:00Z"), at("2026-11-20T00:00:00Z"));
        assert_eq!(
            busy,
            [
                (at("2026-11-10T10:00:00Z"), at("2026-11-10T11:00:00Z")),
                // The whole day in the page's time zone.
                (at("2026-11-11T00:00:00Z"), at("2026-11-12T00:00:00Z")),
                (at("2026-11-12T08:00:00Z"), at("2026-11-12T08:30:00Z")),
            ]
        );
    }

    #[test]
    fn expands_weekly_and_daily_rules_with_exdates_and_moved_occurrences() {
        let ics = feed(concat!(
            // Every Monday and Wednesday at 9:00 New York time until the end of November, minus 18 November.
            "BEGIN:VEVENT\r\nUID:weekly\r\nDTSTART;TZID=America/New_York:20261102T090000\r\n",
            "DTEND;TZID=America/New_York:20261102T093000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20261130T235959Z\r\n",
            "EXDATE;TZID=America/New_York:20261118T090000\r\nEND:VEVENT\r\n",
            // The 16 November one moved to 15:00.
            "BEGIN:VEVENT\r\nUID:weekly\r\nRECURRENCE-ID;TZID=America/New_York:20261116T090000\r\n",
            "DTSTART;TZID=America/New_York:20261116T150000\r\nDTEND;TZID=America/New_York:20261116T153000\r\nEND:VEVENT\r\n",
            // A daily lunch until a date, which counts to the end of that day.
            "BEGIN:VEVENT\r\nUID:lunch\r\nDTSTART:20261116T170000Z\r\nDTEND:20261116T180000Z\r\nRRULE:FREQ=DAILY;UNTIL=20261118\r\nEND:VEVENT\r\n",
        ));
        let busy = busy(&parse(&ics), LONDON, at("2026-11-16T00:00:00Z"), at("2026-11-21T00:00:00Z"));
        assert_eq!(
            busy,
            [
                (at("2026-11-16T17:00:00Z"), at("2026-11-16T18:00:00Z")),
                (at("2026-11-16T20:00:00Z"), at("2026-11-16T20:30:00Z")),
                (at("2026-11-17T17:00:00Z"), at("2026-11-17T18:00:00Z")),
                (at("2026-11-18T17:00:00Z"), at("2026-11-18T18:00:00Z")),
            ]
        );
    }

    #[test]
    fn a_cancelled_occurrence_without_its_own_start_is_free() {
        let ics = feed(concat!(
            "BEGIN:VEVENT\r\nUID:sync\r\nDTSTART:20261116T100000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n",
            "BEGIN:VEVENT\r\nUID:sync\r\nRECURRENCE-ID:20261117T100000Z\r\nSTATUS:CANCELLED\r\nEND:VEVENT\r\n",
        ));
        let busy = busy(&parse(&ics), LONDON, at("2026-11-16T00:00:00Z"), at("2026-11-19T00:00:00Z"));
        assert_eq!(busy, [(at("2026-11-16T10:00:00Z"), at("2026-11-16T11:00:00Z"))]);
    }

    #[test]
    fn recurring_times_keep_their_wall_clock_across_a_dst_change() {
        // London's clocks go back on 25 October 2026, so 9:00 moves from 08:00 to 09:00 UTC.
        let ics = feed(
            "BEGIN:VEVENT\r\nUID:x\r\nDTSTART;TZID=GMT Standard Time:20261019T090000\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n",
        );
        let busy = busy(&parse(&ics), chrono_tz::UTC, at("2026-10-19T00:00:00Z"), at("2026-10-27T00:00:00Z"));
        assert_eq!(busy, [(at("2026-10-19T08:00:00Z"), at("2026-10-19T09:00:00Z")), (at("2026-10-26T09:00:00Z"), at("2026-10-26T10:00:00Z"))]);
    }

    #[test]
    fn reads_folded_lines_and_odd_tzids() {
        let lines = content_lines("SUMMARY:Long\r\n  line\r\nDTSTART;TZID=\"/mozilla.org/20050126_1/Europe/Paris\":20261110T100000\r\n");
        assert_eq!(lines[0].2, "Long line");
        assert_eq!(When::parse(&lines[1].2, &lines[1].1), Some(When::Local("2026-11-10T10:00:00".parse().unwrap(), Some(chrono_tz::Europe::Paris))));
        assert_eq!(duration("P1W2DT3H4M"), Some(Duration::days(9) + Duration::hours(3) + Duration::minutes(4)));
        assert_eq!(duration("-PT5M"), None);
    }

    #[tokio::test]
    async fn fetches_a_calendar_and_follows_redirects() {
        use axum::response::Redirect;
        use axum::routing::get;
        let app = axum::Router::new()
            .route("/old.ics", get(|| async { Redirect::permanent("/cal.ics") }))
            .route("/cal.ics", get(|| async { "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n" }))
            .route("/page", get(|| async { "<html></html>" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        assert!(fetch(&format!("{base}/old.ics"), false).await.unwrap().starts_with("BEGIN:VCALENDAR"));
        assert!(fetch(&format!("{base}/page"), false).await.is_err());
        // The fake calendar is on 127.0.0.1, like an internal service a pasted address could point at.
        assert!(fetch(&format!("{base}/cal.ics"), true).await.is_err());
    }
}
