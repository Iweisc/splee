use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::invite::{self, Change, Host, Links};
use super::*;
use crate::config::Config;
use crate::models::Mailbox;

fn at(s: &str) -> Ts {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn hhmm(t: &Ts, tz: Tz) -> String {
    t.with_timezone(&tz).format("%a %H:%M").to_string()
}

const LONDON: Tz = chrono_tz::Europe::London;

/// 30-minute calls, 9:00 to 12:00 on weekdays in London, no buffer or notice, a week ahead.
fn rules() -> Rules {
    let morning = Some((9 * 60, 12 * 60));
    Rules { minutes: 30, tz: LONDON, hours: [morning, morning, morning, morning, morning, None, None], buffer: 0, notice: 0, days_ahead: 7 }
}

#[test]
fn slots_follow_working_hours_notice_and_buffer() {
    // Monday 16 November 2026, 10:10 in London (GMT).
    let now = at("2026-11-16T10:10:00Z");
    let day = |slots: &[Ts], date: &str| -> Vec<String> {
        slots.iter().filter(|s| s.date_naive().to_string() == date).map(|s| hhmm(s, LONDON)).collect()
    };
    let slots = slots(&rules(), now, &[]);
    // Monday's slots before now are gone, Tuesday has the whole morning and the weekend has none.
    assert_eq!(day(&slots, "2026-11-16"), ["Mon 10:30", "Mon 11:00", "Mon 11:30"]);
    assert_eq!(day(&slots, "2026-11-17"), ["Tue 09:00", "Tue 09:30", "Tue 10:00", "Tue 10:30", "Tue 11:00", "Tue 11:30"]);
    assert!(day(&slots, "2026-11-21").is_empty() && day(&slots, "2026-11-22").is_empty());
    assert_eq!(slots.len(), 3 + 6 * 4);

    // Four hours' notice leaves Monday empty and starts Tuesday at 9.
    let notice = Rules { notice: 240, ..rules() };
    assert_eq!(super::slots(&notice, now, &[]).first().map(|s| hhmm(s, LONDON)).as_deref(), Some("Tue 09:00"));

    // A call on Tuesday 10:00 to 10:30 takes its slot, and with a 15-minute buffer the slots touching it too.
    let busy = [(at("2026-11-17T10:00:00Z"), at("2026-11-17T10:30:00Z"))];
    assert_eq!(day(&super::slots(&rules(), now, &busy), "2026-11-17"), ["Tue 09:00", "Tue 09:30", "Tue 10:30", "Tue 11:00", "Tue 11:30"]);
    let buffered = Rules { buffer: 15, ..rules() };
    assert_eq!(day(&super::slots(&buffered, now, &busy), "2026-11-17"), ["Tue 09:00", "Tue 11:00", "Tue 11:30"]);

    // 15-minute calls start every 15 minutes, and the last one ends when the day does.
    let short = Rules { minutes: 15, hours: [Some((9 * 60, 10 * 60)), None, None, None, None, None, None], ..rules() };
    let monday: Vec<String> = super::slots(&short, at("2026-11-16T00:00:00Z"), &[]).iter().map(|s| hhmm(s, LONDON)).collect();
    assert_eq!(monday, ["Mon 09:00", "Mon 09:15", "Mon 09:30", "Mon 09:45"]);
}

#[test]
fn slots_skip_busy_times_from_the_calendar() {
    let ics = concat!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n",
        // A weekly team call on Tuesdays and Thursdays, 9:30 to 10:30 London time, except on 19 November.
        "BEGIN:VEVENT\r\nUID:team\r\nDTSTART;TZID=Europe/London:20261020T093000\r\nDTEND;TZID=Europe/London:20261020T103000\r\n",
        "RRULE:FREQ=WEEKLY;BYDAY=TU,TH\r\nEXDATE;TZID=Europe/London:20261119T093000\r\nEND:VEVENT\r\n",
        // A day off on Wednesday.
        "BEGIN:VEVENT\r\nUID:off\r\nDTSTART;VALUE=DATE:20261118\r\nDTEND;VALUE=DATE:20261119\r\nSUMMARY:Off\r\nEND:VEVENT\r\n",
        "END:VCALENDAR\r\n",
    );
    let now = at("2026-11-16T00:00:00Z");
    let busy = calendar::busy(&calendar::parse(ics), LONDON, now, now + Duration::days(7));
    let open: Vec<String> = slots(&rules(), now, &busy).iter().map(|s| hhmm(s, LONDON)).collect();
    let day = |name: &str| open.iter().filter(|s| s.starts_with(name)).map(|s| &s[4..]).collect::<Vec<_>>();
    assert_eq!(day("Mon"), ["09:00", "09:30", "10:00", "10:30", "11:00", "11:30"]);
    assert_eq!(day("Tue"), ["09:00", "10:30", "11:00", "11:30"]);
    assert!(day("Wed").is_empty());
    assert_eq!(day("Thu").len(), 6, "the 19th is an EXDATE");
    assert_eq!(day("Fri").len(), 6);
}

#[test]
fn slots_keep_local_hours_across_a_dst_change() {
    // London's clocks go back an hour at 2:00 on Sunday 25 October 2026.
    let every_day = Rules { hours: [Some((9 * 60, 10 * 60)); 7], ..rules() };
    let starts: Vec<Ts> = slots(&every_day, at("2026-10-24T00:00:00Z"), &[]).into_iter().take(6).collect();
    assert_eq!(
        starts,
        [
            at("2026-10-24T08:00:00Z"),
            at("2026-10-24T08:30:00Z"),
            at("2026-10-25T09:00:00Z"),
            at("2026-10-25T09:30:00Z"),
            at("2026-10-26T09:00:00Z"),
            at("2026-10-26T09:30:00Z"),
        ]
    );
    // And forward at 1:00 on 29 March, so a 1:00 or 1:30 slot doesn't exist.
    let night = Rules { hours: [None, None, None, None, None, None, Some((0, 3 * 60))], days_ahead: 1, ..rules() };
    let starts: Vec<String> = slots(&night, at("2026-03-29T00:00:00Z"), &[]).iter().map(|s| s.with_timezone(&LONDON).format("%H:%M").to_string()).collect();
    assert_eq!(starts, ["00:00", "00:30", "02:00", "02:30"]);
}

fn page(id: Uuid, org_id: Uuid, mailbox_id: Uuid) -> BookingPage {
    BookingPage {
        id,
        org_id,
        mailbox_id,
        slug: "alex-morgan".into(),
        name: "Intro call".into(),
        duration_minutes: 30,
        timezone: "Europe/London".into(),
        hours: sqlx::types::Json(vec![Some([540, 1020]); 7]),
        buffer_minutes: 0,
        notice_minutes: 0,
        days_ahead: 14,
        location: "https://meet.example/abc".into(),
        calendar_url_enc: String::new(),
        calendar_error: String::new(),
        created_at: Utc::now(),
    }
}

#[test]
fn the_invite_is_a_calendar_request_from_the_mailbox() {
    let p = page(Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let booking = Booking {
        id: Uuid::new_v4(),
        page_id: p.id,
        starts_at: at("2026-11-17T14:00:00Z"),
        ends_at: at("2026-11-17T14:30:00Z"),
        name: "Sarah Chen".into(),
        email: "sarah@brightline.example".into(),
        note: "About pricing; and seats, please".into(),
        timezone: "America/New_York".into(),
        sequence: 0,
        cancelled_at: None,
        created_at: Utc::now(),
    };
    let host = Host { name: "Alex Morgan", email: "alex@acme.example" };
    let links = Links::new("https://app.test", &p, "tok123");
    let ics = invite::ics(&p, &host, &booking, Change::Booked, &links);
    let unfolded = ics.replace("\r\n ", "");
    for line in [
        "METHOD:REQUEST",
        &format!("UID:{}@splee", booking.id),
        "SEQUENCE:0",
        "DTSTART:20261117T140000Z",
        "DTEND:20261117T143000Z",
        "SUMMARY:Intro call: Sarah Chen and Alex Morgan",
        "LOCATION:https://meet.example/abc",
        "ORGANIZER;CN=\"Alex Morgan\":mailto:alex@acme.example",
        "ATTENDEE;CN=\"Sarah Chen\";ROLE=REQ-PARTICIPANT;PARTSTAT=NEEDS-ACTION;RSVP=TRUE:mailto:sarah@brightline.example",
        "STATUS:CONFIRMED",
    ] {
        assert!(unfolded.lines().any(|l| l == line), "missing {line} in\n{ics}");
    }
    // The note's punctuation is escaped, and the links to move or cancel are in the description.
    assert!(unfolded.contains("Note from Sarah Chen: About pricing\\; and seats\\, please"), "{unfolded}");
    assert!(unfolded.contains("Move or cancel: https://app.test/booking/tok123"));
    assert!(ics.ends_with("\r\n") && ics.split_terminator("\r\n").all(|l| l.len() <= 75 && !l.contains('\n')), "lines are folded and end in CRLF");

    // The guest reads their own time zone, the host the page's.
    let (subject, body) = invite::to_guest(&p, &host, &booking, Change::Booked, &links);
    assert_eq!(subject, "Booked: Intro call with Alex Morgan, Tue 17 Nov at 09:00");
    assert!(body.contains("Tuesday 17 November, 09:00 to 09:30 (America/New York time)"), "{body}");
    assert!(body.contains("https://app.test/booking/tok123?cancel=1"));
    let (subject, body) = invite::to_host(&p, &booking, Change::Booked, &links);
    assert_eq!(subject, "New booking: Sarah Chen, Tue 17 Nov at 14:00");
    assert!(body.starts_with("Sarah Chen (sarah@brightline.example) booked a call for Tuesday 17 November, 14:00 to 14:30 (Europe/London time): Intro call."));
    assert!(body.contains("Their note: About pricing; and seats, please"));

    // A move keeps the UID with a higher SEQUENCE, and a cancellation is a CANCEL.
    let moved = Booking { sequence: 1, ..booking.clone() };
    assert!(invite::ics(&p, &host, &moved, Change::Moved, &links).contains("SEQUENCE:1\r\n"));
    let cancelled = invite::ics(&p, &host, &Booking { sequence: 2, ..booking }, Change::Cancelled, &links);
    assert!(cancelled.contains("METHOD:CANCEL\r\n") && cancelled.contains("STATUS:CANCELLED\r\n") && !cancelled.contains("Move or cancel"));
}

fn state(db: PgPool) -> AppState {
    let cfg = Config {
        database_url: String::new(),
        app_secret: "0123456789abcdef0123456789abcdef".into(),
        public_url: "https://app.test".into(),
        unsubscribe_url: "https://app.test".into(),
        bind_addr: String::new(),
        agent_model: String::new(),
        agent_concurrency: 0,
        sending_enabled: false,
        bird_bin: None,
        open_signup: true,
        public_agent: false,
        porkbun_api_url: String::new(),
        twilio_api_url: String::new(),
        bots_api_url: String::new(),
        outreach_api_url: String::new(),
        operator_emails: Vec::new(),
        google: None,
        microsoft: None,
        slack: None,
        tumblr: None,
        ipinfo_token: None,
    };
    AppState { db, cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(16).0 }
}

/// An org with a project, a mailbox and a booking page open every day from 9:00 to 17:00 London time.
async fn setup(db: &PgPool, name: &str) -> (Uuid, Uuid, Mailbox, BookingPage) {
    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap();
    let project: Uuid =
        sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(db).await.unwrap();
    let mb: Mailbox = sqlx::query_as(
        "insert into mailboxes (org_id, email, from_name, smtp_host, smtp_username, smtp_password_enc)
         values ($1, $2, 'Alex Morgan', 'smtp.test', '', '') returning *",
    )
    .bind(org)
    .bind(format!("alex@{}.example", name.to_lowercase()))
    .fetch_one(db)
    .await
    .unwrap();
    let page: BookingPage = sqlx::query_as(
        "insert into booking_pages (org_id, mailbox_id, slug, timezone, hours, notice_minutes)
         values ($1, $2, $3, 'Europe/London', $4, 0) returning *",
    )
    .bind(org)
    .bind(mb.id)
    .bind(format!("alex-{}", name.to_lowercase()))
    .bind(sqlx::types::Json(vec![Some([540, 1020]); 7]))
    .fetch_one(db)
    .await
    .unwrap();
    (org, project, mb, page)
}

async fn lead(db: &PgPool, project: Uuid, email: &str) -> Lead {
    sqlx::query_as("insert into leads (project_id, full_name, email, company_name, status) values ($1, 'Sarah Chen', $2, 'Brightline', 'hot') returning *")
        .bind(project)
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn reload(db: &PgPool, lead: &Lead) -> Lead {
    sqlx::query_as("select * from leads where id = $1").bind(lead.id).fetch_one(db).await.unwrap()
}

fn guest(email: &str) -> Guest {
    Guest::new(" Sarah   Chen ", email, "", "America/New_York").unwrap()
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_email_carries_the_invite_as_a_calendar_part_and_a_file(db: PgPool) {
    let (.., mb, _) = setup(&db, "Acme").await;
    let ics = "BEGIN:VCALENDAR\r\nMETHOD:REQUEST\r\nEND:VCALENDAR\r\n";
    let message = crate::mail::smtp::build_invite(&mb, "Sarah Chen", "sarah@brightline.example", "Booked", "Hi Sarah", ics, "REQUEST").unwrap();
    let raw = String::from_utf8(message.formatted()).unwrap();
    assert!(raw.contains("multipart/mixed") && raw.contains("multipart/alternative"), "{raw}");
    assert!(raw.contains("Content-Type: text/calendar; charset=utf-8; method=REQUEST"), "{raw}");
    assert!(raw.contains("invite.ics"), "{raw}");
    assert!(raw.contains("From: \"Alex Morgan\" <alex@acme.example>"), "{raw}");
    assert!(raw.contains("To: \"Sarah Chen\" <sarah@brightline.example>"), "{raw}");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_slot_can_only_be_booked_once(db: PgPool) {
    let state = state(db.clone());
    let (_, _, _, page) = setup(&db, "Acme").await;
    let slots = open_slots(&state, &page, None).await.unwrap();
    let first = slots[2];

    // Five people picking the same time at once: one gets it.
    let tries = futures::future::join_all((0..5).map(|i| {
        let (state, page) = (state.clone(), page.clone());
        async move { book(&state, &page, first, &guest(&format!("g{i}@x.example")), &format!("10.0.0.{i}")).await }
    }))
    .await;
    assert_eq!(tries.iter().filter(|r| r.is_ok()).count(), 1, "{tries:?}");
    assert!(tries.iter().filter_map(|r| r.as_ref().err()).all(|e| matches!(e, AppError::Conflict(m) if m == TAKEN)));
    assert!(!open_slots(&state, &page, None).await.unwrap().contains(&first));

    // A 60-minute page whose slots start every 30 minutes: the one starting inside a booked call is refused too, even
    // past the slot check, by the lock.
    sqlx::query("update booking_pages set duration_minutes = 60 where id = $1").bind(page.id).execute(&db).await.unwrap();
    let page: BookingPage = sqlx::query_as("select * from booking_pages where id = $1").bind(page.id).fetch_one(&db).await.unwrap();
    let later = *open_slots(&state, &page, None).await.unwrap().iter().find(|s| **s > first + Duration::hours(3)).unwrap();
    book(&state, &page, later, &guest("a@x.example"), "10.0.1.1").await.unwrap();
    let mut tx = db.begin().await.unwrap();
    let overlap = claim(&mut tx, &page, later + Duration::minutes(30), None).await.unwrap_err();
    assert!(matches!(overlap, AppError::Conflict(_)), "{overlap:?}");
    drop(tx);
    let refused = book(&state, &page, later + Duration::minutes(30), &guest("b@x.example"), "10.0.1.2").await.unwrap_err();
    assert!(matches!(refused, AppError::Conflict(_)), "{refused:?}");

    // A time that isn't a slot, like 3:07, is refused.
    let odd = later + Duration::days(1) + Duration::minutes(7);
    assert!(matches!(book(&state, &page, odd, &guest("c@x.example"), "10.0.1.3").await, Err(AppError::Conflict(_))));

    // One address can book five calls an hour. Every other slot, since an hour-long call overlaps the next one.
    let free: Vec<Ts> = open_slots(&state, &page, None).await.unwrap().into_iter().rev().step_by(2).take(6).collect();
    for (i, start) in free.iter().enumerate() {
        let result = book(&state, &page, *start, &guest(&format!("r{i}@x.example")), "10.0.2.1").await;
        assert_eq!(result.is_ok(), i < 5, "booking {i}: {result:?}");
    }
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_booking_marks_the_orgs_leads_with_that_email(db: PgPool) {
    let state = state(db.clone());
    let (_, project, _, page) = setup(&db, "Acme").await;
    let (_, other_project, ..) = setup(&db, "Other").await;
    let sarah = lead(&db, project, "Sarah@Brightline.example").await;
    let stranger = lead(&db, other_project, "sarah@brightline.example").await;

    let start = open_slots(&state, &page, None).await.unwrap()[4];
    let (booking, token) = book(&state, &page, start, &guest("sarah@brightline.example"), "10.0.0.1").await.unwrap();
    assert_eq!((booking.name.as_str(), booking.timezone.as_str()), ("Sarah Chen", "America/New_York"));
    let marked = reload(&db, &sarah).await;
    assert_eq!((marked.status.as_str(), marked.meeting_at), ("meeting_booked", Some(start)));
    // Through the same code as the scheduling tools' webhooks, so the lead's deal moves too.
    let deal: String = sqlx::query_scalar("select s.name from deals d join deal_stages s on s.id = d.stage_id where d.lead_id = $1")
        .bind(sarah.id)
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(deal, "Meeting booked");
    // Another org's lead with the same address isn't touched.
    assert_eq!(reload(&db, &stranger).await.status, "hot");

    // Moving the call moves the lead's meeting time, and cancelling puts the lead back to Hot.
    let new_start = *open_slots(&state, &page, Some(booking.id)).await.unwrap().iter().find(|s| **s > start + Duration::hours(2)).unwrap();
    reschedule(&state, &token, new_start).await.unwrap();
    let moved = reload(&db, &sarah).await;
    assert_eq!((moved.status.as_str(), moved.meeting_at), ("meeting_booked", Some(new_start)));
    cancel(&state, &token).await.unwrap();
    let cancelled = reload(&db, &sarah).await;
    assert_eq!((cancelled.status.as_str(), cancelled.meeting_at), ("hot", None));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_links_token_moves_and_cancels_only_its_booking(db: PgPool) {
    let state = state(db.clone());
    let (.., page) = setup(&db, "Acme").await;
    let slots = open_slots(&state, &page, None).await.unwrap();
    let (booking, token) = book(&state, &page, slots[0], &guest("sarah@brightline.example"), "10.0.0.1").await.unwrap();
    let (_, other_token) = book(&state, &page, slots[3], &guest("jo@brightline.example"), "10.0.0.2").await.unwrap();
    assert_ne!(token, other_token);
    assert!(token.len() >= 32, "the token is long enough not to guess");
    // Only its hash is stored.
    let stored: String = sqlx::query_scalar("select token_hash from bookings where id = $1").bind(booking.id).fetch_one(&db).await.unwrap();
    assert!(stored != token && stored == crate::crypto::sha256_hex(&token));

    assert!(matches!(by_token(&db, "not-a-token").await, Err(AppError::NotFound)));
    assert!(matches!(reschedule(&state, "not-a-token", slots[1]).await, Err(AppError::NotFound)));
    assert!(matches!(cancel(&state, "not-a-token").await, Err(AppError::NotFound)));

    // Moving onto the other booking's time is refused, moving to a free one bumps the invite's SEQUENCE.
    assert!(matches!(reschedule(&state, &token, slots[3]).await, Err(AppError::Conflict(_))));
    let moved = reschedule(&state, &token, slots[1]).await.unwrap();
    assert_eq!((moved.id, moved.starts_at, moved.ends_at, moved.sequence), (booking.id, slots[1], slots[1] + Duration::minutes(30), 1));
    // The old time is free again, the new one taken.
    let open = open_slots(&state, &page, None).await.unwrap();
    assert!(open.contains(&slots[0]) && !open.contains(&slots[1]));

    let cancelled = cancel(&state, &token).await.unwrap();
    assert_eq!(cancelled.sequence, 2);
    assert!(cancelled.cancelled_at.is_some());
    assert!(open_slots(&state, &page, None).await.unwrap().contains(&slots[1]));
    // A cancelled call can't be moved or cancelled again, and the other booking stands.
    assert!(matches!(reschedule(&state, &token, slots[2]).await, Err(AppError::Conflict(m)) if m.contains("cancelled")));
    assert!(matches!(cancel(&state, &token).await, Err(AppError::Conflict(_))));
    assert!(by_token(&db, &other_token).await.unwrap().0.cancelled_at.is_none());
    // Its time can be booked by someone else.
    book(&state, &page, slots[1], &guest("new@brightline.example"), "10.0.0.3").await.unwrap();
}
