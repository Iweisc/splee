//! Instantly's API v2, with the key as a bearer token. Lists page with `limit` and `starting_after`, and a page has
//! `next_starting_after` until the last one.
//! Docs: https://developer.instantly.ai/api-reference/campaign/list-campaign
//!       https://developer.instantly.ai/api-reference/campaign/get-campaign
//!       https://developer.instantly.ai/api-reference/campaign/get-campaigns-analytics
//!       https://developer.instantly.ai/api-reference/lead/list-leads
//!       https://developer.instantly.ai/api-reference/blocklistentry/list-block-list-entry
//!       https://developer.instantly.ai/getting-started/rate-limit

use std::collections::HashMap;

use serde_json::{Value, json};

use super::{Campaign, Client, Lead, MAX_PAGES, State, Step, count, date, step};
use crate::error::{AppError, AppResult};

pub const API: &str = "https://api.instantly.ai/api/v2";

/// Every page of a GET list, its items in order.
async fn pages(client: &Client, path: &str) -> AppResult<Vec<Value>> {
    let mut items = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut query = vec![("limit", "100".to_string())];
        if let Some(after) = &after {
            query.push(("starting_after", after.clone()));
        }
        let body = client.get(path, &query).await?;
        items.extend(body["items"].as_array().into_iter().flatten().cloned());
        match body["next_starting_after"].as_str().filter(|s| !s.is_empty()) {
            Some(next) => after = Some(next.to_string()),
            None => return Ok(items),
        }
    }
    Err(AppError::BadRequest("Instantly has more than Splee can read at once".into()))
}

pub async fn campaigns(client: &Client) -> AppResult<Vec<Campaign>> {
    let items = pages(client, "/campaigns").await?;
    // Lead counts come from analytics, which a key without analytics access can't read. The list goes on without them.
    let counts: HashMap<String, i64> = match client.get("/campaigns/analytics", &[]).await {
        Ok(rows) => rows
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| Some((r["campaign_id"].as_str()?.to_string(), count(&r["leads_count"])?)))
            .collect(),
        Err(_) => HashMap::new(),
    };
    Ok(items
        .iter()
        .filter_map(|c| {
            let id = c["id"].as_str()?.to_string();
            let status = match c["status"].as_i64() {
                Some(0) => "Draft",
                Some(1) => "Active",
                Some(2) => "Paused",
                Some(3) => "Completed",
                _ => "",
            };
            Some(Campaign { leads: counts.get(&id).copied(), name: c["name"].as_str().unwrap_or_default().to_string(), status: status.into(), id })
        })
        .collect())
}

/// Only a campaign's first sequence is sent, and a step's delay is the wait before the next step. A step's first variant
/// that isn't turned off is the one copied.
pub async fn steps(client: &Client, campaign: &str) -> AppResult<Vec<Step>> {
    let body = client.get(&format!("/campaigns/{campaign}"), &[]).await?;
    Ok(parse_steps(&body))
}

fn parse_steps(campaign: &Value) -> Vec<Step> {
    let mut wait = 0;
    let mut steps = Vec::new();
    for s in campaign["sequences"][0]["steps"].as_array().into_iter().flatten() {
        let variants = s["variants"].as_array().cloned().unwrap_or_default();
        let Some(v) = variants.iter().find(|v| v["v_disabled"] != true).or(variants.first()) else { continue };
        steps.push(step(wait, v["subject"].as_str().unwrap_or_default(), v["body"].as_str().unwrap_or_default()));
        let delay = s["delay"].as_f64().unwrap_or(0.0);
        wait = match s["delay_unit"].as_str() {
            Some("minutes") => (delay / 1440.0).ceil(),
            Some("hours") => (delay / 24.0).ceil(),
            _ => delay.ceil(),
        } as i64;
    }
    steps
}

/// Leads list with a POST, since its filters don't fit a query string.
pub async fn leads(client: &Client, campaign: &str) -> AppResult<Vec<Lead>> {
    let mut leads = Vec::new();
    let mut after: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut body = json!({ "campaign": campaign, "limit": 100 });
        if let Some(after) = &after {
            body["starting_after"] = json!(after);
        }
        let page = client.post("/leads/list", body).await?;
        leads.extend(page["items"].as_array().into_iter().flatten().map(parse_lead));
        match page["next_starting_after"].as_str().filter(|s| !s.is_empty()) {
            Some(next) => after = Some(next.to_string()),
            None => return Ok(leads),
        }
    }
    Err(AppError::BadRequest("The campaign has over 100,000 leads, more than Splee can bring over at once".into()))
}

/// Status -1 is bounced and -2 unsubscribed. The interest status is set by a reply or by hand: 1 interested, 2 to 4 a
/// meeting booked, held or won, and -1 not interested.
fn parse_lead(l: &Value) -> Lead {
    let mut lead = Lead::default();
    for key in ["email", "first_name", "last_name", "job_title", "company_name", "company_domain", "website", "phone", "personalization"] {
        lead.set(key, &l[key]);
    }
    for (key, value) in l["payload"].as_object().into_iter().flatten() {
        // The payload repeats the standard fields under Instantly's variable names.
        if !matches!(key.as_str(), "firstName" | "lastName" | "companyName" | "email" | "website" | "phone" | "personalization") {
            lead.set(key, value);
        }
    }
    let replied = count(&l["email_reply_count"]).unwrap_or(0) > 0 || !l["timestamp_last_reply"].is_null();
    lead.state = match (l["status"].as_i64(), l["lt_interest_status"].as_i64()) {
        (Some(-1), _) => State::Bounced,
        (Some(-2), _) => State::Unsubscribed,
        (_, Some(2..=4)) => State::MeetingBooked,
        (_, Some(1)) => State::Interested,
        (_, Some(-1)) => State::NotInterested,
        _ if replied => State::Replied,
        _ if !l["timestamp_last_contact"].is_null() || count(&l["status"]) == Some(3) => State::Contacted,
        _ => State::New,
    };
    lead.last_contacted_at = date(&l["timestamp_last_contact"]);
    lead
}

pub async fn blocklist(client: &Client) -> AppResult<Vec<String>> {
    let items = pages(client, "/block-lists-entries").await?;
    Ok(items.iter().filter_map(|e| e["bl_value"].as_str().map(str::to_string)).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_lead, parse_steps};
    use crate::outreach::State;

    #[test]
    fn steps_wait_the_delay_of_the_step_before() {
        let campaign = json!({"sequences": [{"steps": [
            {"type": "email", "delay": 2, "variants": [{"subject": "Hi {{firstName}}", "body": "<div>Hey {{firstName}},</div><div><br /></div><div>Quick one.</div>", "v_disabled": true}, {"subject": "B", "body": "b"}]},
            {"type": "email", "delay": 36, "delay_unit": "hours", "variants": [{"subject": "", "body": "Bump"}]},
            {"type": "email", "variants": [{"subject": "", "body": "Last"}]},
        ]}]});
        let steps = parse_steps(&campaign);
        assert_eq!(steps.iter().map(|s| (s.delay_days, s.subject.as_str(), s.body.as_str())).collect::<Vec<_>>(), [(0, "B", "b"), (2, "", "Bump"), (2, "", "Last")]);
        let only = json!({"sequences": [{"steps": [{"delay": 1, "variants": [{"subject": "Hi {{firstName}}", "body": "<div>Hey {{firstName}},</div><div><br /></div><div>Quick one.</div>"}]}]}]});
        assert_eq!((parse_steps(&only)[0].subject.as_str(), parse_steps(&only)[0].body.as_str()), ("Hi {first_name}", "Hey {first_name},\n\nQuick one."));
    }

    #[test]
    fn a_lead_keeps_what_instantly_knows() {
        let bounced = parse_lead(&json!({"email": "a@x.com", "status": -1, "timestamp_last_contact": "2026-10-01T09:00:00.000Z"}));
        assert_eq!(bounced.state, State::Bounced);
        assert_eq!(bounced.last_contacted_at.unwrap().to_rfc3339(), "2026-10-01T09:00:00+00:00");
        assert_eq!(parse_lead(&json!({"status": 1, "email_reply_count": 1})).state, State::Replied);
        assert_eq!(parse_lead(&json!({"status": 3, "lt_interest_status": 2})).state, State::MeetingBooked);
        assert_eq!(parse_lead(&json!({"status": 1, "timestamp_last_contact": "2026-10-01T09:00:00Z"})).state, State::Contacted);
        assert_eq!(parse_lead(&json!({"status": 1})).state, State::New);
        let lead = parse_lead(&json!({"email": "ana@delta.io", "first_name": "Ana", "company_name": "Delta", "job_title": "CTO", "website": "delta.io",
            "payload": {"firstName": "Ana", "linkedIn": "https://linkedin.com/in/ana", "Industry": "SaaS", "empty": ""}}));
        assert_eq!((lead.first_name.as_str(), lead.title.as_str(), lead.company_domain.as_str()), ("Ana", "CTO", "delta.io"));
        assert_eq!((lead.linkedin_url.as_str(), lead.custom.clone()), ("https://linkedin.com/in/ana", vec![("Industry".to_string(), "SaaS".to_string())]));
    }
}
