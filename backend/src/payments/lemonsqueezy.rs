//! Lemon Squeezy. An order is a payment, and so is a subscription renewal's invoice. The first invoice of a subscription is
//! left out, since its order already counts. Both keep their refunds as a running total, refunded_amount.

use chrono::{DateTime, Utc};
use serde_json::Value;

use super::{Charge, Client, Kind, MAX_PAGES, cents, rfc3339, signed, text};
use crate::error::AppResult;

pub const API: &str = "https://api.lemonsqueezy.com";

/// X-Signature is the hex HMAC-SHA256 of the body with the signing secret. Lemon Squeezy signs no timestamp.
pub fn verify(secret: &str, header: &str, body: &[u8]) -> bool {
    signed(secret, &[body], header)
}

/// An order or a subscription invoice, which name their fields alike, and its refunds. `prefix` keeps the two kinds' ids apart.
fn sale(o: &Value, prefix: &str) -> Vec<Charge> {
    let a = &o["attributes"];
    let (Some(id), Some(currency)) = (text(&o["id"]), text(&a["currency"])) else { return vec![] };
    if a["billing_reason"] == "initial" || !matches!(a["status"].as_str(), Some("paid" | "refunded" | "partial_refund")) {
        return vec![];
    }
    let currency = currency.to_uppercase();
    let email = text(&a["user_email"]);
    let created = rfc3339(&a["created_at"]).unwrap_or_else(Utc::now);
    let mut out = vec![];
    if let Some(total) = cents(&a["total"]).filter(|c| *c > 0) {
        out.push(Charge {
            external_id: format!("{prefix}:{id}"),
            kind: Kind::Payment,
            amount_cents: total,
            currency: currency.clone(),
            email: email.clone(),
            customer: None,
            paid_at: created,
        });
    }
    if let Some(refunded) = cents(&a["refunded_amount"]).filter(|c| *c > 0) {
        out.push(Charge {
            external_id: format!("refund:{prefix}:{id}"),
            kind: Kind::RefundTotal,
            amount_cents: refunded,
            currency,
            email,
            customer: None,
            paid_at: rfc3339(&a["refunded_at"]).or_else(|| rfc3339(&a["updated_at"])).unwrap_or(created),
        });
    }
    out
}

pub fn charges(event: &Value) -> Vec<Charge> {
    let data = &event["data"];
    match event["meta"]["event_name"].as_str() {
        Some("order_created" | "order_refunded") => sale(data, "order"),
        Some("subscription_payment_success" | "subscription_payment_refunded") => sale(data, "subscription-invoice"),
        _ => vec![],
    }
}

pub async fn check(client: &Client) -> AppResult<()> {
    client.get("/v1/orders", &[("page[size]", "1".into())]).await?;
    Ok(())
}

/// Orders and renewals made since `cutoff`. The API filters neither by date, so every page is read, 100 at a time.
pub async fn since(client: &Client, cutoff: DateTime<Utc>) -> AppResult<Vec<Charge>> {
    let mut out = vec![];
    for (path, prefix) in [("/v1/orders", "order"), ("/v1/subscription-invoices", "subscription-invoice")] {
        for number in 1..=MAX_PAGES {
            let page = client.get(path, &[("page[number]", number.to_string()), ("page[size]", "100".into())]).await?;
            let data = page["data"].as_array().cloned().unwrap_or_default();
            let recent = data.iter().filter(|o| rfc3339(&o["attributes"]["created_at"]).is_some_and(|at| at >= cutoff));
            out.extend(recent.flat_map(|o| sale(o, prefix)));
            let last = page["meta"]["page"]["lastPage"].as_u64().unwrap_or(1) as usize;
            if data.is_empty() || number >= last {
                break;
            }
        }
    }
    Ok(out)
}
