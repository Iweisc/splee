//! A scheduling tool tells Splee when a lead books, moves or cancels a call, so the lead's status and meeting time follow
//! without anyone marking them.
//! The secret in the URL picks the project, like an unsubscribe link picks the lead.

use axum::extract::{Path, State};
use axum::routing::post;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::hooks;
use crate::models::{Lead, Ts};
use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new().route("/bookings/{token}", post(booked))
}

/// The invitee emails in a new or moved booking, or none for other events (cancellations, pings).
/// Calendly sends `invitee.created` with `payload.email` (a reschedule is a new invitee), Cal.com `BOOKING_CREATED` or
/// `BOOKING_RESCHEDULED` with `payload.attendees[].email`, and anything else, like a Zapier step, can send `{"email": ...}`.
fn booked_emails(body: &Value) -> Vec<String> {
    let found: Vec<&Value> = match (body["event"].as_str(), body["triggerEvent"].as_str()) {
        (Some("invitee.created"), _) => vec![&body["payload"]["email"]],
        (_, Some("BOOKING_CREATED" | "BOOKING_RESCHEDULED")) => {
            body["payload"]["attendees"].as_array().map(|a| a.iter().map(|x| &x["email"]).collect()).unwrap_or_default()
        }
        (None, None) => vec![&body["email"]],
        _ => vec![],
    };
    found.into_iter().filter_map(Value::as_str).map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
}

/// The invitee emails in a cancelled booking. Calendly cancels the old invitee when someone reschedules, with
/// `payload.rescheduled` set, so that one isn't a cancellation.
fn cancelled_emails(body: &Value) -> Vec<String> {
    let found: Vec<&Value> = match (body["event"].as_str(), body["triggerEvent"].as_str()) {
        (Some("invitee.canceled"), _) if body["payload"]["rescheduled"] != true => vec![&body["payload"]["email"]],
        (_, Some("BOOKING_CANCELLED")) => {
            body["payload"]["attendees"].as_array().map(|a| a.iter().map(|x| &x["email"]).collect()).unwrap_or_default()
        }
        _ => vec![],
    };
    found.into_iter().filter_map(Value::as_str).map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
}

/// When the booked call starts: Calendly's `payload.scheduled_event.start_time`, Cal.com's `payload.startTime`, or a
/// plain `start_time`. None when the tool didn't say.
fn meeting_start(body: &Value) -> Option<Ts> {
    [&body["payload"]["scheduled_event"]["start_time"], &body["payload"]["startTime"], &body["start_time"]]
        .into_iter()
        .find_map(|v| DateTime::parse_from_rfc3339(v.as_str()?).ok())
        .map(|t| t.with_timezone(&Utc))
}

async fn booked(State(state): State<AppState>, Path(token): Path<String>, Json(body): Json<Value>) -> AppResult<Json<Value>> {
    let project_id: Uuid = sqlx::query_scalar("select project_id from project_settings where booking_token = $1")
        .bind(&token)
        .fetch_optional(&state.db)
        .await?
        .ok_or(AppError::NotFound)?;
    let cancelled = cancelled_emails(&body);
    if !cancelled.is_empty() {
        return cancel(&state, project_id, &cancelled).await;
    }
    let emails = booked_emails(&body);
    if emails.is_empty() {
        return Ok(Json(json!({ "matched": 0 })));
    }
    let leads: Vec<Lead> = sqlx::query_as("select * from leads where project_id = $1 and lower(email::text) = any($2)")
        .bind(project_id)
        .bind(&emails)
        .fetch_all(&state.db)
        .await?;
    let at = meeting_start(&body);
    let rescheduled = body["triggerEvent"] == "BOOKING_RESCHEDULED";
    for lead in &leads {
        // Set before the move below, so its alerts carry the time.
        if at.is_some() {
            sqlx::query("update leads set meeting_at = $2 where id = $1").bind(lead.id).bind(at).execute(&state.db).await?;
        }
        // A reschedule only moves the time. Its lead was marked when the call was first booked.
        if rescheduled {
            continue;
        }
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

/// A cancelled call puts a booked lead back to Hot, since they were keen enough to book, and drops the stale time.
async fn cancel(state: &AppState, project_id: Uuid, emails: &[String]) -> AppResult<Json<Value>> {
    let leads: Vec<Lead> =
        sqlx::query_as("select * from leads where project_id = $1 and lower(email::text) = any($2) and status = 'meeting_booked'")
            .bind(project_id)
            .bind(emails)
            .fetch_all(&state.db)
            .await?;
    for lead in &leads {
        // The status check makes a retried or doubled cancellation alert once.
        let moved: Option<Lead> =
            sqlx::query_as("update leads set status = 'hot', meeting_at = null where id = $1 and status = 'meeting_booked' returning *")
                .bind(lead.id)
                .fetch_optional(&state.db)
                .await?;
        if let Some(moved) = moved {
            hooks::announce_cancelled(state, &moved, lead.meeting_at);
        }
    }
    Ok(Json(json!({ "matched": leads.len() })))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{booked_emails, cancelled_emails, meeting_start};

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
        let moved = json!({"triggerEvent": "BOOKING_RESCHEDULED", "payload": {"attendees": [{"email": "a@x.com"}]}});
        assert_eq!(booked_emails(&moved), ["a@x.com"]);
    }

    #[test]
    fn reads_cancellations_but_not_calendly_reschedules() {
        let calendly = json!({"event": "invitee.canceled", "payload": {"email": "A@x.com", "rescheduled": false}});
        assert_eq!(cancelled_emails(&calendly), ["a@x.com"]);
        let moved = json!({"event": "invitee.canceled", "payload": {"email": "a@x.com", "rescheduled": true}});
        assert!(cancelled_emails(&moved).is_empty());
        let cal = json!({"triggerEvent": "BOOKING_CANCELLED", "payload": {"attendees": [{"email": "b@y.com"}]}});
        assert_eq!(cancelled_emails(&cal), ["b@y.com"]);
        assert!(cancelled_emails(&json!({"triggerEvent": "BOOKING_CREATED", "payload": {"attendees": [{"email": "b@y.com"}]}})).is_empty());
        assert!(cancelled_emails(&json!({"email": "c@z.com"})).is_empty());
    }

    #[test]
    fn reads_the_meeting_time() {
        let at = |s: &str| chrono::DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&chrono::Utc);
        let calendly = json!({"event": "invitee.created", "payload": {"email": "a@x.com", "scheduled_event": {"start_time": "2026-11-10T14:00:00.000000Z"}}});
        assert_eq!(meeting_start(&calendly), Some(at("2026-11-10T14:00:00Z")));
        let cal = json!({"triggerEvent": "BOOKING_CREATED", "payload": {"startTime": "2026-11-10T15:30:00+01:00", "attendees": []}});
        assert_eq!(meeting_start(&cal), Some(at("2026-11-10T14:30:00Z")));
        assert_eq!(meeting_start(&json!({"email": "a@x.com", "start_time": "2026-11-10T09:00:00Z"})), Some(at("2026-11-10T09:00:00Z")));
        assert_eq!(meeting_start(&json!({"email": "a@x.com"})), None);
        assert_eq!(meeting_start(&json!({"email": "a@x.com", "start_time": "next tuesday"})), None);
    }
}
