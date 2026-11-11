//! Smartlead's API v1, with the key as the api_key query parameter. The campaign list comes whole, and leads and the
//! blocklist page with `offset` and `limit`.
//! Docs: https://api.smartlead.ai/api-reference/campaigns/list
//!       https://api.smartlead.ai/api-reference/campaigns/get-sequences
//!       https://api.smartlead.ai/api-reference/campaigns/get-leads
//!       https://api.smartlead.ai/api-reference/utilities/domain-block-list
//!       https://api.smartlead.ai/guides/rate-limits

use futures::{StreamExt, TryStreamExt, stream};
use serde_json::Value;

use super::{Campaign, Client, Lead, MAX_PAGES, State, Step, count, date, step, title_case};
use crate::error::{AppError, AppResult};

pub const API: &str = "https://server.smartlead.ai/api/v1";

/// Smartlead's own limit is 100 leads a page.
const PAGE: i64 = 100;
const BLOCKLIST_PAGE: usize = 1000;

pub async fn campaigns(client: &Client) -> AppResult<Vec<Campaign>> {
    let list = client.get("/campaigns/", &[]).await?;
    let campaigns: Vec<Campaign> = list
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["id"].as_i64().map(|n| n.to_string()).or_else(|| c["id"].as_str().map(str::to_string))?;
            let status = c["status"].as_str().map(title_case).unwrap_or_default();
            Some(Campaign { id, name: c["name"].as_str().unwrap_or_default().to_string(), status, leads: None })
        })
        .collect();
    // The list has no lead counts, so each campaign's first page of one lead tells its total. A few at a time stays under
    // Smartlead's limit of requests a minute.
    stream::iter(campaigns)
        .map(|mut c| async move {
            let page = client.get(&format!("/campaigns/{}/leads", c.id), &[("offset", "0".into()), ("limit", "1".into())]).await;
            c.leads = page.ok().and_then(|p| total(&p));
            Ok::<_, AppError>(c)
        })
        .buffered(4)
        .try_collect()
        .await
}

/// The docs name the page's fields `total` and `leads`, and the API has sent `total_leads` and `data`, so either is read.
fn total(page: &Value) -> Option<i64> {
    count(&page["total"]).or_else(|| count(&page["total_leads"]))
}

pub async fn steps(client: &Client, campaign: &str) -> AppResult<Vec<Step>> {
    let body = client.get(&format!("/campaigns/{campaign}/sequences"), &[]).await?;
    Ok(parse_steps(&body))
}

/// Each sequence's delay is the wait before it. With an A/B test the step's own copy may be empty and its variants hold it.
fn parse_steps(body: &Value) -> Vec<Step> {
    let mut sequences: Vec<&Value> = body.as_array().into_iter().flatten().collect();
    sequences.sort_by_key(|s| s["seq_number"].as_i64().unwrap_or(0));
    sequences
        .into_iter()
        .map(|s| {
            let variant = &s["sequence_variants"][0];
            let text = |key: &str| s[key].as_str().filter(|t| !t.trim().is_empty()).or_else(|| variant[key].as_str()).unwrap_or_default();
            let delay = count(&s["seq_delay_details"]["delayInDays"]).or_else(|| count(&s["seq_delay_details"]["delay_in_days"])).unwrap_or(0);
            step(delay, text("subject"), text("email_body"))
        })
        .collect()
}

pub async fn leads(client: &Client, campaign: &str) -> AppResult<Vec<Lead>> {
    let mut leads = Vec::new();
    for _ in 0..MAX_PAGES {
        let query = [("offset", leads.len().to_string()), ("limit", PAGE.to_string())];
        let body = client.get(&format!("/campaigns/{campaign}/leads"), &query).await?;
        let items = body["leads"].as_array().or_else(|| body["data"].as_array()).cloned().unwrap_or_default();
        leads.extend(items.iter().map(parse_lead));
        // The total says when to stop, since a page may hold fewer than asked for. Without one, a short page is the last.
        let last = match total(&body) {
            Some(total) => items.is_empty() || leads.len() as i64 >= total,
            None => (items.len() as i64) < PAGE,
        };
        if last {
            return Ok(leads);
        }
    }
    Err(AppError::BadRequest("The campaign has over 100,000 leads, more than Splee can bring over at once".into()))
}

/// A lead's details may sit on the item or under its `lead`. Its email stats say whether it replied, bounced or unsubscribed,
/// its category whether a reply was interested, and its status whether the sequence reached it. BLOCKED is the blocklist.
fn parse_lead(item: &Value) -> Lead {
    let l = if item["lead"].is_object() { &item["lead"] } else { item };
    let mut lead = Lead::default();
    for key in ["email", "first_name", "last_name", "company_name", "phone_number", "website", "company_url", "linkedin_profile", "location"] {
        lead.set(key, &l[key]);
    }
    for (key, value) in l["custom_fields"].as_object().into_iter().flatten() {
        lead.set(key, value);
    }
    let stats = if item["email_stats"].is_object() { &item["email_stats"] } else { &l["email_stats"] };
    let category = item["category_name"].as_str().or_else(|| item["lead_category"].as_str()).unwrap_or_default().to_lowercase();
    let status = item["status"].as_str().or_else(|| l["status"].as_str()).unwrap_or_default();
    let last_sent = date(&item["last_sent_time"]).or_else(|| date(&l["last_sent_time"]));
    lead.state = if stats["is_bounced"] == true {
        State::Bounced
    } else if stats["is_unsubscribed"] == true || l["is_unsubscribed"] == true {
        State::Unsubscribed
    } else if status == "BLOCKED" {
        State::Blocked
    } else if category.contains("meeting") {
        State::MeetingBooked
    } else if category.contains("not interested") {
        State::NotInterested
    } else if category.contains("interested") {
        State::Interested
    } else if stats["is_replied"] == true {
        State::Replied
    } else if last_sent.is_some() || matches!(status, "INPROGRESS" | "COMPLETED") {
        State::Contacted
    } else {
        State::New
    };
    lead.last_contacted_at = last_sent;
    lead
}

pub async fn blocklist(client: &Client) -> AppResult<Vec<String>> {
    let mut entries = Vec::new();
    for page in 0..MAX_PAGES {
        let query = [("offset", (page * BLOCKLIST_PAGE).to_string()), ("limit", BLOCKLIST_PAGE.to_string())];
        let body = client.get("/leads/get-domain-block-list", &query).await?;
        let items = body.as_array().cloned().unwrap_or_default();
        entries.extend(items.iter().filter_map(|e| e["email_or_domain"].as_str().map(str::to_string)));
        if items.len() < BLOCKLIST_PAGE {
            return Ok(entries);
        }
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_lead, parse_steps};
    use crate::outreach::State;

    #[test]
    fn sequences_are_read_in_order_with_their_variants() {
        let body = json!([
            {"seq_number": 2, "subject": "", "email_body": "Following up, {{first_name}}", "seq_delay_details": {"delayInDays": 3}},
            {"seq_number": 1, "subject": "", "email_body": "", "seq_delay_details": {"delayInDays": 0},
             "sequence_variants": [{"subject": "{{company_name}} x Fieldnote", "email_body": "<p>Hi {{first_name}},</p><p>{{custom_hook}}</p>"}]},
        ]);
        let steps = parse_steps(&body);
        assert_eq!(steps.iter().map(|s| (s.delay_days, s.subject.as_str(), s.body.as_str())).collect::<Vec<_>>(),
            [(0, "{company} x Fieldnote", "Hi {first_name},\n\n{{custom_hook}}"), (3, "", "Following up, {first_name}")]);
    }

    #[test]
    fn a_lead_keeps_what_smartlead_knows() {
        let item = json!({"status": "INPROGRESS", "lead": {"email": "bo@acme.com", "first_name": "Bo", "phone_number": "+447700900123",
            "linkedin_profile": "https://linkedin.com/in/bo", "custom_fields": {"Job Title": "COO", "Plan": "Pro"}}});
        let lead = parse_lead(&item);
        assert_eq!((lead.state, lead.title.as_str(), lead.phone.as_str()), (State::Contacted, "COO", "+447700900123"));
        assert_eq!(lead.custom, [("Plan".to_string(), "Pro".to_string())]);
        assert_eq!(parse_lead(&json!({"email": "a@x.com", "status": "STARTED"})).state, State::New);
        assert_eq!(parse_lead(&json!({"email_stats": {"is_bounced": true}, "email": "a@x.com"})).state, State::Bounced);
        assert_eq!(parse_lead(&json!({"lead": {"email": "a@x.com", "is_unsubscribed": true}})).state, State::Unsubscribed);
        assert_eq!(parse_lead(&json!({"category_name": "Meeting Booked", "email": "a@x.com"})).state, State::MeetingBooked);
        assert_eq!(parse_lead(&json!({"category_name": "Not Interested", "email": "a@x.com"})).state, State::NotInterested);
        assert_eq!(parse_lead(&json!({"status": "BLOCKED", "email": "a@x.com"})).state, State::Blocked);
        assert_eq!(parse_lead(&json!({"email_stats": {"is_replied": true}, "email": "a@x.com"})).state, State::Replied);
    }
}
