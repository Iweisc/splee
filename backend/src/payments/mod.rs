//! Stripe, Paddle and Lemon Squeezy, so outreach can be followed to the money it brings in. A payment from a lead, or from
//! someone at the lead's company, is kept against the lead, the first one wins their deal with its amount, and Analytics
//! counts the revenue. routes/payments.rs connects a provider with an API key and takes its signed webhooks. Connecting also
//! queues a look back over the last 90 days, which the worker here runs. Both go through `record`, which the provider's id
//! for the payment makes safe to repeat.

pub mod lemonsqueezy;
pub mod paddle;
pub mod stripe;

#[cfg(test)]
pub mod fake;
#[cfg(test)]
mod tests;

use std::time::Duration;

use chrono::{DateTime, Utc};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

use crate::deals;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Provider {
    Stripe,
    Paddle,
    LemonSqueezy,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Stripe => "stripe",
            Provider::Paddle => "paddle",
            Provider::LemonSqueezy => "lemonsqueezy",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Provider::Stripe => "Stripe",
            Provider::Paddle => "Paddle",
            Provider::LemonSqueezy => "Lemon Squeezy",
        }
    }

    pub fn parse(name: &str) -> Option<Provider> {
        [Provider::Stripe, Provider::Paddle, Provider::LemonSqueezy].into_iter().find(|p| p.name() == name)
    }

    /// What the key has to be able to read, for the message when the provider turns it down.
    fn reads(self) -> &'static str {
        match self {
            Provider::Stripe => "customers, charges and refunds",
            Provider::Paddle => "transactions, customers and adjustments",
            Provider::LemonSqueezy => "orders and subscription invoices",
        }
    }

    /// Paddle's sandbox keys only work on its sandbox API.
    fn api(self, key: &str) -> &'static str {
        match self {
            Provider::Stripe => stripe::API,
            Provider::Paddle if key.contains("_sdbx_") => paddle::SANDBOX_API,
            Provider::Paddle => paddle::API,
            Provider::LemonSqueezy => lemonsqueezy::API,
        }
    }

    /// Whether the webhook's signature header is the HMAC of `body` with the signing secret. `now` is in Unix seconds.
    pub fn verify(self, secret: &str, headers: &axum::http::HeaderMap, body: &[u8], now: i64) -> bool {
        let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default();
        // Anyone can sign with an empty key.
        if secret.is_empty() {
            return false;
        }
        match self {
            Provider::Stripe => stripe::verify(secret, header("stripe-signature"), body, now),
            Provider::Paddle => paddle::verify(secret, header("paddle-signature"), body, now),
            Provider::LemonSqueezy => lemonsqueezy::verify(secret, header("x-signature"), body),
        }
    }

    /// The event's id, which a repeated delivery shares. Lemon Squeezy's events have none, so the body stands in for it.
    pub fn event_id(self, event: &Value, body: &[u8]) -> Option<String> {
        match self {
            Provider::Stripe => event["id"].as_str().map(str::to_string),
            Provider::Paddle => event["event_id"].as_str().map(str::to_string),
            Provider::LemonSqueezy => Some(crate::crypto::sha256_hex(&String::from_utf8_lossy(body))),
        }
    }

    /// The payments and refunds in a webhook event. Events of other types have none.
    pub fn charges(self, event: &Value) -> Vec<Charge> {
        match self {
            Provider::Stripe => stripe::charges(event),
            Provider::Paddle => paddle::charges(event),
            Provider::LemonSqueezy => lemonsqueezy::charges(event),
        }
    }
}

/// A signed webhook older or newer than this is turned away, so a captured one can't be replayed later.
pub const MAX_SKEW_SECONDS: i64 = 300;

/// Whether `timestamp` is a Unix time within MAX_SKEW_SECONDS of `now`.
fn fresh(timestamp: &str, now: i64) -> bool {
    timestamp.trim().parse::<i64>().is_ok_and(|ts| (now - ts).abs() <= MAX_SKEW_SECONDS)
}

/// Whether `sig` is the hex HMAC-SHA256 of the parts, one after another, keyed with `secret`. The comparison is constant-time.
fn signed(secret: &str, parts: &[&[u8]], sig: &str) -> bool {
    let Ok(sig) = hex::decode(sig.trim()) else { return false };
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes a key of any length");
    parts.iter().for_each(|p| mac.update(p));
    mac.verify_slice(&sig).is_ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Payment,
    /// One refund, like a Paddle adjustment.
    Refund,
    /// Everything refunded on a sale so far, which Stripe and Lemon Squeezy report instead of each refund. `external_id` is
    /// the sale's ("refund:{charge id}"), and `record` keeps each increase as its own refund, dated when it happened.
    RefundTotal,
}

/// A payment or a refund as the provider reports it. `amount_cents` is positive either way, in the currency's minor units.
#[derive(Debug, Clone)]
pub struct Charge {
    pub external_id: String,
    pub kind: Kind,
    pub amount_cents: i64,
    pub currency: String,
    /// The payer's address when the object carries it, else it's looked up through `customer`.
    pub email: Option<String>,
    pub customer: Option<String>,
    pub paid_at: DateTime<Utc>,
}

/// A string field, None when it's missing or blank.
fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string)
}

/// An amount the providers send as a number or, like Paddle, as a string of one.
fn cents(v: &Value) -> Option<i64> {
    v.as_i64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
}

fn rfc3339(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str().and_then(|s| DateTime::parse_from_rfc3339(s).ok()).map(|d| d.with_timezone(&Utc))
}

/// An id that goes into a provider's URL, which only gets the characters the providers use for them.
fn path_id(id: &str) -> Option<&str> {
    (!id.is_empty() && id.len() <= 100 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')).then_some(id)
}

/// How many times a request is tried when the provider says to slow down, and the longest wait between tries.
const TRIES: u32 = 5;
const MAX_WAIT: Duration = Duration::from_secs(30);
/// Pages the look back may take, which is 20,000 payments at the providers' page sizes.
pub const MAX_PAGES: usize = 200;
pub const BACKFILL_DAYS: i64 = 90;

/// Talks to one provider with the project's key.
pub struct Client {
    pub provider: Provider,
    base: String,
    key: String,
    http: reqwest::Client,
}

impl Client {
    /// `fake` is Config::payments_api_url: empty for the providers themselves.
    pub fn new(provider: Provider, fake: &str, key: &str) -> AppResult<Client> {
        let key = key.trim().to_string();
        let base = if fake.is_empty() { provider.api(&key).to_string() } else { format!("{}/{}", fake.trim_end_matches('/'), provider.name()) };
        let http = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| AppError::Internal(e.into()))?;
        Ok(Client { provider, base, key, http })
    }

    /// GETs a path of the provider's API with the key, and waits and tries again on 429 or 503 for as long as Retry-After
    /// says, or else a little longer each time.
    pub async fn get(&self, path: &str, query: &[(&str, String)]) -> AppResult<Value> {
        let label = self.provider.label();
        for attempt in 0..TRIES {
            let mut url = reqwest::Url::parse(&format!("{}{path}", self.base)).map_err(|e| AppError::Internal(e.into()))?;
            if !query.is_empty() {
                url.query_pairs_mut().extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
            }
            let mut req = self.http.get(url).bearer_auth(&self.key);
            if self.provider == Provider::LemonSqueezy {
                req = req.header("accept", "application/vnd.api+json");
            }
            let res = req.send().await.map_err(|_| AppError::BadRequest(format!("Couldn't reach {label}. Try again in a moment.")))?;
            let status = res.status().as_u16();
            let retry_after = res.headers().get("retry-after").and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<u64>().ok());
            let body: Value = res.json().await.unwrap_or(Value::Null);
            match status {
                200..=299 => return Ok(body),
                429 | 503 if attempt + 1 < TRIES => {
                    tokio::time::sleep(retry_after.map_or(Duration::from_secs(2 << attempt), Duration::from_secs).min(MAX_WAIT)).await;
                }
                401 => return Err(AppError::BadRequest(format!("{label} didn't accept that API key. Copy it again from {label}."))),
                403 => return Err(AppError::BadRequest(format!("That {label} key can't read {}. Give it read access and try again.", self.provider.reads()))),
                404 => return Ok(Value::Null),
                _ => return Err(AppError::BadRequest(format!("{label} answered {status}. Try again in a few minutes."))),
            }
        }
        unreachable!("the last try returns")
    }

    /// Checks the key can read what the look back and the webhooks need.
    pub async fn check(&self) -> AppResult<()> {
        match self.provider {
            Provider::Stripe => stripe::check(self).await,
            Provider::Paddle => paddle::check(self).await,
            Provider::LemonSqueezy => lemonsqueezy::check(self).await,
        }
    }

    /// The charges, completed transactions or orders since `cutoff` and their refunds, oldest first, so the first payment wins
    /// the deal.
    pub async fn since(&self, cutoff: DateTime<Utc>) -> AppResult<Vec<Charge>> {
        let mut charges = match self.provider {
            Provider::Stripe => stripe::since(self, cutoff).await?,
            Provider::Paddle => paddle::since(self, cutoff).await?,
            Provider::LemonSqueezy => lemonsqueezy::since(self, cutoff).await?,
        };
        charges.sort_by_key(|c| c.paid_at);
        Ok(charges)
    }

    /// The address of a customer the provider keeps by id. None for one that's gone or has none.
    async fn customer_email(&self, id: &str) -> AppResult<Option<String>> {
        let Some(id) = path_id(id) else { return Ok(None) };
        Ok(match self.provider {
            Provider::Stripe => text(&self.get(&format!("/v1/customers/{id}"), &[]).await?["email"]),
            Provider::Paddle => text(&self.get(&format!("/customers/{id}"), &[]).await?["data"]["email"]),
            Provider::LemonSqueezy => None,
        })
    }
}

/// Addresses at these domains are people, not companies, so a payment from one only matches a lead with that exact address.
const FREE_MAIL: &[&str] = &[
    "gmail.com", "googlemail.com", "yahoo.com", "yahoo.co.uk", "yahoo.fr", "ymail.com", "hotmail.com", "hotmail.co.uk", "hotmail.fr", "outlook.com",
    "live.com", "msn.com", "icloud.com", "me.com", "mac.com", "aol.com", "proton.me", "protonmail.com", "pm.me", "gmx.com", "gmx.de", "gmx.net",
    "web.de", "mail.com", "yandex.com", "yandex.ru", "mail.ru", "zoho.com", "fastmail.com", "hey.com", "tutanota.com", "qq.com", "163.com", "126.com",
    "comcast.net", "att.net", "verizon.net", "orange.fr", "free.fr", "laposte.net", "t-online.de", "libero.it", "btinternet.com",
];

pub fn free_mail(domain: &str) -> bool {
    FREE_MAIL.contains(&domain)
}

/// The project's lead who paid: the one with the payer's address, else one at the payer's company domain, preferring the lead
/// who paid before, then one with an open deal, then the one emailed last.
pub async fn match_lead(db: &PgPool, project_id: Uuid, email: &str) -> sqlx::Result<Option<Uuid>> {
    let email = email.trim().to_lowercase();
    let exact: Option<Uuid> =
        sqlx::query_scalar("select id from leads where project_id = $1 and email = $2 order by last_contacted_at desc nulls last, created_at desc limit 1")
            .bind(project_id)
            .bind(&email)
            .fetch_optional(db)
            .await?;
    if exact.is_some() {
        return Ok(exact);
    }
    let Some((_, domain)) = email.rsplit_once('@') else { return Ok(None) };
    if domain.is_empty() || free_mail(domain) {
        return Ok(None);
    }
    sqlx::query_scalar(
        "select l.id from leads l
         where l.project_id = $1 and lower(regexp_replace(l.company_domain::text, '^www\\.', '')) = $2
         order by exists (select 1 from payments p where p.lead_id = l.id) desc,
           exists (select 1 from deals d where d.lead_id = l.id and d.status = 'open') desc, l.last_contacted_at desc nulls last, l.created_at desc
         limit 1",
    )
    .bind(project_id)
    .bind(domain)
    .fetch_optional(db)
    .await
}

/// A project's connection to a provider.
#[derive(Debug, Clone, FromRow)]
pub struct Connection {
    pub id: Uuid,
    pub org_id: Uuid,
    pub project_id: Uuid,
    pub provider: String,
    pub api_key_enc: String,
}

impl Connection {
    pub fn provider(&self) -> AppResult<Provider> {
        Provider::parse(&self.provider).ok_or_else(|| AppError::Internal(anyhow::anyhow!("unknown payment provider {}", self.provider)))
    }

    pub fn client(&self, state: &AppState) -> AppResult<Client> {
        let key = crate::crypto::decrypt(&state.cfg.app_secret, &self.api_key_enc)?;
        Client::new(self.provider()?, &state.cfg.payments_api_url, &key)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Nobody in the project paid it.
    Unmatched,
    /// Kept already, from a webhook or an earlier look back.
    Known,
    New,
}

/// Keeps a payment or refund against the lead who paid it. The lead's first payment wins their open deal with its amount, or
/// a new won deal when they have none open, and fires deal.won. Later payments only add to the revenue.
pub async fn record(state: &AppState, c: &Connection, client: &Client, charge: &Charge) -> AppResult<Recorded> {
    let db = &state.db;
    let provider = c.provider()?;
    // A payment or a single refund is never changed after it's kept, so seeing it again needs no lookup.
    if charge.kind != Kind::RefundTotal {
        let known: bool = sqlx::query_scalar("select exists (select 1 from payments where project_id = $1 and provider = $2 and external_id = $3)")
            .bind(c.project_id)
            .bind(provider.name())
            .bind(&charge.external_id)
            .fetch_one(db)
            .await?;
        if known {
            return Ok(Recorded::Known);
        }
    }
    let email = match (&charge.email, &charge.customer) {
        (Some(email), _) => Some(email.clone()),
        (None, Some(customer)) => client.customer_email(customer).await?,
        (None, None) => None,
    };
    let Some(email) = email.map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')) else { return Ok(Recorded::Unmatched) };
    let Some(lead_id) = match_lead(db, c.project_id, &email).await? else { return Ok(Recorded::Unmatched) };

    deals::ensure_stages(db, c.org_id).await?;
    let mut tx = db.begin().await?;
    // The lead's payments take turns, so two arriving at once can't both be the first.
    sqlx::query("select id from leads where id = $1 for update").bind(lead_id).execute(&mut *tx).await?;
    let (external_id, signed) = match charge.kind {
        Kind::Payment => (charge.external_id.clone(), charge.amount_cents),
        Kind::Refund => (charge.external_id.clone(), -charge.amount_cents),
        Kind::RefundTotal => {
            // Held until commit, so two deliveries of the same total can't both see it unrecorded.
            sqlx::query("select pg_advisory_xact_lock(hashtextextended($1, 0))")
                .bind(format!("{}:{}:{}", c.project_id, provider.name(), charge.external_id))
                .execute(&mut *tx)
                .await?;
            let refunded: i64 = sqlx::query_scalar(
                "select coalesce(-sum(amount_cents), 0)::bigint from payments
                 where project_id = $1 and provider = $2 and starts_with(external_id, $3 || ':')",
            )
            .bind(c.project_id)
            .bind(provider.name())
            .bind(&charge.external_id)
            .fetch_one(&mut *tx)
            .await?;
            // A total no higher than what's kept is a late or repeated report of a refund already counted.
            if charge.amount_cents <= refunded {
                return Ok(Recorded::Known);
            }
            (format!("{}:{}", charge.external_id, charge.amount_cents), refunded - charge.amount_cents)
        }
    };
    let kept: Option<Uuid> = sqlx::query_scalar(
        "insert into payments (org_id, project_id, lead_id, provider, external_id, kind, amount_cents, currency, customer_email, paid_at)
         values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
         on conflict (project_id, provider, external_id) do nothing
         returning id",
    )
    .bind(c.org_id)
    .bind(c.project_id)
    .bind(lead_id)
    .bind(provider.name())
    .bind(&external_id)
    .bind(if charge.kind == Kind::Payment { "payment" } else { "refund" })
    .bind(signed)
    .bind(&charge.currency)
    .bind(&email)
    .bind(charge.paid_at)
    .fetch_optional(&mut *tx)
    .await?;
    if kept.is_none() {
        return Ok(Recorded::Known);
    }
    let mut events = vec![];
    if charge.kind == Kind::Payment {
        let first: bool = sqlx::query_scalar("select count(*) = 1 from payments where lead_id = $1 and kind = 'payment'").bind(lead_id).fetch_one(&mut *tx).await?;
        if first {
            events = deals::win_for_payment(&mut tx, c.org_id, lead_id, charge.amount_cents, &charge.currency, charge.paid_at, provider.name()).await?;
        }
    }
    tx.commit().await?;
    deals::announce_all(state, events).await;
    Ok(Recorded::New)
}

/// Picks up queued look backs. One left running by a server that stopped is queued again, and running it twice only finds
/// payments that are already kept.
pub fn spawn_worker(state: AppState) {
    tokio::spawn(async move {
        if let Err(e) = sqlx::query("update payment_connections set backfill_status = 'queued' where backfill_status = 'running'").execute(&state.db).await {
            tracing::error!(error = ?e, "payments: requeue failed");
        }
        loop {
            let claimed: Result<Option<Uuid>, sqlx::Error> = sqlx::query_scalar(
                "update payment_connections set backfill_status = 'running'
                 where id = (select id from payment_connections where backfill_status = 'queued' order by created_at limit 1 for update skip locked)
                 returning id",
            )
            .fetch_optional(&state.db)
            .await;
            match claimed {
                Ok(Some(id)) => {
                    if let Err(e) = backfill(&state, id).await {
                        tracing::warn!(error = ?e, %id, "payments look back failed");
                    }
                }
                Ok(None) => tokio::time::sleep(Duration::from_secs(3)).await,
                Err(e) => {
                    tracing::error!(error = ?e, "payments: claim failed");
                    tokio::time::sleep(Duration::from_secs(10)).await;
                }
            }
        }
    });
}

/// Records the last 90 days of the provider's payments, and how many it found and matched, or why it couldn't.
pub async fn backfill(state: &AppState, id: Uuid) -> AppResult<()> {
    let outcome = backfill_inner(state, id).await;
    let (status, error, found, matched) = match &outcome {
        Ok((found, matched)) => ("done", String::new(), *found, *matched),
        Err(e) => ("failed", e.public_message(), 0, 0),
    };
    sqlx::query(
        "update payment_connections set backfill_status = $2, backfill_error = $3, backfill_found = $4, backfill_matched = $5, backfilled_at = now()
         where id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(error)
    .bind(found as i32)
    .bind(matched as i32)
    .execute(&state.db)
    .await?;
    outcome.map(|_| ())
}

async fn backfill_inner(state: &AppState, id: Uuid) -> AppResult<(usize, usize)> {
    let c: Connection = sqlx::query_as("select id, org_id, project_id, provider, api_key_enc from payment_connections where id = $1").bind(id).fetch_one(&state.db).await?;
    let client = c.client(state)?;
    let charges = client.since(Utc::now() - chrono::Duration::days(BACKFILL_DAYS)).await?;
    let mut matched = 0;
    for charge in &charges {
        let recorded = record(state, &c, &client, charge).await?;
        if charge.kind == Kind::Payment && recorded != Recorded::Unmatched {
            matched += 1;
        }
    }
    Ok((charges.iter().filter(|c| c.kind == Kind::Payment).count(), matched))
}
