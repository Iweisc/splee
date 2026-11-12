use std::collections::HashMap;
use std::net::IpAddr;

use chrono::{Duration, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use super::*;
use crate::visitors::Kind;

const BASE: &str = "https://track.getacme.example";

/// The test config's PUBLIC_URL and unsubscribe address are both http://localhost:3349.
fn lazy_state() -> AppState {
    crate::autopilot::tests::state(PgPool::connect_lazy("postgres://localhost/unused").unwrap())
}

#[tokio::test]
async fn links_are_tracked_except_unsubscribe_mailto_and_the_quoted_thread() {
    let cfg = lazy_state().cfg;
    let body = "Hi Sam,\n\nPricing is at https://acme.example/pricing. Or see (https://acme.example/case-studies), or write to \
                mailto:jo@acme.example or call tel:+15550100.\nBook a time: http://localhost:3349/book/jo\n\n\
                On Tue, Nov 10, 2026 at 9:00 AM Sam Lee <sam@lee.example> wrote:\n> Got a link? https://quoted.example/x\n";
    let r = rewrite(body, BASE, true, |u| left_alone(u, &cfg));
    let urls: Vec<&str> = r.links.iter().map(|l| l.url.as_str()).collect();
    assert_eq!(urls, ["https://acme.example/pricing", "https://acme.example/case-studies", "http://localhost:3349/book/jo"]);
    let (pricing, cases) = (&r.links[0].token, &r.links[1].token);
    assert_eq!(pricing.len(), 64);
    assert!(r.text.contains(&format!("Pricing is at {BASE}/c/{pricing}. Or see ({BASE}/c/{cases}), or write")), "{}", r.text);
    for kept in ["mailto:jo@acme.example", "tel:+15550100", "> Got a link? https://quoted.example/x"] {
        assert!(r.text.contains(kept), "{kept} in {}", r.text);
    }
    // The HTML shows each link's own address and goes through the tracked one, and leaves the quoted thread alone.
    assert!(r.html.contains(&format!("<a href=\"{BASE}/c/{pricing}\">https://acme.example/pricing</a>.")), "{}", r.html);
    assert!(r.html.contains("<a href=\"https://quoted.example/x\">https://quoted.example/x</a>"), "{}", r.html);
    assert!(r.html.contains("Sam Lee &lt;sam@lee.example&gt; wrote:<br>"), "{}", r.html);

    // The unsubscribe footer, both its page and its one-click address, keeps working without Splee in the way.
    let footer = "Worth a chat? https://acme.example\n\n--\nNot relevant? Unsubscribe: http://localhost:3349/unsubscribe/abc123";
    let r = rewrite(footer, BASE, true, |u| left_alone(u, &cfg));
    assert_eq!(r.links.len(), 1);
    assert!(r.text.ends_with("Unsubscribe: http://localhost:3349/unsubscribe/abc123"), "{}", r.text);
    assert!(left_alone("http://localhost:3349/api/unsubscribe/abc123", &cfg));

    // Gmail wraps a long "On ... wrote:" line, and the quote still starts at its first piece.
    let wrapped = "Thanks!\n\nOn Tue, Nov 10, 2026 at 9:00 AM Sam Lee <\nsam@lee.example> wrote:\nhttps://quoted.example/y";
    assert!(rewrite(wrapped, BASE, true, |_| false).links.is_empty());

    // Click tracking off: the text goes out as written, and the HTML still links the links.
    let off = rewrite(body, BASE, false, |_| false);
    assert_eq!((off.text.as_str(), off.links.len()), (body, 0));
    assert!(off.html.contains("<a href=\"https://acme.example/pricing\">"));
}

#[test]
fn the_pixel_is_a_1x1_gif() {
    assert!(PIXEL.starts_with(b"GIF89a\x01\x00\x01\x00"));
    assert_eq!(PIXEL.len(), 43);
    let doc = html_document("Hi", Some("https://track.getacme.example/o/abc.gif"));
    assert!(doc.contains("<img src=\"https://track.getacme.example/o/abc.gif\" width=\"1\" height=\"1\""), "{doc}");
    assert!(!html_document("Hi", None).contains("<img"));
}

#[test]
fn scanners_head_requests_early_hits_gateways_and_clouds_are_filtered() {
    let sent = Utc::now() - Duration::hours(1);
    let person = Seen {
        head: false,
        user_agent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 14_5) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Safari/605.1.15",
        ip: "81.2.69.160".parse().ok(),
        network: Kind::Isp,
        at: Utc::now(),
    };
    assert_eq!(filter_reason(&person, Some(sent)), None);
    // Gmail fetches images through its proxy when the person opens the email, so that counts.
    let gmail = Seen { user_agent: "Mozilla/5.0 (Windows NT 5.1; rv:11.0) Gecko Firefox/11.0 (via ggpht.com GoogleImageProxy)", ..person };
    assert_eq!(filter_reason(&gmail, Some(sent)), None);

    let reason = |seen: Seen| filter_reason(&seen, Some(sent));
    let base = || Seen { head: false, user_agent: person.user_agent, ip: person.ip, network: Kind::Isp, at: Utc::now() };
    assert_eq!(reason(Seen { head: true, ..base() }), Some("head_request"));
    assert_eq!(filter_reason(&base(), Some(Utc::now() - Duration::seconds(30))), Some("too_soon"));
    for agent in ["Mozilla/5.0 (compatible; Barracuda Sentinel)", "Slackbot-LinkExpanding 1.0", "python-requests/2.31", "SkypeUriPreview Preview/0.5", ""] {
        assert_eq!(reason(Seen { user_agent: agent, ..base() }), Some("scanner_agent"), "{agent}");
    }
    for ip in ["40.107.22.5", "148.163.140.10", "205.139.110.61", "2a01:111:f400:7e1b::1"] {
        assert_eq!(reason(Seen { ip: ip.parse().ok(), ..base() }), Some("security_gateway"), "{ip}");
    }
    assert_eq!(reason(Seen { ip: "40.106.0.1".parse().ok(), ..base() }), None);
    assert_eq!(reason(Seen { network: Kind::Hosting, ..base() }), Some("cloud"));
    // A VPN or a phone network is a person.
    assert_eq!(reason(Seen { network: Kind::Vpn, ..base() }), None);
}

#[test]
fn links_are_named_by_their_last_path_part() {
    assert_eq!(link_name("https://www.acme.example/pricing?ref=email"), "pricing");
    assert_eq!(link_name("https://acme.example/blog/case-studies/"), "case studies");
    assert_eq!(link_name("https://acme.example/"), "acme.example");
    assert_eq!(link_label("https://www.acme.example/pricing/?ref=email"), "acme.example/pricing");
    assert_eq!(cname_target("https://app.splee.example/"), Some("app.splee.example".into()));
    assert_eq!(cname_target("http://localhost:3333"), None);
    assert_eq!(cname_target("http://10.0.0.5:3333"), None);
}

#[derive(Default)]
struct FakeDns {
    cnames: HashMap<&'static str, &'static str>,
    addresses: HashMap<&'static str, Vec<IpAddr>>,
}

impl Resolve for FakeDns {
    async fn cnames(&self, name: &str) -> Vec<String> {
        self.cnames.get(name).map(|c| vec![c.to_string()]).unwrap_or_default()
    }

    async fn addresses(&self, name: &str) -> Vec<IpAddr> {
        self.addresses.get(name).cloned().unwrap_or_default()
    }
}

#[tokio::test]
async fn a_tracking_host_needs_a_cname_to_splee_or_its_addresses() {
    let ip = |s: &str| s.parse::<IpAddr>().unwrap();
    let dns = FakeDns {
        cnames: [("track.getacme.example", "App.Splee.Example."), ("track.tryacme.example", "proxy.other.example.")].into(),
        addresses: [("app.splee.example", vec![ip("198.51.100.7")]), ("track.flat.example", vec![ip("198.51.100.7")]), ("track.wrong.example", vec![ip("192.0.2.1")])]
            .into(),
    };
    assert_eq!(check_host(&dns, "track.getacme.example", "app.splee.example").await, Ok(()));
    // Cloudflare answers with the addresses of a CNAME it flattens.
    assert_eq!(check_host(&dns, "track.flat.example", "app.splee.example").await, Ok(()));
    let elsewhere = check_host(&dns, "track.tryacme.example", "app.splee.example").await.unwrap_err();
    assert_eq!(elsewhere, "track.tryacme.example points to proxy.other.example, not app.splee.example");
    for host in ["track.wrong.example", "track.missing.example"] {
        let missing = check_host(&dns, host, "app.splee.example").await.unwrap_err();
        assert_eq!(missing, format!("No CNAME record for {host} yet. Add one pointing to app.splee.example"));
    }
}

struct Seeded {
    org: Uuid,
    project: Uuid,
    segment: Uuid,
    lead: Uuid,
    mailbox: Mailbox,
}

async fn seed(db: &PgPool) -> Seeded {
    let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Acme') returning id").fetch_one(db).await.unwrap();
    let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, 'Acme') returning id").bind(org).fetch_one(db).await.unwrap();
    sqlx::query("insert into project_settings (project_id) values ($1)").bind(project).execute(db).await.unwrap();
    let segment: Uuid =
        sqlx::query_scalar("insert into segments (project_id, name) values ($1, 'Dentists') returning id").bind(project).fetch_one(db).await.unwrap();
    let lead: Uuid = sqlx::query_scalar("insert into leads (project_id, segment_id, full_name, email, status) values ($1, $2, 'Sam Lee', 'sam@lee.example', 'contacted') returning id")
        .bind(project)
        .bind(segment)
        .fetch_one(db)
        .await
        .unwrap();
    let mailbox = mailbox(db, org, "jo@getacme.example").await;
    Seeded { org, project, segment, lead, mailbox }
}

async fn mailbox(db: &PgPool, org: Uuid, email: &str) -> Mailbox {
    sqlx::query_as("insert into mailboxes (org_id, email, smtp_host, smtp_username, smtp_password_enc) values ($1, $2, 'smtp.test', '', '') returning *")
        .bind(org)
        .bind(email)
        .fetch_one(db)
        .await
        .unwrap()
}

async fn email(db: &PgPool, s: &Seeded, kind: &str, status: &str, sent_ago_minutes: i64) -> EmailMessage {
    sqlx::query_as(
        "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, direction, kind, status, subject, body, sent_at)
         values ($1, $2, $3, $4, 'outbound', $5, $6, 'Hi Sam', 'Pricing: https://acme.example/pricing', now() - make_interval(mins => $7)) returning *",
    )
    .bind(s.project)
    .bind(s.lead)
    .bind(s.segment)
    .bind(s.mailbox.id)
    .bind(kind)
    .bind(status)
    .bind(sent_ago_minutes as i32)
    .fetch_one(db)
    .await
    .unwrap()
}

async fn verify(db: &PgPool, org: Uuid, domain: &str) {
    sqlx::query("insert into tracking_domains (org_id, domain, host, verified_at, checked_at) values ($1, $2, 'track.' || $2, now(), now())")
        .bind(org)
        .bind(domain)
        .execute(db)
        .await
        .unwrap();
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn each_mailbox_uses_its_own_domains_verified_tracking_host(db: PgPool) {
    let s = seed(&db).await;
    let other_org: Uuid = sqlx::query_scalar("insert into organizations (name) values ('Other') returning id").fetch_one(&db).await.unwrap();
    let unverified = mailbox(&db, s.org, "al@tryacme.example").await;
    verify(&db, s.org, "getacme.example").await;
    sqlx::query("insert into tracking_domains (org_id, domain, host) values ($1, 'tryacme.example', 'track.tryacme.example')").bind(s.org).execute(&db).await.unwrap();
    // Another organization proving the same domain doesn't move this one's links.
    verify(&db, other_org, "tryacme.example").await;

    assert_eq!(base_for(&db, "http://localhost:3349/", &s.mailbox).await.unwrap(), "https://track.getacme.example");
    assert_eq!(base_for(&db, "http://localhost:3349/", &unverified).await.unwrap(), "http://localhost:3349");
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn campaign_settings_decide_the_links_and_the_pixel(db: PgPool) {
    let state = crate::autopilot::tests::state(db.clone());
    let s = seed(&db).await;
    verify(&db, s.org, "getacme.example").await;
    let first = email(&db, &s, "initial", "sending", 0).await;
    let body = "Pricing: https://acme.example/pricing\n\n--\nNot relevant? Unsubscribe: http://localhost:3349/unsubscribe/abc";
    let links = |id: Uuid| sqlx::query_as::<_, (String, String)>("select token, url from tracked_links where email_id = $1 order by position").bind(id).fetch_all(&db);
    let flags = |id: Uuid| sqlx::query_as::<_, (bool, bool, Option<String>)>("select track_clicks, track_opens, open_token from email_messages where id = $1").bind(id).fetch_one(&db);

    // By default clicks are tracked and the email stays plain text.
    let p = prepare(&state, &first, &s.mailbox, body).await.unwrap();
    let saved = links(first.id).await.unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(p.text, format!("Pricing: https://track.getacme.example/c/{}\n\n--\nNot relevant? Unsubscribe: http://localhost:3349/unsubscribe/abc", saved[0].0));
    assert!(p.html.is_none());
    assert_eq!(flags(first.id).await.unwrap(), (true, false, None));

    // With opens on, a send tried again gets new tokens and an HTML part with the pixel.
    sqlx::query("update segments set track_opens = true where id = $1").bind(s.segment).execute(&db).await.unwrap();
    let p = prepare(&state, &first, &s.mailbox, body).await.unwrap();
    let again = links(first.id).await.unwrap();
    assert_eq!(again.len(), 1);
    assert_ne!(again[0].0, saved[0].0);
    let (_, _, token) = flags(first.id).await.unwrap();
    let html = p.html.unwrap();
    assert!(html.contains(&format!("src=\"https://track.getacme.example/o/{}.gif\"", token.unwrap())), "{html}");
    assert!(html.contains(&format!("href=\"https://track.getacme.example/c/{}\"", again[0].0)), "{html}");

    // Follow-ups follow the campaign. Replies go out as written.
    let follow_up = email(&db, &s, "follow_up", "sending", 0).await;
    assert!(prepare(&state, &follow_up, &s.mailbox, body).await.unwrap().html.is_some());
    let reply = email(&db, &s, "manual_reply", "sending", 0).await;
    let p = prepare(&state, &reply, &s.mailbox, body).await.unwrap();
    assert_eq!((p.text.as_str(), p.html.is_none()), (body, true));
    assert!(links(reply.id).await.unwrap().is_empty());

    // Both off: nothing changes.
    sqlx::query("update segments set track_opens = false, track_clicks = false where id = $1").bind(s.segment).execute(&db).await.unwrap();
    let p = prepare(&state, &first, &s.mailbox, body).await.unwrap();
    assert_eq!((p.text.as_str(), p.html.is_none()), (body, true));
    assert_eq!(flags(first.id).await.unwrap(), (false, false, None));
}

fn hit(email_id: Uuid, kind: &'static str, link: Option<&str>, agent: &str, at: Ts) -> Hit {
    Hit {
        kind,
        email_id,
        link_token: link.map(String::from),
        head: false,
        user_agent: agent.into(),
        ip: None,
        ip_hash: String::new(),
        at,
    }
}

const SAFARI: &str = "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1";

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn each_email_keeps_its_first_open_and_click_and_counts_only_people(db: PgPool) {
    let state = crate::autopilot::tests::state(db.clone());
    let s = seed(&db).await;
    let sent = email(&db, &s, "initial", "sent", 120).await;
    let sent_at = sent.sent_at.unwrap();
    for (token, url) in [("pricing-token", "https://acme.example/pricing"), ("cases-token", "https://acme.example/case-studies")] {
        sqlx::query("insert into tracked_links (token, email_id, position, url) values ($1, $2, 0 + (select count(*) from tracked_links)::int, $3)")
            .bind(token)
            .bind(sent.id)
            .bind(url)
            .execute(&db)
            .await
            .unwrap();
    }
    let at = |minutes: i64| sent_at + Duration::minutes(minutes);
    let pool = &db;
    let record = |h: Hit, network: Kind| async move { super::record(pool, &h, network).await.unwrap().unwrap() };

    // The scanner opens at once, the person twice later, and once from a cloud.
    record(hit(sent.id, "open", None, SAFARI, sent_at + Duration::seconds(5)), Kind::Unknown).await;
    let first = record(hit(sent.id, "open", None, SAFARI, at(30)), Kind::Isp).await;
    record(hit(sent.id, "open", None, "Mozilla/5.0 (compatible; Proofpoint)", at(31)), Kind::Unknown).await;
    record(hit(sent.id, "open", None, SAFARI, at(32)), Kind::Hosting).await;
    record(hit(sent.id, "open", None, SAFARI, at(50)), Kind::Isp).await;
    // Pricing twice, then every link within a second, which is a link checker.
    record(hit(sent.id, "click", Some("pricing-token"), SAFARI, at(31)), Kind::Isp).await;
    record(hit(sent.id, "click", Some("pricing-token"), SAFARI, at(40)), Kind::Isp).await;
    record(hit(sent.id, "click", Some("pricing-token"), SAFARI, at(90)), Kind::Isp).await;
    record(hit(sent.id, "click", Some("cases-token"), SAFARI, at(90) + Duration::milliseconds(800)), Kind::Isp).await;
    let mut head = hit(sent.id, "click", Some("cases-token"), SAFARI, at(95));
    head.head = true;
    record(head, Kind::Isp).await;

    let reasons: Vec<(String, Option<String>)> =
        sqlx::query_as("select kind, filter_reason from tracking_hits order by created_at, kind").fetch_all(&db).await.unwrap();
    let filtered: Vec<&str> = reasons.iter().filter_map(|(_, r)| r.as_deref()).collect();
    assert_eq!(filtered, ["too_soon", "scanner_agent", "cloud", "link_burst", "link_burst", "head_request"]);

    let lead: Lead = sqlx::query_as("select * from leads where id = $1").bind(s.lead).fetch_one(&db).await.unwrap();
    let detail = serde_json::to_value(crate::routes::leads::detail(&db, lead).await.unwrap()).unwrap();
    let events: Vec<(&str, Option<&str>, i64, &str)> = detail["email_events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["kind"].as_str().unwrap(), e["link"].as_str(), e["count"].as_i64().unwrap(), e["first_at"].as_str().unwrap()))
        .collect();
    let first_at = |m: i64| serde_json::to_value(at(m)).unwrap().as_str().unwrap().to_string();
    assert_eq!(events, [("open", None, 2, first_at(30).as_str()), ("click", Some("acme.example/pricing"), 2, first_at(31).as_str())]);
    assert_eq!(detail["score_reasons"][0]["text"], "Clicked your pricing link twice", "{}", detail["score_reasons"]);

    // Only the first counted open announces itself, and a hit that was filtered says nothing.
    super::announce(&state, first).await.unwrap();

    // A link checker's two clicks, where the first one's network lookup takes longer than the second's whole run. The
    // second still sees the first, pending, and neither counts.
    let checker = |token: &str| hit(sent.id, "click", Some(token), SAFARI, at(200));
    let (slow, fast) = (checker("pricing-token"), checker("cases-token"));
    let slow_id = super::begin(&db, &slow).await.unwrap().unwrap();
    let fast_id = super::begin(&db, &fast).await.unwrap().unwrap();
    super::finish(&db, fast_id, &fast, Kind::Isp).await.unwrap();
    super::burst(&db, fast_id).await.unwrap();
    let reason = || sqlx::query_scalar::<_, Option<String>>("select filter_reason from tracking_hits where id = $1").bind(fast_id).fetch_one(&db);
    assert_eq!(reason().await.unwrap().as_deref(), Some("link_burst"), "filed before it would be announced");
    super::finish(&db, slow_id, &slow, Kind::Isp).await.unwrap();
    super::burst(&db, slow_id).await.unwrap();
    let reasons: Vec<Option<String>> =
        sqlx::query_scalar("select filter_reason from tracking_hits where id = any($1)").bind([slow_id, fast_id]).fetch_all(&db).await.unwrap();
    assert_eq!(reasons, [Some("link_burst".to_string()), Some("link_burst".to_string())]);
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn analytics_show_open_rates_only_where_opens_are_tracked(db: PgPool) {
    let s = seed(&db).await;
    let quiet: Uuid = sqlx::query_scalar("insert into segments (project_id, name, track_opens) values ($1, 'Gyms', false) returning id")
        .bind(s.project)
        .fetch_one(&db)
        .await
        .unwrap();
    let angle: Uuid = sqlx::query_scalar("insert into segment_variants (segment_id, name) values ($1, 'Short') returning id").bind(s.segment).fetch_one(&db).await.unwrap();
    // Two emails in each campaign: the dentists track opens and clicks, the gyms only clicks.
    let mut ids = vec![];
    for (segment, opens) in [(s.segment, true), (s.segment, true), (quiet, false), (quiet, false)] {
        let id: Uuid = sqlx::query_scalar(
            "insert into email_messages (project_id, lead_id, segment_id, mailbox_id, variant_id, direction, kind, status, sent_at, track_clicks, track_opens)
             values ($1, $2, $3, $4, case when $5 then $6 end, 'outbound', 'initial', 'sent', now() - interval '2 days', true, $5) returning id",
        )
        .bind(s.project)
        .bind(s.lead)
        .bind(segment)
        .bind(s.mailbox.id)
        .bind(opens)
        .bind(angle)
        .fetch_one(&db)
        .await
        .unwrap();
        ids.push(id);
    }
    let hit = |email: Uuid, kind: &'static str, reason: Option<&'static str>| {
        sqlx::query("insert into tracking_hits (project_id, email_id, lead_id, kind, filter_reason) values ($1, $2, $3, $4, $5)")
            .bind(s.project)
            .bind(email)
            .bind(s.lead)
            .bind(kind)
            .bind(reason)
            .execute(&db)
    };
    hit(ids[0], "open", None).await.unwrap();
    hit(ids[0], "open", None).await.unwrap();
    hit(ids[1], "open", Some("scanner_agent")).await.unwrap();
    hit(ids[1], "click", None).await.unwrap();
    hit(ids[2], "click", None).await.unwrap();
    hit(ids[3], "click", Some("too_soon")).await.unwrap();

    let a = serde_json::to_value(crate::routes::analytics::analytics(&db, s.project, &Default::default()).await.unwrap()).unwrap();
    let rates = |row: &Value| (row["opened"].as_i64().unwrap(), row["open_rate"].as_f64(), row["clicked"].as_i64().unwrap(), row["click_rate"].as_f64());
    assert_eq!(rates(&a["totals"]), (1, Some(0.5), 2, Some(0.5)));
    let campaign = |name: &str| a["campaigns"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap().clone();
    assert_eq!(rates(&campaign("Dentists")), (1, Some(0.5), 1, Some(0.5)));
    assert_eq!(rates(&campaign("Gyms")), (0, None, 1, Some(0.5)));
    assert_eq!(a["angles"].as_array().unwrap().len(), 1);
    assert_eq!((a["angles"][0]["name"].as_str(), rates(&a["angles"][0])), (Some("Short"), (1, Some(0.5), 1, Some(0.5))));
    assert_eq!(rates(&a["steps"][0]), (1, Some(0.5), 2, Some(0.5)));
    assert_eq!(rates(&a["mailboxes"][0]), (1, Some(0.5), 2, Some(0.5)));
}

#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_failed_check_stops_using_the_tracking_host(db: PgPool) {
    let s = seed(&db).await;
    let good = FakeDns { cnames: [("track.getacme.example", "app.splee.example")].into(), ..Default::default() };
    check_and_save(&db, &good, s.org, "getacme.example", "app.splee.example").await.unwrap();
    assert_eq!(base_for(&db, "http://localhost:3349", &s.mailbox).await.unwrap(), "https://track.getacme.example");
    check_and_save(&db, &FakeDns::default(), s.org, "getacme.example", "app.splee.example").await.unwrap();
    assert_eq!(base_for(&db, "http://localhost:3349", &s.mailbox).await.unwrap(), "http://localhost:3349");
    let error: String = sqlx::query_scalar("select error from tracking_domains").fetch_one(&db).await.unwrap();
    assert!(error.starts_with("No CNAME record for track.getacme.example"), "{error}");
}
