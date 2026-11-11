//! Paddle Billing. A completed transaction is a payment, and an approved refund adjustment a refund. Neither carries the
//! customer's address, so it's looked up by customer id, except in the look back, which asks for the customer with each
//! transaction and gives its refunds the transaction's.

use std::collections::HashMap;

use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

use super::{Charge, Client, Kind, MAX_PAGES, cents, fresh, rfc3339, signed, text};
use crate::error::AppResult;

pub const API: &str = "https://api.paddle.com";
pub const SANDBOX_API: &str = "https://sandbox-api.paddle.com";

/// Paddle-Signature is `ts={unix time};h1={hex HMAC-SHA256 of "{ts}:{body}"}`, with an h1 for each secret while one is rotated.
pub fn verify(secret: &str, header: &str, body: &[u8], now: i64) -> bool {
    let pairs: Vec<(&str, &str)> = header.split(';').filter_map(|p| p.trim().split_once('=')).collect();
    let Some(ts) = pairs.iter().find(|(k, _)| *k == "ts").map(|(_, v)| *v) else { return false };
    fresh(ts, now) && pairs.iter().filter(|(k, _)| *k == "h1").any(|(_, sig)| signed(secret, &[ts.as_bytes(), b":", body], sig))
}

fn transaction(t: &Value) -> Option<Charge> {
    let totals = &t["details"]["totals"];
    Some(Charge {
        external_id: text(&t["id"])?,
        kind: Kind::Payment,
        amount_cents: cents(&totals["grand_total"]).or_else(|| cents(&totals["total"])).filter(|c| *c > 0)?,
        currency: text(&t["currency_code"])?.to_uppercase(),
        email: text(&t["customer"]["email"]),
        customer: text(&t["customer_id"]),
        paid_at: rfc3339(&t["billed_at"]).or_else(|| rfc3339(&t["updated_at"])).or_else(|| rfc3339(&t["created_at"]))?,
    })
}

fn adjustment(a: &Value) -> Option<Charge> {
    if a["action"] != "refund" || a["status"] != "approved" {
        return None;
    }
    Some(Charge {
        external_id: text(&a["id"])?,
        kind: Kind::Refund,
        amount_cents: cents(&a["totals"]["total"]).filter(|c| *c > 0)?,
        currency: text(&a["currency_code"])?.to_uppercase(),
        email: None,
        customer: text(&a["customer_id"]),
        paid_at: rfc3339(&a["updated_at"]).or_else(|| rfc3339(&a["created_at"]))?,
    })
}

pub fn charges(event: &Value) -> Vec<Charge> {
    let data = &event["data"];
    match event["event_type"].as_str() {
        Some("transaction.completed") => transaction(data).into_iter().collect(),
        Some("adjustment.created" | "adjustment.updated") => adjustment(data).into_iter().collect(),
        _ => vec![],
    }
}

pub async fn check(client: &Client) -> AppResult<()> {
    client.get("/transactions", &[("per_page", "1".into())]).await?;
    client.get("/customers", &[("per_page", "1".into())]).await?;
    client.get("/adjustments", &[("per_page", "1".into())]).await?;
    Ok(())
}

/// Completed transactions billed since `cutoff`, with their customers, 200 a page, then their approved refunds.
pub async fn since(client: &Client, cutoff: DateTime<Utc>) -> AppResult<Vec<Charge>> {
    let mut out = vec![];
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![
            ("status", "completed".to_string()),
            ("billed_at[GTE]", cutoff.to_rfc3339_opts(SecondsFormat::Secs, true)),
            ("include", "customer".into()),
            ("order_by", "id[ASC]".into()),
            ("per_page", "200".into()),
        ];
        if let Some(id) = &after {
            query.push(("after", id.clone()));
        }
        let page = client.get("/transactions", &query).await?;
        let data = page["data"].as_array().cloned().unwrap_or_default();
        out.extend(data.iter().filter_map(transaction));
        after = data.last().and_then(|t| text(&t["id"]));
        if after.is_none() || !page["meta"]["pagination"]["has_more"].as_bool().unwrap_or(false) {
            break;
        }
    }
    let paid: HashMap<String, Option<String>> = out.iter().map(|t| (t.external_id.clone(), t.email.clone())).collect();
    let ids: Vec<&str> = paid.keys().map(String::as_str).collect();
    for chunk in ids.chunks(ADJUSTMENT_FILTER) {
        out.extend(adjustments(client, &chunk.join(","), &paid).await?);
    }
    Ok(out)
}

/// How many transaction ids go in one /adjustments filter, which keeps the URL short.
const ADJUSTMENT_FILTER: usize = 50;

async fn adjustments(client: &Client, transactions: &str, paid: &HashMap<String, Option<String>>) -> AppResult<Vec<Charge>> {
    let mut out = vec![];
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![
            ("transaction_id", transactions.to_string()),
            ("action", "refund".into()),
            ("status", "approved".into()),
            ("order_by", "id[ASC]".into()),
            ("per_page", "50".into()),
        ];
        if let Some(id) = &after {
            query.push(("after", id.clone()));
        }
        let page = client.get("/adjustments", &query).await?;
        let data = page["data"].as_array().cloned().unwrap_or_default();
        for a in &data {
            let Some(email) = a["transaction_id"].as_str().and_then(|t| paid.get(t)) else { continue };
            out.extend(adjustment(a).map(|refund| Charge { email: email.clone(), ..refund }));
        }
        after = data.last().and_then(|a| text(&a["id"]));
        if after.is_none() || !page["meta"]["pagination"]["has_more"].as_bool().unwrap_or(false) {
            break;
        }
    }
    Ok(out)
}
