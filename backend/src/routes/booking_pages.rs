//! Booking pages: the settings the team edits, and the public routes behind /book/{slug} and the links in the booking
//! emails (/booking/{token}). See crate::booking.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use chrono::Utc;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use super::demo::client_ip;
use crate::auth::Auth;
use crate::booking::{self, Guest, calendar};
use crate::crypto::encrypt;
use crate::error::{AppError, AppResult};
use crate::models::{Booking, BookingPage, Mailbox, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/booking-pages", get(list).post(create))
        .route("/booking-pages/{id}", patch(update).delete(remove))
        .route("/book/{slug}", get(show).post(book))
        .route("/booking/{token}", get(manage))
        .route("/booking/{token}/cancel", post(cancel))
        .route("/booking/{token}/move", post(reschedule))
}

/// A page as its settings show it.
#[derive(Serialize)]
struct PageView {
    #[serde(flatten)]
    page: BookingPage,
    url: String,
    mailbox_email: String,
    host: String,
    /// The saved calendar address's host, like calendar.google.com, so the settings can say one is set without the secret.
    calendar_host: String,
    upcoming: Vec<Booking>,
}

async fn views(state: &AppState, org_id: Uuid, only: Option<Uuid>) -> AppResult<Vec<PageView>> {
    let pages: Vec<BookingPage> = sqlx::query_as("select * from booking_pages where org_id = $1 and ($2::uuid is null or id = $2) order by created_at")
        .bind(org_id)
        .bind(only)
        .fetch_all(&state.db)
        .await?;
    let mailboxes: Vec<Mailbox> = sqlx::query_as("select * from mailboxes where org_id = $1").bind(org_id).fetch_all(&state.db).await?;
    let ids: Vec<Uuid> = pages.iter().map(|p| p.id).collect();
    let upcoming: Vec<Booking> = sqlx::query_as(
        "select * from bookings where page_id = any($1) and cancelled_at is null and ends_at > now() order by starts_at limit 500",
    )
    .bind(&ids)
    .fetch_all(&state.db)
    .await?;
    let mut views = Vec::new();
    for page in pages {
        let mb = mailboxes.iter().find(|m| m.id == page.mailbox_id);
        let calendar_url = crate::crypto::decrypt(&state.cfg.app_secret, &page.calendar_url_enc).unwrap_or_default();
        views.push(PageView {
            url: page.url(&state.cfg.public_url),
            mailbox_email: mb.map(|m| m.email.clone()).unwrap_or_default(),
            host: mb.map(host_name).unwrap_or_default(),
            calendar_host: calendar_url.replace("webcal://", "https://").parse::<Url>().ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_default(),
            upcoming: upcoming.iter().filter(|b| b.page_id == page.id).cloned().collect(),
            page,
        });
    }
    Ok(views)
}

fn host_name(mb: &Mailbox) -> String {
    if mb.from_name.trim().is_empty() { mb.email.clone() } else { mb.from_name.trim().to_string() }
}

async fn one(state: &AppState, org_id: Uuid, id: Uuid) -> AppResult<PageView> {
    views(state, org_id, Some(id)).await?.pop().ok_or(AppError::NotFound)
}

async fn list(State(state): State<AppState>, auth: Auth) -> AppResult<Json<Vec<PageView>>> {
    Ok(Json(views(&state, auth.org_id, None).await?))
}

#[derive(Deserialize)]
struct CreateBody {
    mailbox_id: Uuid,
}

/// "Alex Morgan" or alex.morgan@acme.com becomes alex-morgan.
fn slug_from(text: &str) -> String {
    let mut slug = String::new();
    for c in text.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(40).collect();
    let slug = slug.trim_end_matches('-');
    if slug.len() < 2 { "meet".to_string() } else { slug.to_string() }
}

/// The mailbox's page, made with the defaults when it has none, so the mailbox form can fill in its link in one step.
async fn create(State(state): State<AppState>, auth: Auth, Json(body): Json<CreateBody>) -> AppResult<Json<PageView>> {
    let mb: Mailbox = sqlx::query_as("select * from mailboxes where id = $1 and org_id = $2")
        .bind(body.mailbox_id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let existing: Option<Uuid> = sqlx::query_scalar("select id from booking_pages where mailbox_id = $1").bind(mb.id).fetch_optional(&state.db).await?;
    if let Some(id) = existing {
        return Ok(Json(one(&state, auth.org_id, id).await?));
    }
    let base = slug_from(if mb.from_name.trim().is_empty() { mb.email.split('@').next().unwrap_or_default() } else { &mb.from_name });
    let mut candidates: Vec<String> = std::iter::once(base.clone()).chain((2..10).map(|n| format!("{base}-{n}"))).collect();
    candidates.push(format!("{base}-{}", &Uuid::new_v4().simple().to_string()[..6]));
    for slug in candidates {
        let id: Option<Uuid> = sqlx::query_scalar("insert into booking_pages (org_id, mailbox_id, slug) values ($1, $2, $3) on conflict (slug) do nothing returning id")
            .bind(auth.org_id)
            .bind(mb.id)
            .bind(&slug)
            .fetch_optional(&state.db)
            .await?;
        if let Some(id) = id {
            return Ok(Json(one(&state, auth.org_id, id).await?));
        }
    }
    Err(AppError::Conflict("Couldn't find a free link for this page. Try again.".into()))
}

#[derive(Deserialize)]
struct PageBody {
    slug: Option<String>,
    name: Option<String>,
    duration_minutes: Option<i32>,
    timezone: Option<String>,
    hours: Option<Vec<Option<[i32; 2]>>>,
    buffer_minutes: Option<i32>,
    notice_minutes: Option<i32>,
    days_ahead: Option<i32>,
    location: Option<String>,
    reminder_hours: Option<i32>,
    /// Empty removes the saved address. Left out keeps it.
    calendar_url: Option<String>,
}

impl PageBody {
    fn validate(&mut self) -> AppResult<()> {
        let bad = |message: &str| Err(AppError::BadRequest(message.into()));
        if let Some(slug) = &mut self.slug {
            *slug = slug.trim().to_lowercase();
            let valid = (2..=48).contains(&slug.len())
                && slug.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                && !slug.starts_with('-');
            if !valid {
                return bad("Use 2 to 48 lowercase letters, numbers and dashes for the link");
            }
        }
        if let Some(name) = &mut self.name {
            *name = name.trim().to_string();
            if name.is_empty() || name.chars().count() > 80 {
                return bad("Give the call a name of up to 80 characters");
            }
        }
        if self.duration_minutes.is_some_and(|m| ![15, 30, 45, 60].contains(&m)) {
            return bad("Calls last 15, 30, 45 or 60 minutes");
        }
        if self.timezone.as_deref().is_some_and(|tz| tz.parse::<chrono_tz::Tz>().is_err()) {
            return bad("Pick a time zone from the list");
        }
        if let Some(hours) = &self.hours
            && (hours.len() != 7 || hours.iter().flatten().any(|[from, to]| !(0 <= *from && from < to && *to <= 1440)))
        {
            return bad("Each working day needs a start before its end");
        }
        if self.buffer_minutes.is_some_and(|m| !(0..=120).contains(&m)) {
            return bad("Keep the buffer between 0 and 120 minutes");
        }
        if self.notice_minutes.is_some_and(|m| !(0..=20160).contains(&m)) {
            return bad("Keep the minimum notice under 14 days");
        }
        if self.days_ahead.is_some_and(|d| !(1..=90).contains(&d)) {
            return bad("Show between 1 and 90 days ahead");
        }
        if self.reminder_hours.is_some_and(|h| !(0..=168).contains(&h)) {
            return bad("Send the reminder at most a week before the call");
        }
        if let Some(location) = &mut self.location {
            *location = location.trim().to_string();
            if location.chars().count() > 500 {
                return bad("Keep the location under 500 characters");
            }
        }
        Ok(())
    }
}

async fn update(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>, Json(mut body): Json<PageBody>) -> AppResult<Json<PageView>> {
    body.validate()?;
    // A new calendar address is read once now, so a wrong one is caught here and not on the public page.
    let calendar = match body.calendar_url.as_deref().map(str::trim) {
        Some("") => Some(String::new()),
        Some(url) => {
            let ics = calendar::fetch(url, !calendar::private_addresses_allowed())
                .await
                .map_err(|e| AppError::BadRequest(format!("Couldn't read that calendar: {e:#}")))?;
            tracing::info!(events = calendar::parse(&ics).len(), "read a booking page's calendar");
            Some(encrypt(&state.cfg.app_secret, url))
        }
        None => None,
    };
    let updated = sqlx::query(
        "update booking_pages set
           slug = coalesce($3, slug), name = coalesce($4, name), duration_minutes = coalesce($5, duration_minutes),
           timezone = coalesce($6, timezone), hours = coalesce($7, hours), buffer_minutes = coalesce($8, buffer_minutes),
           notice_minutes = coalesce($9, notice_minutes), days_ahead = coalesce($10, days_ahead), location = coalesce($11, location),
           calendar_url_enc = coalesce($12, calendar_url_enc), calendar_error = case when $12 is null then calendar_error else '' end,
           reminder_hours = coalesce($13, reminder_hours)
         where id = $1 and org_id = $2",
    )
    .bind(id)
    .bind(auth.org_id)
    .bind(&body.slug)
    .bind(&body.name)
    .bind(body.duration_minutes)
    .bind(&body.timezone)
    .bind(body.hours.map(sqlx::types::Json))
    .bind(body.buffer_minutes)
    .bind(body.notice_minutes)
    .bind(body.days_ahead)
    .bind(&body.location)
    .bind(&calendar)
    .bind(body.reminder_hours)
    .execute(&state.db)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23505") => AppError::Conflict("That link is taken. Try another.".into()),
        _ => e.into(),
    })?;
    if updated.rows_affected() == 0 {
        return Err(AppError::NotFound);
    }
    Ok(Json(one(&state, auth.org_id, id).await?))
}

/// A mailbox whose booking link was this page drops it, so emails don't send leads to a page that's gone.
async fn remove(State(state): State<AppState>, auth: Auth, Path(id): Path<Uuid>) -> AppResult<Json<Value>> {
    let page: BookingPage = sqlx::query_as("delete from booking_pages where id = $1 and org_id = $2 returning *")
        .bind(id)
        .bind(auth.org_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    sqlx::query("update mailboxes set booking_link = '' where id = $1 and booking_link = $2")
        .bind(page.mailbox_id)
        .bind(page.url(&state.cfg.public_url))
        .execute(&state.db)
        .await?;
    Ok(Json(json!({ "ok": true })))
}

const PUBLIC_PER_MINUTE: u32 = 60;
static HITS: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> = LazyLock::new(Default::default);

/// Requests a minute from one address to the public booking routes, kept in memory: plenty for someone clicking through
/// the weeks, too few for a script walking slugs or tokens. Bookings have their own hourly limit in the database.
fn limit(headers: &HeaderMap, peer: SocketAddr) -> AppResult<()> {
    let ip = client_ip(headers, peer);
    let mut hits = HITS.lock().expect("rate limit lock");
    if hits.len() > 10_000 {
        hits.retain(|_, (since, _)| since.elapsed() < Duration::from_secs(60));
    }
    let (since, count) = hits.entry(ip).or_insert((Instant::now(), 0));
    if since.elapsed() >= Duration::from_secs(60) {
        (*since, *count) = (Instant::now(), 0);
    }
    *count += 1;
    if *count > PUBLIC_PER_MINUTE {
        return Err(AppError::TooManyRequests("Too many requests. Try again in a minute.".into()));
    }
    Ok(())
}

/// What a visitor sees of a page. A video link stays in the invite, so the page only says there is one.
#[derive(Serialize)]
struct PublicPage {
    slug: String,
    name: String,
    host: String,
    duration_minutes: i32,
    location: String,
    slots: Vec<Ts>,
}

async fn public_page(state: &AppState, page: &BookingPage, except: Option<Uuid>, with_slots: bool) -> AppResult<PublicPage> {
    let mb: Mailbox = sqlx::query_as("select * from mailboxes where id = $1").bind(page.mailbox_id).fetch_one(&state.db).await?;
    let location = if page.location.starts_with("http://") || page.location.starts_with("https://") { "Video call".into() } else { page.location.clone() };
    Ok(PublicPage {
        slug: page.slug.clone(),
        name: page.name.clone(),
        host: host_name(&mb),
        duration_minutes: page.duration_minutes,
        location,
        slots: if with_slots { booking::open_slots(state, page, except).await? } else { Vec::new() },
    })
}

async fn by_slug(state: &AppState, slug: &str) -> AppResult<BookingPage> {
    sqlx::query_as("select * from booking_pages where slug = $1").bind(slug.to_lowercase()).fetch_optional(&state.db).await?.ok_or(AppError::NotFound)
}

async fn show(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
) -> AppResult<Json<PublicPage>> {
    limit(&headers, peer)?;
    let page = by_slug(&state, &slug).await?;
    Ok(Json(public_page(&state, &page, None, true).await?))
}

#[derive(Deserialize)]
struct BookBody {
    start: Ts,
    name: String,
    email: String,
    #[serde(default)]
    note: String,
    #[serde(default)]
    timezone: String,
}

/// The booking as its guest sees it. `token` is in the links of their email, and the confirmation links to it too.
#[derive(Serialize)]
struct Confirmed {
    starts_at: Ts,
    ends_at: Ts,
    email: String,
    token: String,
}

async fn book(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(slug): Path<String>,
    Json(body): Json<BookBody>,
) -> AppResult<Json<Confirmed>> {
    limit(&headers, peer)?;
    let page = by_slug(&state, &slug).await?;
    let guest = Guest::new(&body.name, &body.email, &body.note, &body.timezone)?;
    let (booking, token) = booking::book(&state, &page, body.start, &guest, &client_ip(&headers, peer)).await?;
    Ok(Json(Confirmed { starts_at: booking.starts_at, ends_at: booking.ends_at, email: booking.email, token }))
}

#[derive(Serialize)]
struct Managed {
    page: PublicPage,
    name: String,
    starts_at: Ts,
    ends_at: Ts,
    cancelled: bool,
    started: bool,
}

async fn managed(state: &AppState, booking: Booking, page: &BookingPage) -> AppResult<Managed> {
    let (cancelled, started) = (booking.cancelled_at.is_some(), booking.starts_at <= Utc::now());
    Ok(Managed {
        page: public_page(state, page, Some(booking.id), !cancelled && !started).await?,
        name: booking.name,
        starts_at: booking.starts_at,
        ends_at: booking.ends_at,
        cancelled,
        started,
    })
}

/// Only shows the booking. Mail scanners open the links in emails, so cancelling and moving take a button press.
async fn manage(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<Json<Managed>> {
    limit(&headers, peer)?;
    let (booking, page) = booking::by_token(&state.db, &token).await?;
    Ok(Json(managed(&state, booking, &page).await?))
}

async fn cancel(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
) -> AppResult<Json<Managed>> {
    limit(&headers, peer)?;
    let booking = booking::cancel(&state, &token).await?;
    let (_, page) = booking::by_token(&state.db, &token).await?;
    Ok(Json(managed(&state, booking, &page).await?))
}

#[derive(Deserialize)]
struct MoveBody {
    start: Ts,
}

async fn reschedule(
    State(state): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Path(token): Path<String>,
    Json(body): Json<MoveBody>,
) -> AppResult<Json<Managed>> {
    limit(&headers, peer)?;
    let booking = booking::reschedule(&state, &token, body.start).await?;
    let (_, page) = booking::by_token(&state.db, &token).await?;
    Ok(Json(managed(&state, booking, &page).await?))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use sqlx::PgPool;

    use super::*;
    use crate::config::Config;

    fn state(db: PgPool) -> AppState {
        let cfg = Config { app_secret: "0123456789abcdef0123456789abcdef".into(), public_url: "https://app.test".into(), ..Config::test() };
        AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0, rate_limits: Default::default() }
    }

    fn owner(org_id: Uuid) -> Auth {
        Auth { user_id: Uuid::new_v4(), org_id, role: "owner".into(), token_hash: String::new() }
    }

    async fn org(db: &PgPool, name: &str) -> Uuid {
        sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap()
    }

    async fn mailbox(db: &PgPool, org_id: Uuid, email: &str, from_name: &str) -> Uuid {
        sqlx::query_scalar(
            "insert into mailboxes (org_id, email, from_name, smtp_host, smtp_username, smtp_password_enc)
             values ($1, $2, $3, 'smtp.test', '', '') returning id",
        )
        .bind(org_id)
        .bind(email)
        .bind(from_name)
        .fetch_one(db)
        .await
        .unwrap()
    }

    async fn create_page(state: &AppState, org_id: Uuid, mailbox_id: Uuid) -> AppResult<PageView> {
        create(State(state.clone()), owner(org_id), Json(CreateBody { mailbox_id })).await.map(|Json(view)| view)
    }

    async fn edit(state: &AppState, org_id: Uuid, id: Uuid, body: Value) -> AppResult<PageView> {
        let body: PageBody = serde_json::from_value(body).unwrap();
        update(State(state.clone()), owner(org_id), Path(id), Json(body)).await.map(|Json(view)| view)
    }

    async fn saved(db: &PgPool, id: Uuid) -> BookingPage {
        sqlx::query_as("select * from booking_pages where id = $1").bind(id).fetch_one(db).await.unwrap()
    }

    /// Each test uses its own address, since the public routes' rate limit is shared by the whole test binary.
    fn visitor(ip: &str) -> (ConnectInfo<SocketAddr>, HeaderMap) {
        (ConnectInfo(format!("{ip}:443").parse().unwrap()), HeaderMap::new())
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn a_mailbox_gets_one_page_on_a_free_link_made_from_its_name(db: PgPool) {
        let state = state(db.clone());
        let (acme, other) = (org(&db, "Acme").await, org(&db, "Other").await);
        let alex = mailbox(&db, acme, "alex@acme.example", " Alex Morgan ").await;

        let page = create_page(&state, acme, alex).await.unwrap();
        assert_eq!(page.page.slug, "alex-morgan");
        assert_eq!(page.page.name, "Intro call");
        assert_eq!(page.url, "https://app.test/book/alex-morgan");
        assert_eq!((page.host.as_str(), page.mailbox_email.as_str()), ("Alex Morgan", "alex@acme.example"));
        assert_eq!(page.calendar_host, "");
        assert_eq!(create_page(&state, acme, alex).await.unwrap().page.id, page.page.id, "a second press returns the same page");

        // A colleague with the same name gets the next free link, and one with no name gets theirs from the address.
        let namesake = mailbox(&db, acme, "alex.m@acme.example", "Alex Morgan").await;
        assert_eq!(create_page(&state, acme, namesake).await.unwrap().page.slug, "alex-morgan-2");
        let sam = mailbox(&db, acme, "sam.lee@acme.example", "").await;
        let sams = create_page(&state, acme, sam).await.unwrap();
        assert_eq!((sams.page.slug.as_str(), sams.host.as_str()), ("sam-lee", "sam.lee@acme.example"));

        // Links are shared by every org, so alex-morgan to alex-morgan-9 taken by another org leaves a random one.
        sqlx::query("update booking_pages set slug = 'acme-' || slug where org_id = $1").bind(acme).execute(&db).await.unwrap();
        for n in 1..10 {
            let slug = if n == 1 { "alex-morgan".to_string() } else { format!("alex-morgan-{n}") };
            let mb = mailbox(&db, other, &format!("alex{n}@other.example"), "").await;
            sqlx::query("insert into booking_pages (org_id, mailbox_id, slug) values ($1, $2, $3)").bind(other).bind(mb).bind(slug).execute(&db).await.unwrap();
        }
        let third = mailbox(&db, acme, "a.morgan@acme.example", "Alex Morgan").await;
        let random = create_page(&state, acme, third).await.unwrap().page.slug;
        let suffix = random.strip_prefix("alex-morgan-").unwrap_or_default();
        assert!(suffix.len() == 6 && suffix.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)), "{random}");

        // Another org's mailbox isn't found, and the list only has the org's own pages, oldest first.
        assert!(matches!(create_page(&state, other, alex).await, Err(AppError::NotFound)));
        for (email, age) in [("sam.lee@acme.example", 4), ("alex@acme.example", 3), ("a.morgan@acme.example", 2), ("alex.m@acme.example", 1)] {
            sqlx::query("update booking_pages set created_at = now() - make_interval(days => $2) where mailbox_id = (select id from mailboxes where email = $1)")
                .bind(email)
                .bind(age)
                .execute(&db)
                .await
                .unwrap();
        }
        let Json(listed) = list(State(state.clone()), owner(acme)).await.unwrap();
        assert_eq!(
            listed.iter().map(|v| v.mailbox_email.as_str()).collect::<Vec<_>>(),
            ["sam.lee@acme.example", "alex@acme.example", "a.morgan@acme.example", "alex.m@acme.example"]
        );
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn editing_a_page_refuses_bad_values_and_a_taken_link_and_saves_nothing(db: PgPool) {
        let state = state(db.clone());
        let (acme, other) = (org(&db, "Acme").await, org(&db, "Other").await);
        let id = create_page(&state, acme, mailbox(&db, acme, "alex@acme.example", "Alex Morgan").await).await.unwrap().page.id;
        create_page(&state, acme, mailbox(&db, acme, "sam@acme.example", "Sam Lee").await).await.unwrap();
        let calendar = encrypt(&state.cfg.app_secret, "webcal://calendar.example/private/basic.ics");
        sqlx::query("update booking_pages set calendar_url_enc = $2, calendar_error = 'Timed out' where id = $1")
            .bind(id)
            .bind(&calendar)
            .execute(&db)
            .await
            .unwrap();
        let before = saved(&db, id).await;

        let refused = [
            json!({ "slug": "a" }),
            json!({ "slug": "x".repeat(49) }),
            json!({ "slug": "-alex" }),
            json!({ "slug": "alex_morgan" }),
            json!({ "name": "   " }),
            json!({ "name": "x".repeat(81) }),
            json!({ "duration_minutes": 20 }),
            json!({ "timezone": "Mars/Olympus" }),
            json!({ "hours": vec![[540, 1020]; 6] }),
            json!({ "hours": [[540, 540], null, null, null, null, null, null] }),
            json!({ "hours": [[-1, 600], null, null, null, null, null, null] }),
            json!({ "hours": [[540, 1441], null, null, null, null, null, null] }),
            json!({ "buffer_minutes": -1 }),
            json!({ "buffer_minutes": 121 }),
            json!({ "notice_minutes": 20161 }),
            json!({ "days_ahead": 0 }),
            json!({ "days_ahead": 91 }),
            json!({ "reminder_hours": 169 }),
            json!({ "location": "x".repeat(501) }),
            // A good value alongside a bad one doesn't get saved either.
            json!({ "name": "Demo", "days_ahead": 91 }),
        ];
        let unchanged = || {
            let (db, before, calendar) = (&db, &before, &calendar);
            async move {
                let after = saved(db, id).await;
                serde_json::to_value(&after).unwrap() == serde_json::to_value(before).unwrap() && after.calendar_url_enc == *calendar
            }
        };
        for body in refused {
            let result = edit(&state, acme, id, body.clone()).await;
            assert!(matches!(result, Err(AppError::BadRequest(_))), "{body}: {:?}", result.map(|v| v.page.slug));
            assert!(unchanged().await, "{body}");
        }
        // A calendar address that can't be read is refused before anything is saved, so the old one stays.
        let unreadable = edit(&state, acme, id, json!({ "name": "Demo", "calendar_url": "://" })).await;
        assert!(matches!(&unreadable, Err(AppError::BadRequest(m)) if m.starts_with("Couldn't read that calendar")), "{:?}", unreadable.map(|v| v.page.slug));
        assert!(unchanged().await);
        let taken = edit(&state, acme, id, json!({ "slug": "sam-lee", "name": "Demo" })).await;
        assert!(matches!(&taken, Err(AppError::Conflict(m)) if m == "That link is taken. Try another."), "{:?}", taken.map(|v| v.page.slug));
        assert!(unchanged().await);
        assert!(matches!(edit(&state, other, id, json!({ "name": "Demo" })).await, Err(AppError::NotFound)));
        assert!(unchanged().await);

        let view = edit(&state, acme, id, json!({ "location": "  Our office  " })).await.unwrap();
        assert_eq!(view.calendar_host, "calendar.example", "a page left without calendar_url keeps its calendar");
        assert_eq!((view.page.location.as_str(), view.page.calendar_error.as_str()), ("Our office", "Timed out"));

        let hours = json!([[0, 1440], null, [540, 600], null, null, null, null]);
        let view = edit(
            &state,
            acme,
            id,
            json!({
                "slug": " Alex-Call ", "name": " Demo ", "duration_minutes": 45, "timezone": "America/New_York", "hours": hours,
                "buffer_minutes": 120, "notice_minutes": 20160, "days_ahead": 90, "reminder_hours": 168, "calendar_url": " ",
            }),
        )
        .await
        .unwrap();
        let page = saved(&db, id).await;
        assert_eq!((page.slug.as_str(), page.name.as_str(), page.duration_minutes, page.timezone.as_str()), ("alex-call", "Demo", 45, "America/New_York"));
        assert_eq!(serde_json::to_value(&page.hours).unwrap(), hours);
        assert_eq!((page.buffer_minutes, page.notice_minutes, page.days_ahead, page.reminder_hours), (120, 20160, 90, 168));
        assert_eq!((page.location.as_str(), page.calendar_url_enc.as_str(), page.calendar_error.as_str()), ("Our office", "", ""), "a blank calendar_url removes it");
        assert_eq!((view.url.as_str(), view.calendar_host.as_str()), ("https://app.test/book/alex-call", ""));

        let long = edit(&state, acme, id, json!({ "slug": "a".repeat(48), "name": "n".repeat(80), "location": "l".repeat(500) })).await.unwrap().page;
        assert_eq!((long.slug.len(), long.name.chars().count(), long.location.chars().count()), (48, 80, 500));

        let low = edit(&state, acme, id, json!({ "slug": "ab", "buffer_minutes": 0, "notice_minutes": 0, "days_ahead": 1, "reminder_hours": 0 })).await.unwrap().page;
        assert_eq!((low.slug.as_str(), low.buffer_minutes, low.notice_minutes, low.days_ahead, low.reminder_hours), ("ab", 0, 0, 1, 0));
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn deleting_a_page_clears_the_mailboxs_link_only_when_it_pointed_there(db: PgPool) {
        let state = state(db.clone());
        let (acme, other) = (org(&db, "Acme").await, org(&db, "Other").await);
        let (alex, sam) = (mailbox(&db, acme, "alex@acme.example", "Alex Morgan").await, mailbox(&db, acme, "sam@acme.example", "Sam Lee").await);
        let alexs = create_page(&state, acme, alex).await.unwrap();
        let sams = create_page(&state, acme, sam).await.unwrap();
        let set_link = |id: Uuid, link: String| {
            let db = db.clone();
            async move { sqlx::query("update mailboxes set booking_link = $2 where id = $1").bind(id).bind(link).execute(&db).await.unwrap() }
        };
        set_link(alex, alexs.url.clone()).await;
        set_link(sam, "https://cal.example/sam".into()).await;
        let link = |id: Uuid| {
            let db = db.clone();
            async move { sqlx::query_scalar::<_, String>("select booking_link from mailboxes where id = $1").bind(id).fetch_one(&db).await.unwrap() }
        };

        assert!(matches!(remove(State(state.clone()), owner(other), Path(alexs.page.id)).await, Err(AppError::NotFound)));
        assert_eq!(link(alex).await, alexs.url, "another org can't delete the page");

        for page in [&alexs, &sams] {
            assert_eq!(remove(State(state.clone()), owner(acme), Path(page.page.id)).await.unwrap().0, json!({ "ok": true }));
        }
        assert_eq!(link(alex).await, "");
        assert_eq!(link(sam).await, "https://cal.example/sam", "a link to somewhere else stays");
        assert!(matches!(remove(State(state.clone()), owner(acme), Path(alexs.page.id)).await, Err(AppError::NotFound)));
        assert_eq!(sqlx::query_scalar::<_, i64>("select count(*) from booking_pages").fetch_one(&db).await.unwrap(), 0);
    }

    #[sqlx::test]
    #[ignore = "needs Postgres"]
    async fn the_public_page_names_the_host_and_hides_a_video_link(db: PgPool) {
        let state = state(db.clone());
        let acme = org(&db, "Acme").await;
        let page = create_page(&state, acme, mailbox(&db, acme, "alex@acme.example", "Alex Morgan").await).await.unwrap().page;
        let visit = |slug: &str| {
            let (state, slug) = (state.clone(), slug.to_string());
            let (peer, headers) = visitor("192.0.2.10");
            async move { show(State(state), peer, headers, Path(slug)).await.map(|Json(p)| p) }
        };

        edit(&state, acme, page.id, json!({ "location": "https://meet.example/alex?pwd=secret", "notice_minutes": 0 })).await.unwrap();
        let asked = Utc::now();
        let shown = visit("Alex-Morgan").await.unwrap();
        assert_eq!((shown.slug.as_str(), shown.host.as_str(), shown.location.as_str()), ("alex-morgan", "Alex Morgan", "Video call"));
        assert!(!shown.slots.is_empty() && shown.slots.iter().all(|s| *s >= asked), "{:?}", shown.slots);

        edit(&state, acme, page.id, json!({ "location": "Our office, 1 Main St" })).await.unwrap();
        assert_eq!(visit("alex-morgan").await.unwrap().location, "Our office, 1 Main St");
        assert!(matches!(visit("nobody").await, Err(AppError::NotFound)));
        let (peer, headers) = visitor("192.0.2.10");
        assert!(matches!(manage(State(state.clone()), peer, headers, Path("not-a-token".into())).await, Err(AppError::NotFound)));
    }

    #[test]
    fn one_address_gets_60_public_requests_a_minute() {
        let peer: SocketAddr = "10.0.0.1:443".parse().unwrap();
        let from = |ip: &str| {
            let mut headers = HeaderMap::new();
            headers.insert("x-forwarded-for", ip.parse().unwrap());
            headers
        };
        for _ in 0..60 {
            limit(&from("198.51.100.60"), peer).unwrap();
        }
        assert!(matches!(limit(&from("198.51.100.60"), peer), Err(AppError::TooManyRequests(_))));
        limit(&from("198.51.100.61"), peer).expect("another address still gets in");

        // A minute on, the address starts again.
        HITS.lock().unwrap().get_mut("198.51.100.60").unwrap().0 -= Duration::from_secs(61);
        limit(&from("198.51.100.60"), peer).expect("a new minute");
    }

    #[test]
    fn slugs_come_from_the_name_or_address() {
        assert_eq!(slug_from("Alex Morgan"), "alex-morgan");
        assert_eq!(slug_from("alex.morgan+sales"), "alex-morgan-sales");
        assert_eq!(slug_from("  Zoë!  "), "zo");
        assert_eq!(slug_from("é"), "meet");
    }
}
