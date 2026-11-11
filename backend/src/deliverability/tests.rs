use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Router;
use chrono::{Days, NaiveDate};
use serde_json::{Value, json};
use sqlx::PgPool;
use uuid::Uuid;

use super::blocklists::tests::FakeResolver;
use super::*;
use crate::crypto::sha256_hex;

fn state(db: &PgPool) -> AppState {
    let cfg = crate::mail::oauth::tests::config(&crate::mail::oauth::tests::FakeProvider { url: String::new(), hits: Arc::default(), forms: Arc::default() });
    AppState { db: db.clone(), cfg: Arc::new(cfg), events: tokio::sync::broadcast::channel(1).0 }
}

async fn org(db: &PgPool, name: &str) -> Uuid {
    sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(db).await.unwrap()
}

async fn mailbox(db: &PgPool, org: Uuid, email: &str) -> Uuid {
    sqlx::query_scalar("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.acme.test', $2, 'x') returning id")
        .bind(org)
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn project(db: &PgPool, org: Uuid) -> Uuid {
    sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Outreach') returning id").bind(org).fetch_one(db).await.unwrap()
}

async fn listed(db: &PgPool, org: Uuid, target: &str, kind: &str, list: &str) {
    sqlx::query("insert into blocklist_checks (org_id, target, kind, list, status, detail) values ($1, $2, $3, $4, 'listed', 'Spam domain')")
        .bind(org)
        .bind(target)
        .bind(kind)
        .bind(list)
        .execute(db)
        .await
        .unwrap();
}

/// `n` leads emailed by `mailbox` `days_ago`, each with a reply of `reply`'s classification for the first `replies` of them.
async fn sent(db: &PgPool, project: Uuid, mailbox: Uuid, n: i64, days_ago: i32, reply: &str, replies: i64) -> Vec<Uuid> {
    let leads: Vec<Uuid> = sqlx::query_scalar("insert into leads (project_id, status) select $1, 'contacted' from generate_series(1, $2) returning id")
        .bind(project)
        .bind(n)
        .fetch_all(db)
        .await
        .unwrap();
    for (i, lead) in leads.iter().enumerate() {
        sqlx::query(
            "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, sent_at, created_at)
             values ($1, $2, $3, 'outbound', 'initial', 'sent', now() - make_interval(days => $4), now() - make_interval(days => $4))",
        )
        .bind(project)
        .bind(lead)
        .bind(mailbox)
        .bind(days_ago)
        .execute(db)
        .await
        .unwrap();
        if (i as i64) < replies {
            sqlx::query(
                "insert into email_messages (project_id, lead_id, mailbox_id, direction, kind, status, classification, created_at)
                 values ($1, $2, $3, 'inbound', 'inbound', 'received', $4, now() - make_interval(days => $5))",
            )
            .bind(project)
            .bind(lead)
            .bind(mailbox)
            .bind(reply)
            .bind(days_ago)
            .execute(db)
            .await
            .unwrap();
        }
    }
    leads
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn blocklist_answers_are_kept_per_target_and_list(db: PgPool) {
    let acme = org(&db, "Acme").await;
    let ana = mailbox(&db, acme, "ana@try-acme.com").await;
    let resolver = FakeResolver::default()
        .with("smtp.acme.test.", &[[85, 1, 2, 3]])
        .with("try-acme.com.dbl.spamhaus.org.", &[[127, 0, 1, 2]])
        .with("try-acme.com.multi.uribl.com.", &[[127, 0, 0, 1]])
        .with("3.2.1.85.zen.spamhaus.org.", &[[127, 255, 255, 254]])
        .with("3.2.1.85.bl.spamcop.net.", &[[127, 0, 0, 2]])
        .failing("3.2.1.85.b.barracudacentral.org.");
    check_blocklists(&db, &resolver, acme).await.unwrap();

    let rows: Vec<(String, String, String)> =
        sqlx::query_as("select target, list, status from blocklist_checks where org_id = $1 order by target, list").bind(acme).fetch_all(&db).await.unwrap();
    let rows: Vec<(&str, &str, &str)> = rows.iter().map(|(t, l, s)| (t.as_str(), l.as_str(), s.as_str())).collect();
    assert_eq!(
        rows,
        vec![
            ("85.1.2.3", "barracuda", "unknown"),
            ("85.1.2.3", "spamcop", "listed"),
            ("85.1.2.3", "spamhaus_zen", "unknown"),
            ("try-acme.com", "spamhaus_dbl", "listed"),
            ("try-acme.com", "surbl", "clear"),
            ("try-acme.com", "uribl", "unknown"),
        ]
    );
    let report = report(&db, acme, today()).await.unwrap();
    let m = &report.mailboxes[0];
    assert_eq!((m.ips.clone(), m.ip_source), (vec!["85.1.2.3".to_string()], "smtp_host"));
    let reasons: Vec<&str> = m.reasons.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(reasons, vec!["Listed on Spamhaus DBL (spam domain)", "Sending IP 85.1.2.3 listed on SpamCop"]);
    assert_eq!(m.verdict, Level::AtRisk);
    let dbl = report.domains[0].listings.iter().find(|l| l.list == "spamhaus_dbl").unwrap();
    assert_eq!(dbl.delist_url, "https://check.spamhaus.org/results/?query=try-acme.com");
    assert_eq!(report.domains[0].listings.len(), 6, "the domain's and its mailbox's address's");

    // A warmup email another mailbox read names the real sending server, which replaces the SMTP host's address.
    let raw = b"Received: from mail-sor-f41.google.com (mail-sor-f41.google.com [209.85.220.41]) by mx.google.com;\r\nSubject: x\r\n\r\nhi";
    note_received_ip(&db, ana, raw).await.unwrap();
    check_blocklists(&db, &resolver, acme).await.unwrap();
    let targets: Vec<String> = sqlx::query_scalar("select distinct target from blocklist_checks where org_id = $1 order by 1").bind(acme).fetch_all(&db).await.unwrap();
    assert_eq!(targets, vec!["209.85.220.41", "try-acme.com"]);
    let report = super::report(&db, acme, today()).await.unwrap();
    assert_eq!(report.mailboxes[0].ip_source, "received");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn an_at_risk_mailbox_is_held_until_resumed_and_only_a_new_problem_holds_it_again(db: PgPool) {
    let state = state(&db);
    let acme = org(&db, "Acme").await;
    let (ana, bo) = (mailbox(&db, acme, "ana@try-acme.com").await, mailbox(&db, acme, "bo@acme-mail.com").await);
    // The alert goes to the project's chat. A loopback URL is refused before anything is sent, and the refusal is recorded,
    // which shows the alert went out through the hooks.
    let outreach = project(&db, acme).await;
    sqlx::query("insert into chat_channels (project_id, url) values ($1, 'http://127.0.0.1:9/hook')").bind(outreach).execute(&db).await.unwrap();
    listed(&db, acme, "try-acme.com", "domain", "spamhaus_dbl").await;

    evaluate(&state, acme).await.unwrap();
    let status = |id: Uuid| {
        let db = db.clone();
        async move {
            sqlx::query_as::<_, (String, String, Option<Ts>, Vec<String>)>("select status, last_error, health_held_at, health_ack from mailboxes where id = $1")
                .bind(id)
                .fetch_one(&db)
                .await
                .unwrap()
        }
    };
    let (s, why, held, _) = status(ana).await;
    assert_eq!(s, "paused");
    assert_eq!(why, "Held by Splee because this mailbox is at risk: Listed on Spamhaus DBL (spam domain). Fix it, then resume the mailbox to send again.");
    assert!(held.is_some());
    assert_eq!(status(bo).await.0, "active");
    let alert: Option<String> = sqlx::query_scalar("select error from chat_channels where project_id = $1").bind(outreach).fetch_one(&db).await.unwrap();
    assert!(alert.is_some_and(|e| e.contains("not a public address")));
    let page = report(&db, acme, today()).await.unwrap();
    assert!(page.mailboxes.iter().find(|m| m.id == ana).is_some_and(|m| m.held && m.verdict == Level::AtRisk));

    // Resumed on the Mailboxes page, it sends through the listing it was held for.
    let ana_user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('ana@acme.test', 'Ana', 'x') returning id").fetch_one(&db).await.unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(acme).bind(ana_user).execute(&db).await.unwrap();
    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
        .bind(sha256_hex("session-acme"))
        .bind(ana_user)
        .bind(acme)
        .execute(&db)
        .await
        .unwrap();
    let base = serve(state.clone()).await;
    let client = reqwest::Client::new();
    let resume = || {
        client
            .patch(format!("{base}/mailboxes/{ana}"))
            .header("cookie", "splee_session=session-acme")
            .json(&json!({ "status": "active" }))
            .send()
    };
    assert_eq!(resume().await.unwrap().status(), 200);
    let (s, why, held, ack) = status(ana).await;
    assert_eq!((s.as_str(), why.as_str(), held, ack), ("active", "", None, vec!["list:spamhaus_dbl:try-acme.com".to_string()]));
    evaluate(&state, acme).await.unwrap();
    assert_eq!(status(ana).await.0, "active", "the same listing doesn't hold it again");

    // A new problem does.
    sqlx::query("update mailboxes set sending_ips = '{85.1.2.3}' where id = $1").bind(ana).execute(&db).await.unwrap();
    listed(&db, acme, "85.1.2.3", "ip", "spamhaus_zen").await;
    evaluate(&state, acme).await.unwrap();
    let (s, why, ..) = status(ana).await;
    assert_eq!(s, "paused");
    assert!(why.contains("Listed on Spamhaus DBL (spam domain). Sending IP 85.1.2.3 listed on Spamhaus ZEN (spam domain)"), "{why}");

    // Once the domain's listing clears, it's forgotten, so a later listing holds the mailbox again.
    assert_eq!(resume().await.unwrap().status(), 200);
    sqlx::query("delete from blocklist_checks where target = 'try-acme.com'").execute(&db).await.unwrap();
    evaluate(&state, acme).await.unwrap();
    assert_eq!(status(ana).await.3, vec!["list:spamhaus_zen:85.1.2.3".to_string()]);

    // A mailbox someone paused themselves is left alone.
    listed(&db, acme, "acme-mail.com", "domain", "surbl").await;
    sqlx::query("update mailboxes set status = 'paused', last_error = '' where id = $1").bind(bo).execute(&db).await.unwrap();
    evaluate(&state, acme).await.unwrap();
    let (_, why, held, _) = status(bo).await;
    assert_eq!((why.as_str(), held), ("", None));
}

async fn serve(state: AppState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/api", listener.local_addr().unwrap());
    let app = Router::new().nest("/api", crate::routes::router()).with_state(state);
    tokio::spawn(async move { axum::serve(listener, app).await });
    base
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn rates_count_leads_in_each_window(db: PgPool) {
    let acme = org(&db, "Acme").await;
    let outreach = project(&db, acme).await;
    let ana = mailbox(&db, acme, "ana@try-acme.com").await;
    // This week: 50 leads with 3 bounces, and 10 more of whom 2 wrote back.
    sent(&db, outreach, ana, 50, 2, "bounce", 3).await;
    sent(&db, outreach, ana, 10, 1, "question", 2).await;
    // Earlier this month: 40 leads, 1 asked to stop and 1 clicked unsubscribe.
    let month = sent(&db, outreach, ana, 40, 20, "unsubscribe", 1).await;
    // Apart, since the status trigger stamps the time of a status change.
    sqlx::query("update leads set status = 'unsubscribed' where id = $1").bind(month[5]).execute(&db).await.unwrap();
    sqlx::query("update leads set status_changed_at = now() - interval '19 days' where id = $1").bind(month[5]).execute(&db).await.unwrap();
    // Too long ago to count.
    sent(&db, outreach, ana, 30, 45, "bounce", 30).await;

    let week = rates(&db, acme, 7).await.unwrap()[&ana];
    assert_eq!(week, Rates { emailed: 60, bounced: 3, opted_out: 0, replied: 2 });
    let month = rates(&db, acme, 30).await.unwrap()[&ana];
    // The reply asking to stop is a reply as well as an opt-out, like the Mailboxes page counts it.
    assert_eq!(month, Rates { emailed: 100, bounced: 3, opted_out: 2, replied: 3 });

    // 3 of 60 is 5% this week, over the 3% line.
    let report = report(&db, acme, today()).await.unwrap();
    let reasons: Vec<&str> = report.mailboxes[0].reasons.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(reasons, vec!["Bounce rate 5% this week, above 3%"]);

    // The chart's last day covers the week: 3 bounces of the 60 leads emailed.
    let all = &report.series[0];
    assert_eq!((all.domain.as_str(), all.days.len()), ("", 30));
    assert_eq!(all.days.last().unwrap().bounce_rate, Some(0.05));
    assert_eq!(report.series[1].domain, "try-acme.com");
}

#[test]
fn the_chart_rolls_a_week_of_counts() {
    let today = NaiveDate::from_ymd_opt(2026, 11, 11).unwrap();
    let day = |ago: u64| today - Days::new(ago);
    let r = |emailed, bounced, opted_out| Rates { emailed, bounced, opted_out, replied: 0 };
    let counts = BTreeMap::from([(day(9), r(100, 9, 0)), (day(3), r(40, 2, 1)), (day(0), r(60, 1, 2))]);
    let spam = BTreeMap::from([(day(1), 0.002)]);
    let points = rolling(&counts, &spam, today);
    assert_eq!(points.len(), 30);
    assert_eq!(points[0].date, day(29));
    let at = |ago: usize| &points[29 - ago];
    // Today's week holds days 0 to 6: 3 of 100 bounced, 3 of 100 opted out.
    assert_eq!(*at(0), Point { date: day(0), bounce_rate: Some(0.03), opt_out_rate: Some(0.03), spam_rate: None });
    assert_eq!(at(1).spam_rate, Some(0.002));
    // Day 9's sends fall out of the week ending on day 2, and before day 9 nothing was sent.
    assert_eq!(at(2).bounce_rate, Some(0.05));
    assert_eq!(at(3).bounce_rate, Some(11.0 / 140.0));
    assert_eq!(at(10).bounce_rate, None);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn each_org_sees_and_holds_only_its_own(db: PgPool) {
    let state = state(&db);
    let (acme, beta) = (org(&db, "Acme").await, org(&db, "Beta").await);
    let (ana, bea) = (mailbox(&db, acme, "ana@shared.test").await, mailbox(&db, beta, "bea@shared.test").await);
    listed(&db, acme, "shared.test", "domain", "spamhaus_dbl").await;
    sqlx::query("insert into postmaster_days (org_id, domain, day, reputation) values ($1, 'shared.test', current_date, 'BAD')").bind(acme).execute(&db).await.unwrap();

    evaluate(&state, acme).await.unwrap();
    evaluate(&state, beta).await.unwrap();
    let status = |id: Uuid| sqlx::query_scalar::<_, String>("select status from mailboxes where id = $1").bind(id).fetch_one(&db);
    assert_eq!((status(ana).await.unwrap(), status(bea).await.unwrap()), ("paused".into(), "active".into()));

    let page = report(&db, beta, today()).await.unwrap();
    assert_eq!(page.mailboxes.iter().map(|m| m.id).collect::<Vec<_>>(), vec![bea]);
    assert_eq!(page.domains.len(), 1);
    assert!(page.domains[0].listings.is_empty() && page.domains[0].postmaster.is_none() && page.domains[0].verdict == Level::Good);

    // Through the API too, with Beta's session.
    let user: Uuid = sqlx::query_scalar("insert into users (email, name, password_hash) values ('bea@beta.test', 'Bea', 'x') returning id").fetch_one(&db).await.unwrap();
    sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(beta).bind(user).execute(&db).await.unwrap();
    sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
        .bind(sha256_hex("session-beta"))
        .bind(user)
        .bind(beta)
        .execute(&db)
        .await
        .unwrap();
    let base = serve(state).await;
    let body: Value = reqwest::Client::new()
        .get(format!("{base}/deliverability"))
        .header("cookie", "splee_session=session-beta")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["mailboxes"].as_array().unwrap().len(), 1);
    assert_eq!(body["mailboxes"][0]["email"], "bea@shared.test");
    assert_eq!(body["mailboxes"][0]["verdict"], "good");
    assert_eq!(body["postmaster"], json!({ "available": true, "connection": null }));
}
