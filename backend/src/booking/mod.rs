//! Booking pages, for a team without Calendly or Cal.com. Each mailbox can have one at /book/{slug}: its working hours,
//! minus the owner's calendar (calendar.rs) and the calls already booked, are the times a visitor can pick. A booking
//! emails both sides an invite from the mailbox (invite.rs), and a lead with that email in one of the org's projects
//! moves to Meeting booked through the same code as the scheduling tools' webhooks (routes::bookings::mark_booked).

pub mod calendar;
pub mod invite;

use chrono::{Datelike, Days, Duration, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use uuid::Uuid;

use self::invite::Change;
use crate::crypto::{decrypt, random_token, sha256_hex};
use crate::error::{AppError, AppResult};
use crate::models::{Booking, BookingPage, Lead, Ts};
use crate::routes::bookings::{mark_booked, mark_cancelled};
use crate::state::AppState;

pub const TAKEN: &str = "Someone just booked that time. Pick another one.";
/// Bookings one address can make in an hour, so a script can't fill someone's week.
const BOOKINGS_PER_HOUR_PER_IP: i64 = 5;

/// What decides a page's open times.
#[derive(Debug, Clone)]
pub struct Rules {
    pub minutes: i64,
    pub tz: Tz,
    /// Monday first: None for a day off, else (start, end) in minutes after midnight.
    pub hours: [Option<(i64, i64)>; 7],
    pub buffer: i64,
    pub notice: i64,
    pub days_ahead: i64,
}

impl BookingPage {
    pub fn tz(&self) -> Tz {
        self.timezone.parse().unwrap_or(chrono_tz::UTC)
    }

    pub fn rules(&self) -> Rules {
        let mut hours = [None; 7];
        for (day, range) in hours.iter_mut().zip(self.hours.iter()) {
            *day = range.map(|[from, to]| (i64::from(from), i64::from(to)));
        }
        Rules {
            minutes: self.duration_minutes.into(),
            tz: self.tz(),
            hours,
            buffer: self.buffer_minutes.into(),
            notice: self.notice_minutes.into(),
            days_ahead: self.days_ahead.into(),
        }
    }

    pub fn url(&self, public_url: &str) -> String {
        format!("{public_url}/book/{}", self.slug)
    }
}

/// The starts of the free slots from `now` on, in order. Slots start every 15 minutes for 15-minute calls and every 30
/// otherwise, within the day's working hours in the page's time zone. A slot needs `buffer` minutes clear of anything in
/// `busy` on both sides, and starts at least `notice` minutes from now.
pub fn slots(rules: &Rules, now: Ts, busy: &[(Ts, Ts)]) -> Vec<Ts> {
    let step = if rules.minutes == 15 { 15 } else { 30 };
    let earliest = now + Duration::minutes(rules.notice);
    let today = now.with_timezone(&rules.tz).date_naive();
    let mut out = Vec::new();
    for day in (0..rules.days_ahead).filter_map(|d| today.checked_add_days(Days::new(d as u64))) {
        let Some((open, close)) = rules.hours[day.weekday().num_days_from_monday() as usize] else { continue };
        let midnight = day.and_time(NaiveTime::MIN);
        for minute in (open..=close - rules.minutes).step_by(step as usize) {
            // A time the clocks skip doesn't exist, so it's no slot. One they repeat is the first of the two.
            let Some(start) = rules.tz.from_local_datetime(&(midnight + Duration::minutes(minute))).earliest() else { continue };
            let start = start.with_timezone(&Utc);
            if start >= earliest && free(rules, start, busy) {
                out.push(start);
            }
        }
    }
    out
}

fn free(rules: &Rules, start: Ts, busy: &[(Ts, Ts)]) -> bool {
    let (from, to) = (start - Duration::minutes(rules.buffer), start + Duration::minutes(rules.minutes + rules.buffer));
    busy.iter().all(|(b0, b1)| *b1 <= from || *b0 >= to)
}

/// The page's booked calls and its owner's calendar from now until its last day, leaving out booking `except`, which is
/// being moved. A calendar that can't be read is noted on the page for its settings, and its last good copy, if any, used.
pub async fn busy(state: &AppState, page: &BookingPage, now: Ts, except: Option<Uuid>) -> AppResult<Vec<(Ts, Ts)>> {
    let to = now + Duration::days(i64::from(page.days_ahead) + 2);
    let mut busy: Vec<(Ts, Ts)> = sqlx::query_as(
        "select starts_at, ends_at from bookings
         where page_id = $1 and cancelled_at is null and id is distinct from $2 and ends_at > $3 and starts_at < $4",
    )
    .bind(page.id)
    .bind(except)
    .bind(now - Duration::days(1))
    .bind(to)
    .fetch_all(&state.db)
    .await?;
    if page.calendar_url_enc.is_empty() {
        return Ok(busy);
    }
    let url = decrypt(&state.cfg.app_secret, &page.calendar_url_enc)?;
    let (events, error) = calendar::events(&url).await;
    busy.extend(calendar::busy(&events, page.tz(), now - Duration::days(1), to));
    let error = error.map(|e| format!("{e:#}")).unwrap_or_default();
    if error != page.calendar_error {
        if !error.is_empty() {
            tracing::warn!(page = %page.id, error, "couldn't read a booking page's calendar");
        }
        sqlx::query("update booking_pages set calendar_error = $2 where id = $1").bind(page.id).bind(&error).execute(&state.db).await?;
    }
    Ok(busy)
}

pub async fn open_slots(state: &AppState, page: &BookingPage, except: Option<Uuid>) -> AppResult<Vec<Ts>> {
    let now = Utc::now();
    Ok(slots(&page.rules(), now, &busy(state, page, now, except).await?))
}

/// The person booking, as checked from the form.
#[derive(Debug, Clone)]
pub struct Guest {
    pub name: String,
    pub email: String,
    pub note: String,
    pub timezone: String,
}

impl Guest {
    pub fn new(name: &str, email: &str, note: &str, timezone: &str) -> AppResult<Guest> {
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if name.is_empty() || name.chars().count() > 200 {
            return Err(AppError::BadRequest("Enter your name".into()));
        }
        let email = crate::routes::util_input::email(email).ok_or_else(|| AppError::BadRequest("Enter a valid email address".into()))?;
        if note.chars().count() > 2000 {
            return Err(AppError::BadRequest("Keep the note under 2,000 characters".into()));
        }
        // An unknown zone only changes how the times in their email read, so it falls back instead of failing.
        let timezone = timezone.parse::<Tz>().map_or_else(|_| "UTC".to_string(), |tz| tz.name().to_string());
        Ok(Guest { name, email, note: note.trim().to_string(), timezone })
    }
}

/// Takes the page's lock for the rest of `tx`, so bookings and moves on one page happen one at a time, then checks that
/// nothing booked overlaps `start` with the buffer around it.
async fn claim(tx: &mut sqlx::PgConnection, page: &BookingPage, start: Ts, except: Option<Uuid>) -> AppResult<()> {
    sqlx::query("select pg_advisory_xact_lock(hashtext('booking:' || $1::text))").bind(page.id).execute(&mut *tx).await?;
    let rules = page.rules();
    let taken: bool = sqlx::query_scalar(
        "select exists(select 1 from bookings where page_id = $1 and cancelled_at is null and id is distinct from $2
                         and starts_at < $4 and ends_at > $3)",
    )
    .bind(page.id)
    .bind(except)
    .bind(start - Duration::minutes(rules.buffer))
    .bind(start + Duration::minutes(rules.minutes + rules.buffer))
    .fetch_one(&mut *tx)
    .await?;
    if taken {
        return Err(AppError::Conflict(TAKEN.into()));
    }
    Ok(())
}

/// The unique index on (page, start) is the last line against a double booking, if two requests ever skip the lock.
fn taken_if_duplicate(e: sqlx::Error) -> AppError {
    match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => AppError::Conflict(TAKEN.into()),
        _ => e.into(),
    }
}

async fn check_open(state: &AppState, page: &BookingPage, start: Ts, except: Option<Uuid>) -> AppResult<()> {
    if !open_slots(state, page, except).await?.contains(&start) {
        return Err(AppError::Conflict(TAKEN.into()));
    }
    Ok(())
}

/// Books `start` for `guest`, emails both sides and marks a matching lead. Returns the booking and the secret token in
/// its cancel and reschedule links, which is only stored hashed.
pub async fn book(state: &AppState, page: &BookingPage, start: Ts, guest: &Guest, ip: &str) -> AppResult<(Booking, String)> {
    check_open(state, page, start, None).await?;
    let token = random_token();
    let mut tx = state.db.begin().await?;
    // Counted under a lock per address, so several bookings sent at once can't all see room for one more.
    sqlx::query("select pg_advisory_xact_lock(hashtextextended('booking-ip:' || $1, 0))").bind(ip).execute(&mut *tx).await?;
    let recent: i64 = sqlx::query_scalar("select count(*) from bookings where ip = $1 and created_at > now() - interval '1 hour'")
        .bind(ip)
        .fetch_one(&mut *tx)
        .await?;
    if recent >= BOOKINGS_PER_HOUR_PER_IP {
        return Err(AppError::TooManyRequests("You've booked a lot of calls in the last hour. Try again later.".into()));
    }
    claim(&mut tx, page, start, None).await?;
    let booking: Booking = sqlx::query_as(
        "insert into bookings (page_id, starts_at, ends_at, name, email, note, timezone, token_hash, ip)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9) returning *",
    )
    .bind(page.id)
    .bind(start)
    .bind(start + Duration::minutes(page.duration_minutes.into()))
    .bind(&guest.name)
    .bind(&guest.email)
    .bind(&guest.note)
    .bind(&guest.timezone)
    .bind(sha256_hex(&token))
    .bind(ip)
    .fetch_one(&mut *tx)
    .await
    .map_err(taken_if_duplicate)?;
    tx.commit().await?;
    after_change(state, page, &booking, &token, Change::Booked).await;
    Ok((booking, token))
}

/// The booking behind a link's token.
pub async fn by_token(db: &sqlx::PgPool, token: &str) -> AppResult<(Booking, BookingPage)> {
    let booking: Booking =
        sqlx::query_as("select * from bookings where token_hash = $1").bind(sha256_hex(token)).fetch_optional(db).await?.ok_or(AppError::NotFound)?;
    let page: BookingPage = sqlx::query_as("select * from booking_pages where id = $1").bind(booking.page_id).fetch_one(db).await?;
    Ok((booking, page))
}

fn changeable(booking: &Booking) -> AppResult<()> {
    if booking.cancelled_at.is_some() {
        return Err(AppError::Conflict("This call was cancelled.".into()));
    }
    if booking.starts_at <= Utc::now() {
        return Err(AppError::Conflict("This call has already started.".into()));
    }
    Ok(())
}

/// Moves a booking to `start`. The invite keeps its UID with a higher SEQUENCE, so calendars move the event.
pub async fn reschedule(state: &AppState, token: &str, start: Ts) -> AppResult<Booking> {
    let (booking, page) = by_token(&state.db, token).await?;
    changeable(&booking)?;
    if start == booking.starts_at {
        return Ok(booking);
    }
    check_open(state, &page, start, Some(booking.id)).await?;
    let mut tx = state.db.begin().await?;
    claim(&mut tx, &page, start, Some(booking.id)).await?;
    let moved: Booking = sqlx::query_as(
        "update bookings set starts_at = $2, ends_at = $3, sequence = sequence + 1 where id = $1 and cancelled_at is null returning *",
    )
    .bind(booking.id)
    .bind(start)
    .bind(start + Duration::minutes(page.duration_minutes.into()))
    .fetch_optional(&mut *tx)
    .await
    .map_err(taken_if_duplicate)?
    .ok_or_else(|| AppError::Conflict("This call was cancelled.".into()))?;
    tx.commit().await?;
    after_change(state, &page, &moved, token, Change::Moved).await;
    Ok(moved)
}

pub async fn cancel(state: &AppState, token: &str) -> AppResult<Booking> {
    let (booking, page) = by_token(&state.db, token).await?;
    changeable(&booking)?;
    // Only the request that cancels it sends the emails, if someone clicks twice.
    let cancelled: Option<Booking> = sqlx::query_as(
        "update bookings set cancelled_at = now(), sequence = sequence + 1 where id = $1 and cancelled_at is null returning *",
    )
    .bind(booking.id)
    .fetch_optional(&state.db)
    .await?;
    let Some(cancelled) = cancelled else { return Ok(booking) };
    after_change(state, &page, &cancelled, token, Change::Cancelled).await;
    Ok(cancelled)
}

/// Emails both sides and moves the guest's leads once the change is saved. A failure is only logged, since the booking
/// already stands and the visitor should see that it does.
async fn after_change(state: &AppState, page: &BookingPage, booking: &Booking, token: &str, change: Change) {
    invite::send(state, page, booking, token, change);
    let leads = async {
        let leads = org_leads(&state.db, page.org_id, &booking.email).await?;
        match change {
            Change::Booked => mark_booked(state, &leads, Some(booking.starts_at), false).await,
            Change::Moved => mark_booked(state, &leads, Some(booking.starts_at), true).await,
            Change::Cancelled => {
                // A lead with a call at another time, booked some other way, keeps it.
                let leads: Vec<Lead> = leads.into_iter().filter(|l| l.meeting_at.is_none_or(|at| at == booking.starts_at)).collect();
                mark_cancelled(state, &leads).await.map(drop)
            }
        }
    };
    if let Err(e) = leads.await {
        tracing::error!(error = %e, booking = %booking.id, "couldn't update the leads of a booking");
    }
}

/// The leads with this email in any of the org's projects.
pub async fn org_leads(db: &sqlx::PgPool, org_id: Uuid, email: &str) -> sqlx::Result<Vec<Lead>> {
    sqlx::query_as("select l.* from leads l join projects p on p.id = l.project_id where p.org_id = $1 and l.email = $2::citext")
        .bind(org_id)
        .bind(email)
        .fetch_all(db)
        .await
}

#[cfg(test)]
mod tests;
