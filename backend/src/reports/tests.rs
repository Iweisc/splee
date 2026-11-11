use std::net::SocketAddr;

use axum::Json;
use axum::extract::{ConnectInfo, Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::email::{due, period};
use super::*;
use crate::auth::Auth;
use crate::routes::reports as routes;

fn at(s: &str) -> Ts {
    DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
}

fn state(db: &PgPool) -> AppState {
    crate::autopilot::tests::state(db.clone())
}

struct Seeded {
    org: Uuid,
    project: Uuid,
    /// A project of the same org with nothing sent.
    quiet: Uuid,
}

/// The user every test acts as, whichever organization it acts in.
const USER: Uuid = Uuid::from_u128(1);

fn owner(org: Uuid) -> Auth {
    Auth { user_id: USER, org_id: org, role: "owner".into(), token_hash: String::new() }
}

/// Northwind in New York, which is UTC-5 in November. `week` is a day in the week whose numbers the tests read: an email
/// that day, a hot reply with the lead's address in its signature the day after and a meeting the day after that. The
/// week before gets one email.
async fn seed(db: &PgPool, week: Ts) -> Seeded {
    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Fieldnote Agency') returning id").fetch_one(db).await.unwrap();
    sqlx::query("insert into users (id, email, name, password_hash) values ($1, 'sam@fieldnote.test', 'Sam', 'x')").bind(USER).execute(db).await.unwrap();
    let project = |name: &'static str| {
        sqlx::query_scalar::<_, Uuid>("insert into projects (org_id, name, domain) values ($1, $2, 'northwind.test') returning id").bind(org).bind(name)
    };
    let (project, quiet) = (project("Northwind").fetch_one(db).await.unwrap(), project("Quiet").fetch_one(db).await.unwrap());
    for p in [project, quiet] {
        sqlx::query("insert into project_settings (project_id, timezone) values ($1, 'America/New_York')").bind(p).execute(db).await.unwrap();
    }
    let segment: Uuid =
        sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Founders') returning id").bind(project).fetch_one(db).await.unwrap();
    let mailbox: Uuid = sqlx::query_scalar(
        "insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, 'sam@fieldnote.test', 'smtp.test', '', '') returning id",
    )
    .bind(org)
    .fetch_one(db)
    .await
    .unwrap();
    let lead = |first: &'static str, last: &'static str, email: &'static str, status: &'static str| {
        sqlx::query_scalar::<_, Uuid>(
            "insert into leads (project_id, segment_id, first_name, last_name, full_name, email, company_name, status)
             values ($1, $2, $3, $4, $3 || ' ' || $4, $5, 'Acme Freight', $6) returning id",
        )
        .bind(project)
        .bind(segment)
        .bind(first)
        .bind(last)
        .bind(email)
        .bind(status)
    };
    let maya = lead("Maya", "Okafor", "maya.okafor@acme.test", "meeting_booked").fetch_one(db).await.unwrap();
    let theo = lead("Theo", "Lind", "theo@acme.test", "contacted").fetch_one(db).await.unwrap();
    let day = chrono::Duration::days(1);
    let messages = [
        (maya, "outbound", "initial", None, "Hi Maya", week),
        (maya, "inbound", "inbound", Some("hot"), "Yes, let's talk next week.\n\nMaya Okafor\nmaya.okafor@acme.test\n\nOn Mon, Sam wrote:\n> Hi Maya", week + day),
        (theo, "outbound", "initial", None, "Hi Theo", week - day * 7),
    ];
    for (lead, direction, kind, classification, body, at) in messages {
        sqlx::query(
            "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, step, status, classification, body, sent_at, created_at, cost_micros)
             values ($1, $2, $3, $4, $5, $6, 0, case when $5 = 'outbound' then 'sent' else 'received' end, $7, $8,
               case when $5 = 'outbound' then $9::timestamptz end, $9::timestamptz, 1000)",
        )
        .bind(project)
        .bind(lead)
        .bind(segment)
        .bind(if direction == "outbound" { Some(mailbox) } else { None })
        .bind(direction)
        .bind(kind)
        .bind(classification)
        .bind(body)
        .bind(at)
        .execute(db)
        .await
        .unwrap();
    }
    sqlx::query("update leads set status_changed_at = $2 where id = $1").bind(maya).bind(week + day * 2).execute(db).await.unwrap();
    sqlx::query("insert into agent_runs (kind, project_id, org_id, input, status, cost_usd, created_at) values ('write_emails', $1, $2, '{}', 'succeeded', 0.75, $3)")
        .bind(project)
        .bind(org)
        .bind(week)
        .execute(db)
        .await
        .unwrap();
    Seeded { org, project, quiet }
}

/// Makes a link through the API and returns its id and token.
async fn make(state: &AppState, org: Uuid, project: Uuid, body: Value) -> (Uuid, String) {
    let Json(made) = routes::create(State(state.clone()), owner(org), Path(project), Json(serde_json::from_value(body).unwrap())).await.unwrap();
    let url = made["url"].as_str().unwrap();
    assert!(url.starts_with("http://localhost:3349/report/"), "{url}");
    (made["link"]["id"].as_str().unwrap().parse().unwrap(), url.rsplit('/').next().unwrap().to_string())
}

/// Each test opens the page from its own address, so the in-memory rate limit of one doesn't spill into another.
async fn open(state: &AppState, token: &str, ip: &str) -> AppResult<Value> {
    let peer: SocketAddr = format!("{ip}:4000").parse().unwrap();
    let Json(report) = routes::show(State(state.clone()), ConnectInfo(peer), HeaderMap::new(), Path(token.to_string())).await?;
    Ok(serde_json::to_value(report).unwrap())
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn tokens_are_stored_hashed_and_revoking_stops_the_link(db: PgPool) {
    let s = seed(&db, Utc::now() - chrono::Duration::days(3)).await;
    let state = state(&db);
    let (id, token) = make(&state, s.org, s.project, json!({ "email_schedule": "weekly", "recipients": ["Client@Northwind.test"] })).await;
    assert_eq!(token.len(), 64);
    let hash: String = sqlx::query_scalar("select token_hash from report_links where id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert_eq!(hash, sha256_hex(&token));
    let row: String = sqlx::query_scalar("select row_to_json(l)::text from report_links l where id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert!(!row.contains(&token), "the token itself is never stored");

    assert_eq!(open(&state, &token, "198.51.100.1").await.unwrap()["name"], "Northwind");
    let viewed: Option<Ts> = sqlx::query_scalar("select last_viewed_at from report_links where id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert!(viewed.is_some());
    // A recipient's link from the email opens the same page. A wrong signature or a token one character off opens nothing.
    let recipient: Uuid = sqlx::query_scalar("select id from report_recipients where link_id = $1").bind(id).fetch_one(&db).await.unwrap();
    let from_email = recipient_token("", VIEW, recipient);
    assert_eq!(open(&state, &from_email, "198.51.100.1").await.unwrap()["name"], "Northwind");
    let unsubscribe = recipient_token("", UNSUBSCRIBE, recipient);
    assert!(matches!(open(&state, &unsubscribe, "198.51.100.1").await, Err(AppError::NotFound)));
    let off_by_one = format!("{}{}", &token[..63], if token.ends_with('0') { '1' } else { '0' });
    assert!(matches!(open(&state, &off_by_one, "198.51.100.1").await, Err(AppError::NotFound)));

    assert_eq!(routes::revoke(State(state.clone()), owner(s.org), Path(id)).await.unwrap()["ok"], true);
    assert!(matches!(open(&state, &token, "198.51.100.1").await, Err(AppError::NotFound)));
    assert!(matches!(open(&state, &from_email, "198.51.100.1").await, Err(AppError::NotFound)));
    assert!(matches!(routes::revoke(State(state.clone()), owner(s.org), Path(id)).await, Err(AppError::NotFound)));
    let Json(list) = routes::list(State(state.clone()), owner(s.org), Path(s.project)).await.unwrap();
    assert!(list.is_empty());
    // A revoked link sends no more emails either.
    assert!(due(&state, at("2026-11-16T13:05:00Z")).await.unwrap().is_empty());
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_expired_link_opens_nothing(db: PgPool) {
    let s = seed(&db, Utc::now() - chrono::Duration::days(3)).await;
    let state = state(&db);
    let past = json!({ "expires_at": "2020-01-01T00:00:00Z" });
    let refused = routes::create(State(state.clone()), owner(s.org), Path(s.project), Json(serde_json::from_value(past).unwrap())).await;
    assert!(matches!(refused, Err(AppError::BadRequest(_))));

    let soon = (Utc::now() + chrono::Duration::days(1)).to_rfc3339();
    let (id, token) = make(&state, s.org, s.project, json!({ "expires_at": soon })).await;
    assert!(open(&state, &token, "198.51.100.2").await.is_ok());
    sqlx::query("update report_links set expires_at = now() - interval '1 minute' where id = $1").bind(id).execute(&db).await.unwrap();
    assert!(matches!(open(&state, &token, "198.51.100.2").await, Err(AppError::NotFound)));
    let Json(list) = routes::list(State(state.clone()), owner(s.org), Path(s.project)).await.unwrap();
    assert!(list.is_empty(), "the list only has live links");
}

const SPEND_KEYS: [&str; 6] = ["claude_cost_micros", "email_cost_micros", "spend_micros", "cost_per_hot_micros", "cost_per_meeting_micros", "return_on_spend"];

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_page_leaves_out_spend_mailboxes_and_leads_unless_ticked(db: PgPool) {
    let s = seed(&db, Utc::now() - chrono::Duration::days(3)).await;
    let state = state(&db);
    let ip = "198.51.100.3";

    let (_, plain) = make(&state, s.org, s.project, json!({ "title": "Q4 outbound", "intro": "Here's how the month is going." })).await;
    let page = open(&state, &plain, ip).await.unwrap();
    let a = &page["analytics"];
    assert_eq!((page["title"].as_str(), page["intro"].as_str()), (Some("Q4 outbound"), Some("Here's how the month is going.")));
    assert_eq!(page["logo_url"], "https://www.google.com/s2/favicons?domain=northwind.test&sz=128");
    assert_eq!((a["totals"]["sent"].as_i64(), a["totals"]["replies"].as_i64(), a["totals"]["meetings"].as_i64()), (Some(2), Some(1), Some(1)));
    for key in SPEND_KEYS {
        assert!(a["totals"].get(key).is_none() && a["previous"].get(key).is_none(), "{key}");
    }
    assert!(a["campaigns"].as_array().unwrap().iter().all(|c| ["spend_micros", "cost_per_hot_micros", "return_on_spend"].iter().all(|k| c.get(*k).is_none())));
    assert!(a.get("mailboxes").is_none());
    assert!(page["meetings"].is_null() && page["hot_replies"].is_null());
    // Nothing on the page is an address: not the mailbox, not a lead's.
    assert!(!page.to_string().contains('@'), "{page}");

    let (_, spend) = make(&state, s.org, s.project, json!({ "show_spend": true })).await;
    let a = open(&state, &spend, ip).await.unwrap()["analytics"].clone();
    assert_eq!(a["totals"]["claude_cost_micros"], 750_000);
    assert!(SPEND_KEYS.iter().all(|k| a["totals"].get(*k).is_some()) && a["campaigns"][0].get("spend_micros").is_some());
    assert!(a.get("mailboxes").is_none());

    let (_, mailboxes) = make(&state, s.org, s.project, json!({ "show_mailboxes": true })).await;
    let page = open(&state, &mailboxes, ip).await.unwrap();
    assert_eq!(page["analytics"]["mailboxes"][0]["email"], "sam@fieldnote.test");
    assert!(page["analytics"]["totals"].get("spend_micros").is_none());
    assert!(!page.to_string().contains("acme.test"));

    let (_, leads) = make(&state, s.org, s.project, json!({ "show_leads": true, "brand": "org" })).await;
    let page = open(&state, &leads, ip).await.unwrap();
    assert_eq!((page["name"].as_str(), page["logo_url"].as_str()), (Some("Fieldnote Agency"), Some("")));
    let meetings = page["meetings"].as_array().unwrap();
    assert_eq!((meetings.len(), &meetings[0]["first_name"], &meetings[0]["company"]), (1, &json!("Maya"), &json!("Acme Freight")));
    let hot = page["hot_replies"].as_array().unwrap();
    // The signature's address and the quoted thread are gone. Last names and addresses never show.
    assert_eq!(hot[0]["excerpt"], "Yes, let's talk next week. Maya Okafor");
    let text = page.to_string();
    assert!(!text.contains('@') && !text.contains("\"Okafor\"") && !text.contains("last_name"), "{text}");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_public_page_is_rate_limited_per_address(db: PgPool) {
    let s = seed(&db, Utc::now() - chrono::Duration::days(3)).await;
    let state = state(&db);
    let (_, token) = make(&state, s.org, s.project, json!({})).await;
    for _ in 0..60 {
        open(&state, &token, "198.51.100.4").await.unwrap();
    }
    assert!(matches!(open(&state, &token, "198.51.100.4").await, Err(AppError::TooManyRequests(_))));
    // Guessing tokens counts too, and another address has its own minute.
    assert!(matches!(open(&state, "guess", "198.51.100.4").await, Err(AppError::TooManyRequests(_))));
    assert!(open(&state, &token, "198.51.100.5").await.is_ok());
}

#[test]
fn emails_go_out_monday_or_on_the_first_from_eight() {
    let local = |s: &str| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap();
    let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    // Monday 16 November 2026.
    assert_eq!(period("weekly", local("2026-11-16 07:59")), None);
    assert_eq!(period("weekly", local("2026-11-16 08:00")), Some((day("2026-11-09"), day("2026-11-15"))));
    assert_eq!(period("weekly", local("2026-11-17 09:00")), None);
    assert_eq!(period("monthly", local("2026-11-16 09:00")), None);
    assert_eq!(period("monthly", local("2026-12-01 08:30")), Some((day("2026-11-01"), day("2026-11-30"))));
    assert_eq!(period("monthly", local("2027-03-01 08:30")), Some((day("2027-02-01"), day("2027-02-28"))));
    assert_eq!(period("none", local("2026-11-16 09:00")), None);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn weekly_emails_follow_the_project_time_zone_and_skip_quiet_weeks(db: PgPool) {
    // Tuesday 10 November, 10:00 in New York.
    let s = seed(&db, at("2026-11-10T15:00:00Z")).await;
    let state = state(&db);
    let weekly = json!({ "email_schedule": "weekly", "recipients": ["ana@northwind.test", "ben@northwind.test"], "show_leads": true });
    let (id, _) = make(&state, s.org, s.project, weekly.clone()).await;
    make(&state, s.org, s.quiet, weekly).await;
    make(&state, s.org, s.project, json!({ "email_schedule": "monthly", "recipients": ["ana@northwind.test"] })).await;

    // 12:30 UTC on Monday is 7:30 in New York, still too early.
    assert!(due(&state, at("2026-11-16T12:30:00Z")).await.unwrap().is_empty());
    // At 8:05 the busy project's two recipients get theirs, the quiet project's get nothing, and the monthly waits.
    let emails = due(&state, at("2026-11-16T13:05:00Z")).await.unwrap();
    let mut to: Vec<&str> = emails.iter().map(|e| e.to.as_str()).collect();
    to.sort();
    assert_eq!(to, ["ana@northwind.test", "ben@northwind.test"]);
    assert!(emails.iter().all(|e| e.link_id == id));
    let e = &emails[0];
    assert_eq!(e.subject, "Northwind outreach report: Nov 9 – 15, 2026");
    for line in ["Emails sent: 1, same as the week before", "Replies: 1, up from 0 the week before", "Meetings booked: 1, up from 0 the week before"] {
        assert!(e.text.contains(line), "{line}\n{}", e.text);
    }
    assert!(e.text.contains("Top campaign: Founders, 1 reply and 1 hot lead from 1 emails"), "{}", e.text);
    assert!(e.text.contains("- Maya at Acme Freight"), "{}", e.text);
    assert!(!e.text.contains("Spend") && !e.html.contains("Spend") && !e.text.contains("acme.test"), "{}", e.text);
    assert!(e.html.contains("See the full report</a>") && e.html.contains("http://localhost:3349/report/"));
    // The email links to the unsubscribe page, and List-Unsubscribe to the API it posts to.
    assert!(e.one_click_url.starts_with("http://localhost:3349/api/report-unsubscribe/"));
    assert!(e.html.contains(&e.one_click_url.replace("/api/", "/")));
    // Each recipient has their own links.
    assert_ne!(emails[0].one_click_url, emails[1].one_click_url);

    // Once they've gone out, a later check that Monday sends nothing again.
    for e in &emails {
        assert!(super::email::deliver(&db, e, || async { Ok(true) }).await.unwrap());
    }
    assert!(due(&state, at("2026-11-16T15:00:00Z")).await.unwrap().is_empty());
    // The 1st of December in New York sends the monthly, for November.
    let monthly = due(&state, at("2026-12-01T13:30:00Z")).await.unwrap();
    assert_eq!(monthly.len(), 1);
    assert_eq!(monthly[0].subject, "Northwind outreach report: November 2026");
    assert!(monthly[0].text.contains("the month before"));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn two_backends_checking_at_once_send_one_email(db: PgPool) {
    let s = seed(&db, at("2026-11-10T15:00:00Z")).await;
    let state = state(&db);
    make(&state, s.org, s.project, json!({ "email_schedule": "weekly", "recipients": ["ana@northwind.test"] })).await;
    let now = at("2026-11-16T13:05:00Z");
    let (a, b) = tokio::join!(due(&state, now), due(&state, now));
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_eq!((a.len(), b.len()), (1, 1));
    // Both try to send it, and the slow first send holds it until it's saved as sent.
    let sends = std::sync::atomic::AtomicUsize::new(0);
    let send = || async {
        sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        Ok(true)
    };
    let (x, y) = tokio::join!(super::email::deliver(&db, &a[0], send), super::email::deliver(&db, &b[0], send));
    assert_eq!((x.unwrap() as u8 + y.unwrap() as u8, sends.into_inner()), (1, 1));
    assert!(due(&state, now).await.unwrap().is_empty());
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_copy_that_failed_goes_out_at_the_next_check(db: PgPool) {
    let s = seed(&db, at("2026-11-10T15:00:00Z")).await;
    let state = state(&db);
    make(&state, s.org, s.project, json!({ "email_schedule": "weekly", "recipients": ["ana@northwind.test", "ben@northwind.test"] })).await;
    let now = at("2026-11-16T13:05:00Z");
    let emails = due(&state, now).await.unwrap();
    assert_eq!(emails.len(), 2);
    // Ana's goes out and Ben's fails.
    for e in &emails {
        let works = e.to.starts_with("ana");
        let sent = super::email::deliver(&db, e, || async move { if works { Ok(true) } else { anyhow::bail!("SMTP said no") } }).await;
        assert_eq!(sent.is_ok(), works);
    }
    let again = due(&state, now).await.unwrap();
    assert_eq!(again.iter().map(|e| e.to.as_str()).collect::<Vec<_>>(), ["ben@northwind.test"]);
    assert!(super::email::deliver(&db, &again[0], || async { Ok(true) }).await.unwrap());
    assert!(due(&state, now).await.unwrap().is_empty());
    let done: Option<NaiveDate> = sqlx::query_scalar("select emailed_period from report_links").fetch_one(&db).await.unwrap();
    assert_eq!(done, NaiveDate::from_ymd_opt(2026, 11, 9));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn unsubscribing_stops_only_that_report(db: PgPool) {
    let s = seed(&db, at("2026-11-10T15:00:00Z")).await;
    let state = state(&db);
    let body = json!({ "email_schedule": "weekly", "recipients": ["ana@northwind.test", "ben@northwind.test"] });
    let (first, _) = make(&state, s.org, s.project, body.clone()).await;
    let (second, _) = make(&state, s.org, s.project, body).await;
    let ana: Uuid = sqlx::query_scalar("select id from report_recipients where link_id = $1 and email = 'ana@northwind.test'")
        .bind(first)
        .fetch_one(&db)
        .await
        .unwrap();
    let peer: SocketAddr = "198.51.100.6:4000".parse().unwrap();
    let token = recipient_token("", UNSUBSCRIBE, ana);
    let Json(done) = routes::unsubscribe(State(state.clone()), ConnectInfo(peer), HeaderMap::new(), Path(token)).await.unwrap();
    assert_eq!((&done["email"], &done["unsubscribed"]), (&json!("ana@northwind.test"), &json!(true)));
    // The page's own token, or one signed for viewing, can't unsubscribe anyone.
    let view = recipient_token("", VIEW, ana);
    assert!(matches!(routes::unsubscribe(State(state.clone()), ConnectInfo(peer), HeaderMap::new(), Path(view)).await, Err(AppError::NotFound)));

    let emails = due(&state, at("2026-11-16T13:05:00Z")).await.unwrap();
    let mut sent: Vec<(Uuid, &str)> = emails.iter().map(|e| (e.link_id, e.to.as_str())).collect();
    sent.sort();
    let mut expected = vec![(first, "ben@northwind.test"), (second, "ana@northwind.test"), (second, "ben@northwind.test")];
    expected.sort();
    assert_eq!(sent, expected);

    // Saving the link again with Ana still listed keeps her unsubscribed.
    let body = json!({ "email_schedule": "weekly", "recipients": ["ana@northwind.test", "ben@northwind.test", "cy@northwind.test"] });
    let Json(link) = routes::update(State(state.clone()), owner(s.org), Path(first), Json(serde_json::from_value(body).unwrap())).await.unwrap();
    let link = serde_json::to_value(link).unwrap();
    assert_eq!(link["recipients"], json!([
        { "email": "ana@northwind.test", "unsubscribed": true },
        { "email": "ben@northwind.test", "unsubscribed": false },
        { "email": "cy@northwind.test", "unsubscribed": false },
    ]));
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn links_stay_in_their_organization(db: PgPool) {
    let s = seed(&db, Utc::now() - chrono::Duration::days(3)).await;
    let state = state(&db);
    let (id, token) = make(&state, s.org, s.project, json!({})).await;
    let other: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
    let body = || Json(serde_json::from_value(json!({})).unwrap());
    assert!(matches!(routes::list(State(state.clone()), owner(other), Path(s.project)).await, Err(AppError::NotFound)));
    assert!(matches!(routes::create(State(state.clone()), owner(other), Path(s.project), body()).await, Err(AppError::NotFound)));
    assert!(matches!(routes::update(State(state.clone()), owner(other), Path(id), body()).await, Err(AppError::NotFound)));
    assert!(matches!(routes::revoke(State(state.clone()), owner(other), Path(id)).await, Err(AppError::NotFound)));
    assert!(open(&state, &token, "198.51.100.7").await.is_ok(), "the other org's tries changed nothing");
    // And the right org's list has it, with the link's settings and no token.
    let Json(list) = routes::list(State(state.clone()), owner(s.org), Path(s.project)).await.unwrap();
    let list = serde_json::to_value(list).unwrap();
    assert_eq!((list.as_array().unwrap().len(), &list[0]["range_preset"]), (1, &json!("last_30")));
    assert!(list[0].get("token_hash").is_none() && !list.to_string().contains(&token));
}

#[test]
fn excerpts_drop_addresses_and_quotes() {
    assert_eq!(excerpt("Sounds good!\nCall me: jo@acme.test\n\nOn Mon, Nov 9, Sam <sam@x.test> wrote:\n> Hi"), "Sounds good! Call me:");
    assert_eq!(no_addresses("Maya maya@acme.test"), "Maya");
    let long = "word ".repeat(60);
    let cut = excerpt(&long);
    assert!(cut.ends_with('…') && cut.chars().count() <= 161, "{cut}");
    assert_eq!(range("month_to_date", NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()).0, NaiveDate::from_ymd_opt(2026, 11, 1).unwrap());
    assert_eq!(range("last_7", NaiveDate::from_ymd_opt(2026, 11, 12).unwrap()).0, NaiveDate::from_ymd_opt(2026, 11, 6).unwrap());
}
