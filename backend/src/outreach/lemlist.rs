//! lemlist's API, with HTTP basic auth: an empty user and the key as the password. Campaigns and unsubscribes page with
//! `offset` and `limit`, and a campaign's leads come whole from its export.
//! Docs: https://developer.lemlist.com/api-reference/getting-started/authentication
//!       https://developer.lemlist.com/api-reference/getting-started/rate-limits
//!       https://developer.lemlist.com/api-reference/endpoints/campaigns/get-many-campaigns
//!       https://developer.lemlist.com/api-reference/endpoints/campaigns/get-campaign-reports
//!       https://developer.lemlist.com/api-reference/endpoints/sequences/get-campaign-sequences
//!       https://developer.lemlist.com/api-reference/endpoints/campaigns/export-campaign-leads-v2
//!       https://developer.lemlist.com/api-reference/endpoints/unsubscribes/list-unsubscribed-variables

use std::collections::HashMap;

use serde_json::Value;

use super::{Campaign, Client, Lead, MAX_PAGES, State, Step, count, step, title_case};
use crate::error::AppResult;

pub const API: &str = "https://api.lemlist.com/api";

const PAGE: usize = 100;

/// Every page of an offset-paged list. lemlist has sent a list both bare and as `{campaigns: [...]}`.
async fn pages(client: &Client, path: &str, extra: &[(&str, String)]) -> AppResult<Vec<Value>> {
    let mut items = Vec::new();
    for page in 0..MAX_PAGES {
        let mut query = vec![("offset", (page * PAGE).to_string()), ("limit", PAGE.to_string())];
        query.extend(extra.iter().cloned());
        let body = client.get(path, &query).await?;
        let list = body.as_array().or_else(|| body["campaigns"].as_array()).cloned().unwrap_or_default();
        let last = list.len() < PAGE;
        items.extend(list);
        if last {
            break;
        }
    }
    Ok(items)
}

pub async fn campaigns(client: &Client) -> AppResult<Vec<Campaign>> {
    let items = pages(client, "/campaigns", &[("version", "v2".into())]).await?;
    let mut campaigns: Vec<Campaign> = items
        .iter()
        .filter_map(|c| {
            let status = c["status"].as_str().map(title_case).unwrap_or_default();
            Some(Campaign { id: c["_id"].as_str()?.to_string(), name: c["name"].as_str().unwrap_or_default().to_string(), status, leads: None })
        })
        .collect();
    // Lead counts are in the campaigns' reports. Without them the list still shows.
    let mut counts: HashMap<String, i64> = HashMap::new();
    for chunk in campaigns.chunks(50) {
        let ids = chunk.iter().map(|c| c.id.as_str()).collect::<Vec<_>>().join(",");
        let Ok(reports) = client.get("/campaigns/reports", &[("campaignIds", ids)]).await else { break };
        counts.extend(reports.as_array().into_iter().flatten().filter_map(|r| Some((r["_id"].as_str()?.to_string(), count(&r["totalCount"])?))));
    }
    for c in &mut campaigns {
        c.leads = counts.get(&c.id).copied();
    }
    Ok(campaigns)
}

pub async fn steps(client: &Client, campaign: &str) -> AppResult<Vec<Step>> {
    let body = client.get(&format!("/campaigns/{campaign}/sequences"), &[]).await?;
    Ok(parse_steps(&body))
}

/// The sequences come as an object of id to sequence. Only email steps are copied, and the wait for a LinkedIn step or a
/// task skipped on the way adds to the next email's.
fn parse_steps(body: &Value) -> Vec<Step> {
    let mut all: Vec<&Value> = body.as_object().into_iter().flatten().flat_map(|(_, s)| s["steps"].as_array().into_iter().flatten()).collect();
    all.sort_by_key(|s| (s["sequenceStep"].as_i64().or_else(|| s["index"].as_i64()).unwrap_or(0), s["index"].as_i64().unwrap_or(0)));
    let mut steps = Vec::new();
    let mut wait = 0;
    for s in all {
        wait += count(&s["delay"]).unwrap_or(0);
        if s["type"] == "email" {
            steps.push(step(if steps.is_empty() { 0 } else { wait }, s["subject"].as_str().unwrap_or_default(), s["message"].as_str().unwrap_or_default()));
            wait = 0;
        }
    }
    steps
}

pub async fn leads(client: &Client, campaign: &str) -> AppResult<Vec<Lead>> {
    let body = client.get(&format!("/v2/campaigns/{campaign}/export/leads"), &[("state", "all".into()), ("format", "json".into())]).await?;
    Ok(body.as_array().into_iter().flatten().map(parse_lead).collect())
}

/// The export's own columns, which aren't the lead's details. Every other column is a custom field.
const NOT_FIELDS: [&str; 13] =
    ["_id", "emailStatus", "picture", "timezone", "lastState", "status", "salesforceId", "pipedriveId", "hubspotId", "attioId", "contactId", "campaignId", "leadId"];

/// lastState is the furthest the lead got. lemlist's export has no dates.
fn parse_lead(l: &Value) -> Lead {
    let mut lead = Lead::default();
    for (key, value) in l.as_object().into_iter().flatten() {
        if !NOT_FIELDS.contains(&key.as_str()) {
            lead.set(key, value);
        }
    }
    let state = l["lastState"].as_str().unwrap_or_default();
    lead.state = match state {
        "emailsBounced" => State::Bounced,
        "emailsUnsubscribed" => State::Unsubscribed,
        "meetingBooked" => State::MeetingBooked,
        s if s.ends_with("NotInterested") || s == "notInterested" => State::NotInterested,
        s if s.ends_with("Interested") || s == "interested" => State::Interested,
        s if s.ends_with("Replied") => State::Replied,
        "imported" | "scanned" | "skipped" | "reviewed" | "paused" | "" => State::New,
        _ => State::Contacted,
    };
    lead
}

pub async fn blocklist(client: &Client) -> AppResult<Vec<String>> {
    let items = pages(client, "/v2/unsubscribes/variables", &[]).await?;
    Ok(items.iter().filter_map(|e| e["value"].as_str().map(str::to_string)).collect())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{parse_lead, parse_steps};
    use crate::outreach::State;

    #[test]
    fn only_email_steps_are_copied_with_the_wait_before_them() {
        let body = json!({"seq_1": {"_id": "seq_1", "steps": [
            {"type": "linkedinVisit", "index": 1, "delay": 0},
            {"type": "email", "index": 0, "delay": 0, "subject": "{{companyName}} and Fieldnote", "message": "<p>Hi {{firstName}},</p><p>{{icebreaker}}</p>"},
            {"type": "linkedinInvite", "index": 2, "delay": 1},
            {"type": "email", "index": 3, "delay": 2, "subject": "", "message": "Any thoughts, {{firstName}}?"},
        ]}});
        let steps = parse_steps(&body);
        assert_eq!(steps.iter().map(|s| (s.delay_days, s.subject.as_str(), s.body.as_str())).collect::<Vec<_>>(),
            [(0, "{company} and Fieldnote", "Hi {first_name},\n\n{{icebreaker}}"), (3, "", "Any thoughts, {first_name}?")]);
    }

    #[test]
    fn a_lead_keeps_what_lemlist_knows() {
        let lead = parse_lead(&json!({"email": "cy@delta.io", "firstName": "Cy", "jobTitle": "VP Sales", "companyDomain": "delta.io",
            "icebreaker": "Saw your post", "lastState": "emailsOpened", "_id": "lea_1", "Seats": 40}));
        assert_eq!((lead.state, lead.title.as_str(), lead.company_domain.as_str(), lead.hook.as_str()), (State::Contacted, "VP Sales", "delta.io", "Saw your post"));
        assert_eq!(lead.custom, [("Seats".to_string(), "40".to_string())]);
        for (state, expected) in [("emailsBounced", State::Bounced), ("emailsUnsubscribed", State::Unsubscribed), ("emailsReplied", State::Replied),
            ("emailsInterested", State::Interested), ("emailsNotInterested", State::NotInterested), ("meetingBooked", State::MeetingBooked),
            ("imported", State::New)] {
            assert_eq!(parse_lead(&json!({"lastState": state})).state, expected, "{state}");
        }
    }
}
