//! A website form (Tally, Typeform, Webflow, or anything that can POST JSON) adds whoever fills it in as a lead.
//! The secret in the URL picks the project, like the booking webhook's.

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::util_input;
use super::util_lead_import::{self, NewLead};
use crate::error::{AppError, AppResult};
use crate::hooks::{self, ChatAlert};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/forms/{token}", post(submitted))
}

/// The (question, answer) pairs in a submission. Tally sends `data.fields[]`, Typeform `form_response.answers[]` with the
/// questions in `form_response.definition.fields[]`, Webflow `payload.data`, and anything else flat JSON.
fn answers(body: &Value) -> Vec<(String, String)> {
    if let Some(fields) = body["data"]["fields"].as_array() {
        return fields
            .iter()
            .filter_map(|f| {
                // Tally's choice answers are option ids, with the options' text alongside.
                let options = f["options"].as_array().map(Vec::as_slice).unwrap_or_default();
                let chosen = |id: &Value| options.iter().find(|o| o["id"] == *id).map_or(id.clone(), |o| o["text"].clone());
                let value = match &f["value"] {
                    Value::Array(ids) if !options.is_empty() => Value::Array(ids.iter().map(chosen).collect()),
                    value => value.clone(),
                };
                Some((f["label"].as_str()?.to_string(), text(&value)?))
            })
            .collect();
    }
    if let Some(answers) = body["form_response"]["answers"].as_array() {
        let fields = body["form_response"]["definition"]["fields"].as_array().map(Vec::as_slice).unwrap_or_default();
        let titles: HashMap<&str, &str> = fields.iter().filter_map(|f| Some((f["id"].as_str()?, f["title"].as_str()?))).collect();
        return answers
            .iter()
            .filter_map(|a| {
                let kind = a["type"].as_str()?;
                let title = a["field"]["id"].as_str().and_then(|id| titles.get(id)).copied().unwrap_or(kind);
                Some((title.to_string(), text(&a[kind])?))
            })
            .collect();
    }
    let flat = if body["payload"]["data"].is_object() { &body["payload"]["data"] } else { body };
    flat.as_object().map(|o| o.iter().filter_map(|(k, v)| Some((k.clone(), text(v)?))).collect()).unwrap_or_default()
}

/// An answer as text: choices joined with commas, and Typeform's `{"label"}` and `{"labels"}` choice objects read.
fn text(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => if *b { "Yes" } else { "No" }.to_string(),
        Value::Array(items) => items.iter().filter_map(text).collect::<Vec<_>>().join(", "),
        Value::Object(o) => return o.get("label").or_else(|| o.get("labels")).and_then(text),
        Value::Null => return None,
    };
    (!text.trim().is_empty()).then_some(text)
}

#[derive(Clone, Copy, PartialEq)]
enum Slot {
    FirstName,
    LastName,
    FullName,
    Title,
    Company,
    Website,
    Linkedin,
}

/// Which lead field a question fills, from its words. The order matters: "Company website" is a website and
/// "Company name" a company, not a name. Questions about the company that aren't its name, like its size, fill nothing.
fn slot(question: &str) -> Option<Slot> {
    let words: Vec<String> = question.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    let has = |options: &[&str]| words.iter().any(|w| options.contains(&w.as_str()));
    if has(&["linkedin"]) {
        Some(Slot::Linkedin)
    } else if has(&["website", "domain", "site", "url"]) {
        Some(Slot::Website)
    } else if has(&["first", "firstname", "given"]) {
        Some(Slot::FirstName)
    } else if has(&["last", "lastname", "surname", "family"]) {
        Some(Slot::LastName)
    } else if has(&["title", "jobtitle", "role", "position"]) {
        Some(Slot::Title)
    } else if has(&["size", "employees", "industry", "revenue", "type"]) {
        None
    } else if has(&["company", "companyname", "organization", "organisation"]) {
        Some(Slot::Company)
    } else if has(&["name", "fullname"]) {
        Some(Slot::FullName)
    } else {
        None
    }
}

/// The lead in a submission, and the answers that fit no lead field, like a message, for the lead's notes.
fn lead_from(answers: Vec<(String, String)>) -> (NewLead, Vec<(String, String)>) {
    let mut lead = NewLead::default();
    let mut rest = Vec::new();
    for (question, answer) in answers {
        if answer.trim().is_empty() {
            continue;
        }
        if lead.email.is_empty()
            && let Some(email) = util_input::email(&answer)
        {
            lead.email = email;
            continue;
        }
        let field = match slot(&question) {
            Some(Slot::FirstName) => &mut lead.first_name,
            Some(Slot::LastName) => &mut lead.last_name,
            Some(Slot::FullName) => &mut lead.full_name,
            Some(Slot::Title) => &mut lead.title,
            Some(Slot::Company) => &mut lead.company_name,
            Some(Slot::Website) => &mut lead.company_domain,
            Some(Slot::Linkedin) => &mut lead.linkedin_url,
            None => {
                rest.push((question, answer));
                continue;
            }
        };
        if field.is_empty() {
            *field = answer;
        } else {
            rest.push((question, answer));
        }
    }
    (lead, rest)
}

#[derive(Deserialize)]
struct FormQuery {
    // Text, not a Uuid, so a mangled id gets the same {"added": 0} as an unknown one instead of a 400.
    campaign: Option<String>,
}

/// Always answers 2xx with how many leads were added, so a form tool doesn't retry a duplicate or a suppressed address,
/// and the URL doesn't reveal who is on the do-not-contact list.
async fn submitted(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<FormQuery>,
    Json(body): Json<Value>,
) -> AppResult<Json<Value>> {
    let (project_id, org_id): (Uuid, Uuid) =
        sqlx::query_as("select p.id, p.org_id from project_settings s join projects p on p.id = s.project_id where s.form_token = $1")
            .bind(&token)
            .fetch_optional(&state.db)
            .await?
            .ok_or(AppError::NotFound)?;
    let (mut lead, rest) = lead_from(answers(&body));
    if let Some(campaign) = q.campaign {
        let Ok(segment_id) = campaign.parse() else { return Ok(Json(json!({ "added": 0 }))) };
        lead.segment_id = Some(segment_id);
    }
    let email = lead.email.clone();
    let outcome = util_lead_import::insert(&state.db, org_id, project_id, "form", vec![(1, lead)]).await?;
    if outcome.imported == 0 {
        return Ok(Json(json!({ "added": 0 })));
    }
    let notes = rest.iter().map(|(q, a)| format!("{q}: {a}")).collect::<Vec<_>>().join("\n");
    let lead: crate::models::Lead = sqlx::query_as("update leads set notes = $3 where project_id = $1 and email = $2::citext returning *")
        .bind(project_id)
        .bind(&email)
        .bind(&notes)
        .fetch_one(&state.db)
        .await?;
    let state = state.clone();
    tokio::spawn(async move {
        let project: String =
            sqlx::query_scalar("select name from projects where id = $1").bind(project_id).fetch_one(&state.db).await.unwrap_or_default();
        let who = match (lead.full_name.as_str(), lead.company_name.as_str()) {
            ("", _) => email,
            (name, "") => name.to_string(),
            (name, company) => format!("{name} ({company})"),
        };
        let summary = format!("{who} for {project}");
        let link = hooks::lead_url(&state.cfg.public_url, project_id, lead.id);
        let alert = ChatAlert { label: "New form lead", summary: &summary, quote: (!notes.is_empty()).then_some(&notes), link: &link, link_text: "Open the lead" };
        hooks::post_chat(&state.db, project_id, &alert).await;
    });
    Ok(Json(json!({ "added": 1 })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn reads_tally_typeform_webflow_and_plain_forms() {
        let tally = json!({"eventType": "FORM_RESPONSE", "data": {"fields": [
            {"label": "Full name", "type": "INPUT_TEXT", "value": "Sarah Chen"},
            {"label": "Work email", "type": "INPUT_EMAIL", "value": "sarah@brightline.example"},
            {"label": "Company size", "type": "MULTIPLE_CHOICE", "value": ["o2"], "options": [{"id": "o1", "text": "1-10"}, {"id": "o2", "text": "11-50"}]},
            {"label": "Seats", "type": "INPUT_NUMBER", "value": 12},
            {"label": "Company", "type": "INPUT_TEXT", "value": "Brightline"},
        ]}});
        let (lead, rest) = lead_from(answers(&tally));
        assert_eq!((lead.full_name.as_str(), lead.email.as_str(), lead.company_name.as_str()), ("Sarah Chen", "sarah@brightline.example", "Brightline"));
        assert_eq!(rest, [("Company size".to_string(), "11-50".to_string()), ("Seats".to_string(), "12".to_string())]);

        let typeform = json!({"event_type": "form_response", "form_response": {
            "definition": {"fields": [{"id": "a", "title": "What's your first name?"}, {"id": "b", "title": "And your email?"}, {"id": "c", "title": "What can we help with?"}]},
            "answers": [
                {"type": "text", "text": "Marcus", "field": {"id": "a", "type": "short_text"}},
                {"type": "email", "email": "marcus@fernway.example", "field": {"id": "b", "type": "email"}},
                {"type": "text", "text": "Pricing for 12 seats", "field": {"id": "c", "type": "long_text"}},
                {"type": "choices", "choices": {"labels": ["HubSpot", "Gmail"]}, "field": {"id": "d", "type": "multiple_choice"}},
            ],
        }});
        let (lead, rest) = lead_from(answers(&typeform));
        assert_eq!((lead.first_name.as_str(), lead.email.as_str()), ("Marcus", "marcus@fernway.example"));
        assert_eq!(rest[0], ("What can we help with?".to_string(), "Pricing for 12 seats".to_string()));
        assert_eq!(rest[1].1, "HubSpot, Gmail");

        let webflow = json!({"triggerType": "form_submission", "payload": {"name": "Contact", "data": {"Name": "Priya", "Email": "priya@corvid.example", "Company Website": "corvid.example"}}});
        let (lead, _) = lead_from(answers(&webflow));
        assert_eq!((lead.full_name.as_str(), lead.company_domain.as_str()), ("Priya", "corvid.example"));

        let plain = json!({"email": " Tom@Halcyon.example ", "first_name": "Tom", "lastName": "Alvarez", "job_title": "Director of Ops", "linkedin": "https://linkedin.com/in/tom"});
        let (lead, _) = lead_from(answers(&plain));
        assert_eq!((lead.first_name.as_str(), lead.last_name.as_str(), lead.title.as_str()), ("Tom", "Alvarez", "Director of Ops"));
        assert_eq!(lead.linkedin_url, "https://linkedin.com/in/tom");
        assert_eq!(lead.email, "tom@halcyon.example");
    }
}
