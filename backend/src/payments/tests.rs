use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::http::{HeaderMap, HeaderValue};
use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

use super::fake::{self, Fake, LEMON_KEY, PADDLE_KEY, STRIPE_KEY};
use super::{Provider, backfill, lemonsqueezy, match_lead, paddle, stripe};
use crate::config::Config;
use crate::crypto::sha256_hex;
use crate::state::AppState;

const SECRET: &str = "whsec_test_secret";

fn hmac_hex(secret: &str, data: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(data);
    hex::encode(mac.finalize().into_bytes())
}

fn stripe_header(secret: &str, body: &[u8], t: i64) -> String {
    format!("t={t},v1={}", hmac_hex(secret, &[t.to_string().as_bytes(), b".", body].concat()))
}

fn paddle_header(secret: &str, body: &[u8], ts: i64) -> String {
    format!("ts={ts};h1={}", hmac_hex(secret, &[ts.to_string().as_bytes(), b":", body].concat()))
}

#[test]
fn signatures_are_checked_for_all_three() {
    let now = 1_790_000_000;
    let body = br#"{"id":"evt_1","type":"charge.succeeded"}"#;
    let good = stripe_header(SECRET, body, now);
    assert!(stripe::verify(SECRET, &good, body, now + 299));
    assert!(!stripe::verify("whsec_other", &good, body, now), "the wrong secret");
    assert!(!stripe::verify(SECRET, &good, br#"{"id":"evt_2"}"#, now), "a changed body");
    assert!(!stripe::verify(SECRET, &good, body, now + 301), "stale");
    assert!(!stripe::verify(SECRET, &good, body, now - 301), "from the future");
    assert!(!stripe::verify(SECRET, &good.replace("v1=", "v0="), body, now), "only v1 counts");
    assert!(!stripe::verify(SECRET, &good.replace(&format!("t={now},"), ""), body, now), "no timestamp");
    // While a secret is rolled, Stripe signs with both.
    let rolled = format!("t={now},v1={},{}", "cd".repeat(32), &good[good.find("v1=").unwrap()..]);
    assert!(stripe::verify(SECRET, &rolled, body, now));

    let good = paddle_header(SECRET, body, now);
    assert!(paddle::verify(SECRET, &good, body, now + 60));
    assert!(!paddle::verify("pdl_other", &good, body, now));
    assert!(!paddle::verify(SECRET, &good, b"{}", now));
    assert!(!paddle::verify(SECRET, &good, body, now + 301), "stale");
    assert!(!paddle::verify(SECRET, "h1=00", body, now), "no timestamp");

    let good = hmac_hex(SECRET, body);
    assert!(lemonsqueezy::verify(SECRET, &good, body));
    assert!(!lemonsqueezy::verify("other", &good, body));
    assert!(!lemonsqueezy::verify(SECRET, &good, b"{}"));
    assert!(!lemonsqueezy::verify(SECRET, "not hex", body));

    // Anyone could sign with an empty secret.
    let mut headers = HeaderMap::new();
    headers.insert("x-signature", HeaderValue::from_str(&hmac_hex("", body)).unwrap());
    assert!(!Provider::LemonSqueezy.verify("", &headers, body, now));
}

#[test]
fn events_become_payments_and_refunds() {
    let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
    // A Checkout one-off has no invoice and only the receipt address.
    let mut one_off = charge_succeeded("evt_1", "ch_1", 4800, "", at);
    one_off["data"]["object"]["receipt_email"] = json!("a@b.example");
    let c = &stripe::charges(&one_off)[0];
    assert_eq!((c.external_id.as_str(), c.amount_cents, c.currency.as_str(), c.paid_at), ("ch_1", 4800, "USD", at));
    assert_eq!((c.email.as_deref(), c.customer.as_deref()), (Some("a@b.example"), Some("cus_x")));
    let mut held = charge_succeeded("evt_2", "ch_2", 4800, "a@b.example", at);
    held["data"]["object"]["captured"] = json!(false);
    assert!(stripe::charges(&held).is_empty(), "an authorization isn't revenue until it's captured");
    held["type"] = json!("charge.captured");
    held["data"]["object"]["captured"] = json!(true);
    assert_eq!(stripe::charges(&held)[0].external_id, "ch_2");
    assert!(stripe::charges(&json!({"type": "customer.created", "data": {"object": {}}})).is_empty());
    let refund = &stripe::charges(&charge_refunded("evt_3", 1500, at))[0];
    assert_eq!((refund.kind, refund.external_id.as_str(), refund.amount_cents), (super::Kind::RefundTotal, "refund:ch_1", 1500));

    let renewal = |reason: &str| {
        json!({"meta": {"event_name": "subscription_payment_success"}, "data": {"id": "7", "attributes": {"user_email": "a@b.example", "currency": "EUR",
            "total": 900, "status": "paid", "billing_reason": reason, "created_at": "2026-11-01T10:00:00Z"}}})
    };
    assert!(lemonsqueezy::charges(&renewal("initial")).is_empty(), "the order counts the first payment");
    assert_eq!(lemonsqueezy::charges(&renewal("renewal"))[0].external_id, "subscription-invoice:7");

    let adjustment = |status: &str| {
        json!({"event_type": "adjustment.updated", "data": {"id": "adj_1", "action": "refund", "status": status, "currency_code": "USD",
            "customer_id": "ctm_1", "totals": {"total": "1500"}, "updated_at": "2026-11-02T10:00:00Z"}})
    };
    assert!(paddle::charges(&adjustment("pending_approval")).is_empty());
    let refund = &paddle::charges(&adjustment("approved"))[0];
    assert_eq!((refund.kind, refund.amount_cents, refund.customer.as_deref()), (super::Kind::Refund, 1500, Some("ctm_1")));
}

struct Seeded {
    state: AppState,
    base: String,
    http: reqwest::Client,
    fake: Fake,
    acme: Uuid,
    project: Uuid,
    beta_project: Uuid,
    /// Sarah at northwind.example, with an open deal from a hot reply, and Tom at contoso.example, without one.
    sarah: Uuid,
    tom: Uuid,
}

impl Seeded {
    /// Two organizations, Acme and Beta, each with an owner signed in as session "acme" or "beta" and a project. Beta has a
    /// lead with Sarah's address too.
    async fn start(db: PgPool) -> Self {
        let (fake_url, fake) = fake::start().await;
        let base_state = crate::autopilot::tests::state(db.clone());
        let cfg = Config { app_secret: "test-secret".into(), payments_api_url: fake_url, ..(*base_state.cfg).clone() };
        let state = AppState { cfg: Arc::new(cfg), ..base_state };
        let mut ids = vec![];
        for name in ["acme", "beta"] {
            let org: Uuid = sqlx::query_scalar("insert into organizations (name) values ($1) returning id").bind(name).fetch_one(&db).await.unwrap();
            let user: Uuid = sqlx::query_scalar("insert into users (email, password_hash) values ($1, 'x') returning id")
                .bind(format!("{name}@example.test"))
                .fetch_one(&db)
                .await
                .unwrap();
            sqlx::query("insert into memberships (org_id, user_id, role) values ($1, $2, 'owner')").bind(org).bind(user).execute(&db).await.unwrap();
            sqlx::query("insert into sessions (token_hash, user_id, org_id, expires_at) values ($1, $2, $3, now() + interval '1 day')")
                .bind(sha256_hex(name))
                .bind(user)
                .bind(org)
                .execute(&db)
                .await
                .unwrap();
            let project: Uuid = sqlx::query_scalar("insert into projects (org_id, name) values ($1, $2) returning id").bind(org).bind(name).fetch_one(&db).await.unwrap();
            ids.push((org, project));
        }
        let ((acme, project), (_, beta_project)) = (ids[0], ids[1]);
        let lead = |project: Uuid, name: &'static str, email: &'static str, domain: &'static str| {
            sqlx::query_as::<_, crate::models::Lead>(
                "insert into leads (project_id, full_name, email, company_name, company_domain, status) values ($1, $2, $3, $2, $4, 'hot') returning *",
            )
            .bind(project)
            .bind(name)
            .bind(email)
            .bind(domain)
        };
        let sarah = lead(project, "Sarah", "sarah@northwind.example", "northwind.example").fetch_one(&db).await.unwrap();
        let tom = lead(project, "Tom", "tom@contoso.example", "www.contoso.example").fetch_one(&db).await.unwrap();
        lead(project, "Gina", "gina@gmail.com", "gmail.com").fetch_one(&db).await.unwrap();
        lead(beta_project, "Sarah", "sarah@northwind.example", "northwind.example").fetch_one(&db).await.unwrap();
        crate::deals::hot_reply(&state, &sarah).await;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/api", listener.local_addr().unwrap());
        let app = Router::new().nest("/api", crate::routes::router()).with_state(state.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { state, base, http: reqwest::Client::new(), fake, acme, project, beta_project, sarah: sarah.id, tom: tom.id }
    }

    fn db(&self) -> &PgPool {
        &self.state.db
    }

    async fn call(&self, session: &str, method: reqwest::Method, path: &str, body: Value) -> (u16, Value) {
        let res = self.http.request(method, format!("{}{path}", self.base)).header("cookie", format!("splee_session={session}")).json(&body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    /// Connects the provider to Acme's project with its signing secret and returns the webhook token. The look back stays
    /// queued, since no worker runs in tests.
    async fn connect(&self, provider: &str, key: &str) -> String {
        let (code, list) = self.call("acme", reqwest::Method::PUT, &format!("/projects/{}/payments/{provider}", self.project), json!({ "api_key": key })).await;
        assert_eq!(code, 200, "{list}");
        let path = format!("/projects/{}/payments/{provider}/secret", self.project);
        let (code, list) = self.call("acme", reqwest::Method::PUT, &path, json!({ "secret": SECRET })).await;
        assert_eq!(code, 200, "{list}");
        let row = list.as_array().unwrap().iter().find(|c| c["provider"] == provider).unwrap().clone();
        assert_eq!((row["has_secret"].as_bool(), row["backfill_status"].as_str()), (Some(true), Some("queued")));
        row["webhook_token"].as_str().unwrap().to_string()
    }

    async fn deliver(&self, provider: &str, token: &str, event: &Value) -> (u16, Value) {
        let body = serde_json::to_vec(event).unwrap();
        let now = Utc::now().timestamp();
        let (name, value) = match provider {
            "stripe" => ("stripe-signature", stripe_header(SECRET, &body, now)),
            "paddle" => ("paddle-signature", paddle_header(SECRET, &body, now)),
            _ => ("x-signature", hmac_hex(SECRET, &body)),
        };
        self.raw(provider, token, name, &value, body).await
    }

    async fn raw(&self, provider: &str, token: &str, header: &str, value: &str, body: Vec<u8>) -> (u16, Value) {
        let res = self.http.post(format!("{}/payments/{provider}/{token}", self.base)).header(header, value).body(body).send().await.unwrap();
        (res.status().as_u16(), res.json().await.unwrap_or_default())
    }

    async fn payments(&self, lead: Uuid) -> Vec<(String, String, i64, String)> {
        sqlx::query_as("select provider, external_id, amount_cents, currency from payments where lead_id = $1 order by paid_at, external_id")
            .bind(lead)
            .fetch_all(self.db())
            .await
            .unwrap()
    }

    /// The lead's deals as (status, value, currency, source).
    async fn deals(&self, lead: Uuid) -> Vec<(String, i64, String, String)> {
        sqlx::query_as("select status, value_cents, currency, source from deals where lead_id = $1 order by created_at")
            .bind(lead)
            .fetch_all(self.db())
            .await
            .unwrap()
    }

    async fn connection(&self, provider: &str) -> Uuid {
        sqlx::query_scalar("select id from payment_connections where project_id = $1 and provider = $2").bind(self.project).bind(provider).fetch_one(self.db()).await.unwrap()
    }
}

fn charge(id: &str, cents: i64, email: &str, paid_at: DateTime<Utc>) -> Value {
    json!({"id": id, "object": "charge", "status": "succeeded", "paid": true, "captured": true, "amount": cents, "amount_captured": cents,
        "amount_refunded": 0, "currency": "usd", "customer": "cus_x", "billing_details": {"email": email}, "created": paid_at.timestamp()})
}

fn charge_succeeded(event: &str, id: &str, cents: i64, email: &str, paid_at: DateTime<Utc>) -> Value {
    json!({"id": event, "type": "charge.succeeded", "created": paid_at.timestamp(), "data": {"object": charge(id, cents, email, paid_at)}})
}

/// Sarah's $4,800 charge ch_1 with `refunded` refunded so far, the latest at `at`.
fn charge_refunded(event: &str, refunded: i64, at: DateTime<Utc>) -> Value {
    let mut c = charge("ch_1", 480_000, "sarah@northwind.example", at - chrono::Duration::days(1));
    c["amount_refunded"] = json!(refunded);
    json!({"id": event, "type": "charge.refunded", "created": at.timestamp(), "data": {"object": c}})
}

/// Each of the lead's refund rows as (month, external id, amount).
async fn refunds(db: &PgPool, lead: Uuid) -> Vec<(String, String, i64)> {
    sqlx::query_as("select to_char(paid_at, 'YYYY-MM'), external_id, amount_cents from payments where lead_id = $1 and kind = 'refund' order by paid_at")
        .bind(lead)
        .fetch_all(db)
        .await
        .unwrap()
}

/// The project's deal events that reached the webhook log, waiting for the ones sent in the background.
async fn deal_events(db: &PgPool, project: Uuid, at_least: usize) -> Vec<(String, String)> {
    let mut sent = vec![];
    for _ in 0..50 {
        sent = sqlx::query_as("select event, payload->'lead'->>'email' from webhook_deliveries where project_id = $1 order by created_at")
            .bind(project)
            .fetch_all(db)
            .await
            .unwrap();
        if sent.len() >= at_least {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    sent
}

async fn log_deal_events(db: &PgPool, project: Uuid) {
    sqlx::query("insert into project_settings (project_id, webhook_events) values ($1, '{deal.created,deal.stage_changed,deal.won,deal.lost}')")
        .bind(project)
        .execute(db)
        .await
        .unwrap();
    // Refused before connecting, as it isn't public, so nothing leaves the machine and the delivery log still keeps the payload.
    sqlx::query("insert into webhook_endpoints (project_id, url) values ($1, 'http://127.0.0.1/hook')").bind(project).execute(db).await.unwrap();
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_first_payment_wins_the_deal_and_later_ones_add_up(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    log_deal_events(&db, s.project).await;
    let token = s.connect("stripe", STRIPE_KEY).await;
    let paid_at = Utc::now() - chrono::Duration::hours(2);

    let first = charge_succeeded("evt_1", "ch_1", 480_000, "Sarah@Northwind.example", paid_at);
    assert_eq!(s.deliver("stripe", &token, &first).await, (200, json!({ "matched": 1 })));
    assert_eq!(s.deliver("stripe", &token, &first).await, (200, json!({ "duplicate": true })));
    // Stripe may send the same charge again under another event.
    let mut captured = charge_succeeded("evt_2", "ch_1", 480_000, "sarah@northwind.example", paid_at);
    captured["type"] = json!("charge.captured");
    assert_eq!(s.deliver("stripe", &token, &captured).await.0, 200);
    assert_eq!(s.deals(s.sarah).await, [("won".into(), 480_000, "USD".into(), "hot_reply".into())]);
    let closed: DateTime<Utc> = sqlx::query_scalar("select closed_at from deals where lead_id = $1").bind(s.sarah).fetch_one(&db).await.unwrap();
    assert_eq!(closed.timestamp(), paid_at.timestamp(), "the deal closes when they paid");

    // A colleague pays the next invoice. It adds to Sarah's revenue and leaves the deal alone.
    s.deliver("stripe", &token, &charge_succeeded("evt_3", "ch_2", 120_000, "finance@northwind.example", paid_at + chrono::Duration::minutes(10))).await;
    // Two partial refunds of the first charge, each with the total so far, then the first again, late.
    let hours_ago = |h: i64| Utc::now() - chrono::Duration::hours(h);
    s.deliver("stripe", &token, &charge_refunded("evt_4", 20_000, hours_ago(1))).await;
    s.deliver("stripe", &token, &charge_refunded("evt_5", 50_000, hours_ago(0))).await;
    s.deliver("stripe", &token, &charge_refunded("evt_6", 20_000, hours_ago(1))).await;
    let payments = s.payments(s.sarah).await;
    let kept = [("ch_1", 480_000), ("ch_2", 120_000), ("refund:ch_1:20000", -20_000), ("refund:ch_1:50000", -30_000)];
    assert_eq!(payments.iter().map(|p| (p.1.as_str(), p.2)).collect::<Vec<_>>(), kept);
    assert_eq!(payments.iter().map(|p| p.2).sum::<i64>(), 550_000);
    assert_eq!(s.deals(s.sarah).await, [("won".into(), 480_000, "USD".into(), "hot_reply".into())]);

    // Tom has no deal, so his first payment, through Paddle, makes a won one. Paddle's transaction only has the customer's id.
    let paddle = s.connect("paddle", PADDLE_KEY).await;
    s.fake.customers.lock().unwrap().insert("ctm_tom".into(), "tom@contoso.example".into());
    let completed = json!({"event_id": "evt_p1", "event_type": "transaction.completed", "data": {"id": "txn_1", "status": "completed", "customer_id": "ctm_tom",
        "currency_code": "EUR", "billed_at": "2026-11-10T09:00:00Z", "details": {"totals": {"total": "99000", "grand_total": "99000"}}}});
    assert_eq!(s.deliver("paddle", &paddle, &completed).await, (200, json!({ "matched": 1 })));
    assert_eq!(s.deals(s.tom).await, [("won".into(), 99_000, "EUR".into(), "paddle".into())]);
    assert_eq!(s.fake.hits("/paddle/customers/ctm_tom"), 1);
    let (sarah, tom) = (crate::deals::for_lead(&db, s.sarah).await.unwrap().unwrap(), crate::deals::for_lead(&db, s.tom).await.unwrap().unwrap());
    assert_eq!((sarah.paid_through.as_deref(), tom.paid_through.as_deref()), (Some("stripe"), Some("paddle")));

    let events = deal_events(&db, s.project, 4).await;
    let won: Vec<&str> = events.iter().filter(|(e, _)| e == "deal.won").map(|(_, email)| email.as_str()).collect();
    assert_eq!(won, ["sarah@northwind.example", "tom@contoso.example"], "deal.won fires once per lead");
    // Sarah's hot reply came before the webhook was set up. Each event is sent on its own task, so they're logged in any order.
    let mut sent: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
    sent.sort();
    assert_eq!(sent, ["deal.created", "deal.stage_changed", "deal.won", "deal.won"]);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn payments_match_by_address_then_company_but_not_free_mail(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    let gina: Uuid = sqlx::query_scalar("select id from leads where email = 'gina@gmail.com'").fetch_one(&db).await.unwrap();
    let mut found = vec![];
    for email in ["SARAH@northwind.example", "ap@northwind.example", "ap@contoso.example", "gina@gmail.com", "someone@gmail.com", "ap@elsewhere.example"] {
        found.push(match_lead(s.db(), s.project, email).await.unwrap());
    }
    // www. on Tom's company domain doesn't matter, and Gina's company domain is a free mail one, so only her address matches.
    assert_eq!(found, [Some(s.sarah), Some(s.sarah), Some(s.tom), Some(gina), None, None]);

    // A Lemon Squeezy order from a stranger at Gmail isn't kept anywhere, and Beta's Sarah never gets Acme's payments.
    let token = s.connect("lemonsqueezy", LEMON_KEY).await;
    let order = |id: &str, email: &str| {
        json!({"meta": {"event_name": "order_created", "test_mode": false}, "data": {"type": "orders", "id": id, "attributes": {"user_email": email,
            "currency": "USD", "total": 4900, "status": "paid", "refunded_amount": 0, "created_at": "2026-11-11T10:00:00Z"}}})
    };
    assert_eq!(s.deliver("lemonsqueezy", &token, &order("1", "someone@gmail.com")).await, (200, json!({ "matched": 0 })));
    assert_eq!(s.deliver("lemonsqueezy", &token, &order("2", "sarah@northwind.example")).await, (200, json!({ "matched": 1 })));
    assert_eq!(s.deliver("lemonsqueezy", &token, &order("2", "sarah@northwind.example")).await, (200, json!({ "duplicate": true })));
    let kept: Vec<(Uuid, String)> = sqlx::query_as("select project_id, external_id from payments").fetch_all(&db).await.unwrap();
    assert_eq!(kept, [(s.project, "order:2".to_string())]);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn the_look_back_pages_and_never_counts_twice(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    log_deal_events(&db, s.project).await;
    let days_ago = |d: i64| Utc::now() - chrono::Duration::days(d);
    let mut failed = charge("ch_2", 40_000, "sarah@northwind.example", days_ago(30));
    failed["status"] = json!("failed");
    *s.fake.stripe_charges.lock().unwrap() = vec![
        charge("ch_5", 30_000, "sarah@northwind.example", days_ago(1)),
        charge("ch_4", 20_000, "ap@northwind.example", days_ago(10)),
        charge("ch_3", 70_000, "stranger@elsewhere.example", days_ago(20)),
        failed,
        charge("ch_1", 10_000, "sarah@northwind.example", days_ago(120)),
    ];
    let token = s.connect("stripe", STRIPE_KEY).await;
    // The webhook for one of them comes before the look back runs.
    s.deliver("stripe", &token, &charge_succeeded("evt_9", "ch_4", 20_000, "ap@northwind.example", days_ago(10))).await;
    let id = s.connection("stripe").await;
    backfill(&s.state, id).await.unwrap();
    backfill(&s.state, id).await.unwrap();
    let ids = |p: Vec<(String, String, i64, String)>| p.into_iter().map(|p| (p.1, p.2)).collect::<Vec<_>>();
    assert_eq!(ids(s.payments(s.sarah).await), [("ch_4".to_string(), 20_000), ("ch_5".to_string(), 30_000)]);
    assert_eq!(s.deals(s.sarah).await, [("won".into(), 20_000, "USD".into(), "hot_reply".into())]);
    let (status, found, matched): (String, i32, i32) =
        sqlx::query_as("select backfill_status, backfill_found, backfill_matched from payment_connections where id = $1").bind(id).fetch_one(&db).await.unwrap();
    assert_eq!((status.as_str(), found, matched), ("done", 3, 2), "the failed charge isn't a payment and the stranger matched nobody");
    // Four charges are in range, two a page, read twice.
    assert_eq!(s.fake.hits("/stripe/v1/charges?limit=100"), 4);

    // Paddle sends the customer with each transaction, so nobody is looked up one by one.
    s.fake.customers.lock().unwrap().insert("ctm_tom".into(), "tom@contoso.example".into());
    let transaction = |id: &str, cents: &str, d: i64| {
        json!({"id": id, "status": "completed", "customer_id": "ctm_tom", "currency_code": "USD", "billed_at": days_ago(d).to_rfc3339(),
            "details": {"totals": {"grand_total": cents}}})
    };
    *s.fake.paddle_transactions.lock().unwrap() = vec![transaction("txn_1", "5000", 40), transaction("txn_2", "7000", 20), transaction("txn_3", "9000", 2)];
    s.connect("paddle", PADDLE_KEY).await;
    backfill(&s.state, s.connection("paddle").await).await.unwrap();
    assert_eq!(ids(s.payments(s.tom).await), [("txn_1".to_string(), 5000), ("txn_2".to_string(), 7000), ("txn_3".to_string(), 9000)]);
    assert_eq!(s.deals(s.tom).await, [("won".into(), 5000, "USD".into(), "paddle".into())], "the oldest payment is the first");
    assert_eq!((s.fake.hits("/paddle/transactions?status=completed"), s.fake.hits("/paddle/customers/")), (2, 0));

    // Lemon Squeezy: three orders over two pages, one partly refunded, and a subscription's renewal. Its first invoice is the
    // order's payment, so it's skipped.
    let sale = |id: &str, email: &str, total: i64, refunded: i64, reason: Option<&str>, d: i64| {
        json!({"id": id, "attributes": {"user_email": email, "currency": "USD", "total": total, "refunded_amount": refunded,
            "status": if refunded > 0 { "partial_refund" } else { "paid" }, "billing_reason": reason, "created_at": days_ago(d).to_rfc3339()}})
    };
    let gina: Uuid = sqlx::query_scalar("select id from leads where email = 'gina@gmail.com'").fetch_one(&db).await.unwrap();
    *s.fake.lemon_orders.lock().unwrap() =
        vec![sale("3", "gina@gmail.com", 4900, 0, None, 1), sale("2", "gina@gmail.com", 2900, 900, None, 5), sale("1", "gina@gmail.com", 1000, 0, None, 200)];
    *s.fake.lemon_invoices.lock().unwrap() = vec![sale("12", "gina@gmail.com", 4900, 0, Some("renewal"), 3), sale("11", "gina@gmail.com", 4900, 0, Some("initial"), 33)];
    s.connect("lemonsqueezy", LEMON_KEY).await;
    let lemon = s.connection("lemonsqueezy").await;
    backfill(&s.state, lemon).await.unwrap();
    backfill(&s.state, lemon).await.unwrap();
    let mut kept = ids(s.payments(gina).await);
    kept.sort();
    let expected = [("order:2", 2900), ("order:3", 4900), ("refund:order:2:900", -900), ("subscription-invoice:12", 4900)];
    assert_eq!(kept, expected.map(|(id, cents)| (id.to_string(), cents)));

    let events = deal_events(&db, s.project, 6).await;
    assert_eq!(events.iter().filter(|(e, _)| e == "deal.won").count(), 3, "one each for Sarah, Tom and Gina");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn keys_and_webhooks_are_checked_and_stay_in_the_organization(db: PgPool) {
    let seeded = Seeded::start(db.clone()).await;
    let s = &seeded;
    let put = move |session: &'static str, path: String, body: Value| async move { s.call(session, reqwest::Method::PUT, &path, body).await };
    let stripe = format!("/projects/{}/payments/stripe", s.project);

    let (code, err) = put("acme", stripe.clone(), json!({ "api_key": "sk_test_123" })).await;
    assert_eq!((code, err["error"].as_str().unwrap().starts_with("Use a restricted key")), (400, true));
    let (code, err) = put("acme", stripe.clone(), json!({ "api_key": "rk_test_wrong" })).await;
    assert_eq!((code, err["error"].as_str()), (400, Some("Stripe didn't accept that API key. Copy it again from Stripe.")));
    let (code, _) = put("acme", format!("/projects/{}/payments/square", s.project), json!({ "api_key": "x" })).await;
    assert_eq!(code, 400);
    // Another organization's project looks like one that doesn't exist.
    assert_eq!(put("beta", stripe.clone(), json!({ "api_key": STRIPE_KEY })).await.0, 404);
    assert_eq!(s.call("beta", reqwest::Method::GET, &format!("/projects/{}/payments", s.project), json!({})).await.0, 404);
    assert_eq!(s.call("beta", reqwest::Method::DELETE, &stripe, json!({})).await.0, 404);

    // Before the signing secret is saved, the webhook can't be checked, so it's refused.
    let (code, list) = put("acme", stripe.clone(), json!({ "api_key": STRIPE_KEY })).await;
    assert_eq!(code, 200);
    let token = list[0]["webhook_token"].as_str().unwrap().to_string();
    let stored: String = sqlx::query_scalar("select api_key_enc from payment_connections").fetch_one(&db).await.unwrap();
    assert!(!stored.is_empty() && !stored.contains(STRIPE_KEY), "the key is kept encrypted");
    assert!(list[0].get("api_key_enc").is_none() && list[0].get("signing_secret_enc").is_none());
    let event = serde_json::to_vec(&charge_succeeded("evt_1", "ch_1", 1000, "sarah@northwind.example", Utc::now())).unwrap();
    let now = Utc::now().timestamp();
    assert_eq!(s.raw("stripe", &token, "stripe-signature", &stripe_header(SECRET, &event, now), event.clone()).await.0, 400);
    assert_eq!(put("acme", format!("{stripe}/secret"), json!({ "secret": "nope" })).await.0, 400);
    assert_eq!(put("beta", format!("{stripe}/secret"), json!({ "secret": SECRET })).await.0, 404);
    assert_eq!(put("acme", format!("{stripe}/secret"), json!({ "secret": SECRET })).await.0, 200);

    let (t, signed) = (token.as_str(), event.clone());
    let send = move |header: String| {
        let body = signed.clone();
        async move { s.raw("stripe", t, "stripe-signature", &header, body).await }
    };
    assert_eq!(send(stripe_header("whsec_other", &event, now)).await.0, 401);
    assert_eq!(send(stripe_header(SECRET, &event, now - 600)).await.0, 401, "stale");
    assert_eq!(s.raw("stripe", "not-a-token", "stripe-signature", &stripe_header(SECRET, &event, now), event.clone()).await.0, 404);
    assert_eq!(s.raw("paddle", &token, "paddle-signature", &paddle_header(SECRET, &event, now), event.clone()).await.0, 404, "the token is Stripe's");
    assert_eq!(send(stripe_header(SECRET, &event, now)).await, (200, json!({ "matched": 1 })));

    let paddle = s.connect("paddle", PADDLE_KEY).await;
    let body = serde_json::to_vec(&json!({"event_id": "evt_p", "event_type": "transaction.created", "data": {}})).unwrap();
    assert_eq!(s.raw("paddle", &paddle, "paddle-signature", &paddle_header("other", &body, now), body.clone()).await.0, 401);
    assert_eq!(s.raw("paddle", &paddle, "paddle-signature", &paddle_header(SECRET, &body, now - 301), body.clone()).await.0, 401);
    assert_eq!(s.raw("paddle", &paddle, "paddle-signature", &paddle_header(SECRET, &body, now), body.clone()).await, (200, json!({ "matched": 0 })));
    let lemon = s.connect("lemonsqueezy", LEMON_KEY).await;
    assert_eq!(s.raw("lemonsqueezy", &lemon, "x-signature", &hmac_hex("other", &body), body.clone()).await.0, 401);

    // Beta sees none of it, and disconnecting keeps the payments.
    let (_, beta) = s.call("beta", reqwest::Method::GET, &format!("/projects/{}/payments", s.beta_project), json!({})).await;
    assert_eq!(beta, json!([]));
    let (code, left) = s.call("acme", reqwest::Method::DELETE, &stripe, json!({})).await;
    assert_eq!((code, left.as_array().unwrap().len()), (200, 2));
    assert_eq!(s.payments(s.sarah).await.len(), 1);
    let orgs: Vec<Uuid> = sqlx::query_scalar("select org_id from payments").fetch_all(&db).await.unwrap();
    assert_eq!(orgs, [s.acme], "Beta's Sarah has the same address but isn't Acme's lead");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_charge_without_an_invoice_counts(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    let token = s.connect("stripe", STRIPE_KEY).await;
    // A Payment Link or Checkout one-off: no invoice and no customer, only the address the receipt went to.
    let mut one_off = charge_succeeded("evt_1", "ch_1", 25_000, "", Utc::now());
    one_off["data"]["object"]["customer"] = Value::Null;
    one_off["data"]["object"]["receipt_email"] = json!("sarah@northwind.example");
    assert_eq!(s.deliver("stripe", &token, &one_off).await, (200, json!({ "matched": 1 })));
    assert_eq!(s.payments(s.sarah).await, [("stripe".into(), "ch_1".into(), 25_000, "USD".into())]);
    assert_eq!(s.deals(s.sarah).await, [("won".into(), 25_000, "USD".into(), "hot_reply".into())]);
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn a_refund_from_before_connecting_counts_once(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    let days_ago = |d: i64| Utc::now() - chrono::Duration::days(d);
    let mut refunded = charge("ch_1", 480_000, "sarah@northwind.example", days_ago(20));
    refunded["amount_refunded"] = json!(50_000);
    *s.fake.stripe_charges.lock().unwrap() = vec![refunded];
    let refund = |id: &str, cents: i64, status: &str, d: i64| json!({"id": id, "charge": "ch_1", "amount": cents, "status": status, "created": days_ago(d).timestamp()});
    *s.fake.stripe_refunds.lock().unwrap() = vec![refund("re_3", 30_000, "succeeded", 5), refund("re_2", 9_000, "failed", 8), refund("re_1", 20_000, "succeeded", 10)];
    let token = s.connect("stripe", STRIPE_KEY).await;
    backfill(&s.state, s.connection("stripe").await).await.unwrap();
    // Stripe resends the latest charge.refunded, then a third refund comes in.
    assert_eq!(s.deliver("stripe", &token, &charge_refunded("evt_1", 50_000, days_ago(5))).await.0, 200);
    s.deliver("stripe", &token, &charge_refunded("evt_2", 60_000, days_ago(0))).await;
    let kept: Vec<(String, i64)> = refunds(&db, s.sarah).await.into_iter().map(|r| (r.1, r.2)).collect();
    let expected = [("refund:ch_1:20000", -20_000), ("refund:ch_1:50000", -30_000), ("refund:ch_1:60000", -10_000)];
    assert_eq!(kept, expected.map(|(id, cents)| (id.to_string(), cents)));

    // Paddle's look back asks for the adjustments of the transactions it found, and the webhook for one changes nothing.
    s.fake.customers.lock().unwrap().insert("ctm_tom".into(), "tom@contoso.example".into());
    *s.fake.paddle_transactions.lock().unwrap() = vec![json!({"id": "txn_1", "status": "completed", "customer_id": "ctm_tom", "currency_code": "USD",
        "billed_at": days_ago(20).to_rfc3339(), "details": {"totals": {"grand_total": "9000"}}})];
    let adjustment = |id: &str, transaction: &str, status: &str| {
        json!({"id": id, "transaction_id": transaction, "action": "refund", "status": status, "currency_code": "USD", "customer_id": "ctm_tom",
            "totals": {"total": "2000"}, "updated_at": days_ago(3).to_rfc3339()})
    };
    *s.fake.paddle_adjustments.lock().unwrap() =
        vec![adjustment("adj_1", "txn_1", "approved"), adjustment("adj_2", "txn_1", "rejected"), adjustment("adj_3", "txn_old", "approved")];
    let paddle = s.connect("paddle", PADDLE_KEY).await;
    backfill(&s.state, s.connection("paddle").await).await.unwrap();
    let event = json!({"event_id": "evt_p1", "event_type": "adjustment.updated", "data": adjustment("adj_1", "txn_1", "approved")});
    assert_eq!(s.deliver("paddle", &paddle, &event).await, (200, json!({ "matched": 1 })));
    let tom: Vec<(String, i64)> = s.payments(s.tom).await.into_iter().map(|p| (p.1, p.2)).collect();
    assert_eq!(tom, [("txn_1".to_string(), 9000), ("adj_1".to_string(), -2000)]);
    assert_eq!(s.fake.hits("/paddle/customers/"), 0, "the refund takes the transaction's customer");
}

// Ignored by default: sqlx::test creates throwaway databases on the DATABASE_URL server, which may come from .env. See CONTRIBUTING.md.
#[sqlx::test]
#[ignore = "needs Postgres"]
async fn partial_refunds_land_in_the_month_they_happened(db: PgPool) {
    let s = Seeded::start(db.clone()).await;
    let token = s.connect("stripe", STRIPE_KEY).await;
    let at = |day: &str| DateTime::parse_from_rfc3339(&format!("{day}T12:00:00Z")).unwrap().with_timezone(&Utc);
    s.deliver("stripe", &token, &charge_succeeded("evt_1", "ch_1", 480_000, "sarah@northwind.example", at("2026-08-20"))).await;
    s.deliver("stripe", &token, &charge_refunded("evt_2", 100_000, at("2026-09-10"))).await;
    s.deliver("stripe", &token, &charge_refunded("evt_3", 250_000, at("2026-10-15"))).await;
    let expected = [("2026-09", "refund:ch_1:100000", -100_000), ("2026-10", "refund:ch_1:250000", -150_000)];
    assert_eq!(refunds(&db, s.sarah).await, expected.map(|(month, id, cents)| (month.to_string(), id.to_string(), cents)));
}
