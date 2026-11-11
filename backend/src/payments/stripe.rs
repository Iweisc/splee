//! Stripe, with a restricted key that reads customers, charges and refunds. Every payment is a charge, whether it came from
//! an invoice, a subscription or a Checkout one-off, so a succeeded charge is a payment and its amount_refunded the refunds.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{Charge, Client, Kind, MAX_PAGES, cents, fresh, signed, text};
use crate::error::AppResult;

pub const API: &str = "https://api.stripe.com";

/// Stripe-Signature is `t={unix time},v1={hex HMAC-SHA256 of "{t}.{body}"}`, with a v1 for each signing secret while one is
/// being rolled.
pub fn verify(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let pairs: Vec<(&str, &str)> = header.split(',').filter_map(|p| p.trim().split_once('=')).collect();
    let Some(t) = pairs.iter().find(|(k, _)| *k == "t").map(|(_, v)| *v) else { return false };
    fresh(t, now) && pairs.iter().filter(|(k, _)| *k == "v1").any(|(_, sig)| signed(secret, &[t.as_bytes(), b".", body], sig))
}

fn unix(v: &Value) -> Option<DateTime<Utc>> {
    v.as_i64().and_then(|s| DateTime::from_timestamp(s, 0))
}

/// A customer is an id, or the whole object when the request expanded it.
fn customer(v: &Value) -> Option<String> {
    text(v).or_else(|| text(&v["id"]))
}

/// A charge that took the money. One only authorized counts once it's captured, through charge.captured.
fn payment(c: &Value) -> Option<Charge> {
    if c["status"] != "succeeded" || c["captured"] == false {
        return None;
    }
    Some(Charge {
        external_id: text(&c["id"])?,
        kind: Kind::Payment,
        amount_cents: cents(&c["amount_captured"]).or_else(|| cents(&c["amount"])).filter(|a| *a > 0)?,
        currency: text(&c["currency"])?.to_uppercase(),
        email: text(&c["billing_details"]["email"]).or_else(|| text(&c["receipt_email"])),
        customer: customer(&c["customer"]),
        paid_at: unix(&c["created"])?,
    })
}

/// What's been refunded on the charge so far, as of `at`.
fn refunded(c: &Value, total: i64, at: DateTime<Utc>) -> Option<Charge> {
    Some(Charge { external_id: format!("refund:{}", text(&c["id"])?), kind: Kind::RefundTotal, amount_cents: total, paid_at: at, ..payment(c)? })
}

pub fn charges(event: &Value) -> Vec<Charge> {
    let c = &event["data"]["object"];
    match event["type"].as_str() {
        Some("charge.succeeded" | "charge.captured") => payment(c).into_iter().collect(),
        Some("charge.refunded") => {
            let at = unix(&event["created"]).unwrap_or_else(Utc::now);
            cents(&c["amount_refunded"]).filter(|t| *t > 0).and_then(|total| refunded(c, total, at)).into_iter().collect()
        }
        _ => vec![],
    }
}

pub async fn check(client: &Client) -> AppResult<()> {
    for path in ["/v1/charges", "/v1/refunds", "/v1/customers"] {
        client.get(path, &[("limit", "1".into())]).await?;
    }
    Ok(())
}

/// Succeeded charges created since `cutoff`, 100 a page, newest first, and for each one refunded, its running total after
/// each refund, which are the same ids the charge.refunded webhook gives.
pub async fn since(client: &Client, cutoff: DateTime<Utc>) -> AppResult<Vec<Charge>> {
    let mut out = vec![];
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("limit", "100".to_string()), ("created[gte]", cutoff.timestamp().to_string())];
        if let Some(id) = &after {
            query.push(("starting_after", id.clone()));
        }
        let page = client.get("/v1/charges", &query).await?;
        let data = page["data"].as_array().cloned().unwrap_or_default();
        for c in &data {
            let Some(paid) = payment(c) else { continue };
            out.push(paid);
            if cents(&c["amount_refunded"]).is_some_and(|t| t > 0) {
                out.extend(refunds(client, c).await?);
            }
        }
        after = data.last().and_then(|c| text(&c["id"]));
        if after.is_none() || !page["has_more"].as_bool().unwrap_or(false) {
            break;
        }
    }
    Ok(out)
}

/// The charge's refunds, oldest first, as the running total after each.
async fn refunds(client: &Client, c: &Value) -> AppResult<Vec<Charge>> {
    let Some(id) = text(&c["id"]) else { return Ok(vec![]) };
    let page = client.get("/v1/refunds", &[("charge", id), ("limit", "100".into())]).await?;
    let mut list: Vec<(DateTime<Utc>, i64)> = page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|r| !matches!(r["status"].as_str(), Some("failed" | "canceled")))
        .filter_map(|r| Some((unix(&r["created"])?, cents(&r["amount"])?)))
        .collect();
    list.sort();
    let mut total = 0;
    Ok(list
        .into_iter()
        .filter_map(|(at, amount)| {
            total += amount;
            refunded(c, total, at)
        })
        .collect())
}
