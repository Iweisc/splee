//! A scheduling tool tells Splee when a lead books a call, so the lead moves to meeting_booked without anyone marking it.
//! The secret in the URL picks the project, like an unsubscribe link picks the lead.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::Lead;
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/bookings/{token}", post(booked))
}

/// The invitee emails in a new booking, or none for other events (cancellations, reschedules, pings).
/// Calendly sends `invitee.created` with `payload.email`, Cal.com `BOOKING_CREATED` with `payload.attendees[].email`, and
/// anything else, like a Zapier step, can send `{"email": ...}`.
fn booked_emails(body: &Value) -> Vec<String> {
    let found: Vec<&Value> = match (body["event"].as_str(), body["triggerEvent"].as_str()) {
        (Some("invitee.created"), _) => vec![&body["payload"]["email"]],
        (_, Some("BOOKING_CREATED")) => body["payload"]["attendees"].as_array().map(|a| a.iter().map(|x| &x["email"]).collect()).unwrap_or_default(),
        (None, None) => vec![&body["email"]],
        _ => vec![],
    };
    found.into_iter().filter_map(Value::as_str).map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
}

async fn booked(State(state): State<AppState>, Path(token): Path<String>, Json(body): Json<Value>) -> AppResult<Json<Value>> {
    let project_id: Uuid = sqlx::query_scalar("select project_id from project_settings where booking_token = $1")
        .bind(&token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let emails = booked_emails(&body);
    if emails.is_empty() {
        return Ok(Json(json!({ "matched": 0 })));
    }
    let leads: Vec<Lead> = sqlx::query_as("select * from leads where project_id = $1 and lower(email::text) = any($2)")
        .bind(project_id)
        .bind(&emails)
        .fetch_all(&state.db)
        .await?;
    for lead in &leads {
        // Only the request that moves the lead sends the alerts, if the tool retries or two bookings arrive at once.
        let moved: Option<Lead> = sqlx::query_as("update leads set status = 'meeting_booked' where id = $1 and status <> 'meeting_booked' returning *")
            .bind(lead.id)
            .fetch_optional(&state.db)
            .await?;
        if let Some(moved) = moved {
            hooks::status_changed(&state, &lead.status, &moved).await;
        }
    }
    Ok(Json(json!({ "matched": leads.len() })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::booked_emails;

    #[test]
    fn reads_calendly_cal_com_and_plain_bookings() {
        let calendly = json!({"event": "invitee.created", "payload": {"email": "Sarah@Brightline.com", "name": "Sarah"}});
        assert_eq!(booked_emails(&calendly), ["sarah@brightline.com"]);
        let cal = json!({"triggerEvent": "BOOKING_CREATED", "payload": {"attendees": [{"email": "a@x.com"}, {"email": "b@y.com"}]}});
        assert_eq!(booked_emails(&cal), ["a@x.com", "b@y.com"]);
        assert_eq!(booked_emails(&json!({"email": "c@z.com"})), ["c@z.com"]);
        assert!(booked_emails(&json!({"event": "invitee.canceled", "payload": {"email": "a@x.com"}})).is_empty());
        assert!(booked_emails(&json!({"triggerEvent": "BOOKING_CANCELLED", "payload": {"attendees": [{"email": "a@x.com"}]}})).is_empty());
        assert!(booked_emails(&json!({"triggerEvent": "PING"})).is_empty());
    }
}
