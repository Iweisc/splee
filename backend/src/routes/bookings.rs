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

/// The emails in `payload.attendees`, or Zcal's `data.attendees`, leaving out the host. SavvyCal lists the organizer there
/// too, Cal.com and Zcal don't.
fn attendees(body: &Value) -> Vec<&Value> {
    let list = body["payload"]["attendees"].as_array().or(body["data"]["attendees"].as_array());
    list.map(|a| a.iter().filter(|x| x["is_organizer"] != true).map(|x| &x["email"]).collect()).unwrap_or_default()
}

/// The invitee emails in a new or moved booking, or none for other events (cancellations, pings).
/// Calendly sends `invitee.created` with `payload.email` (a reschedule is a new invitee), Cal.com `BOOKING_CREATED` or
/// `BOOKING_RESCHEDULED` and SavvyCal `event.created` or `event.rescheduled` with `payload.attendees[].email`, OnceHub
/// `booking.scheduled` or `booking.rescheduled` with `data.form_submission.email`, Zcal `event.created` or
/// `event.rescheduled` with `data.attendees[].email` (the same names as SavvyCal's), TidyCal `booking.created`,
/// `booking.approved` (a booking that needed approval) or `booking.rescheduled` with `data.contact.email`, and anything
/// else, like a Zapier step, can send `{"email": ...}`.
fn booked_emails(body: &Value) -> Vec<String> {
    let found: Vec<&Value> = match (body["event"].as_str(), body["triggerEvent"].as_str(), body["type"].as_str()) {
        (Some("invitee.created"), _, _) => vec![&body["payload"]["email"]],
        (_, Some("BOOKING_CREATED" | "BOOKING_RESCHEDULED"), _) | (_, _, Some("event.created" | "event.rescheduled")) => attendees(body),
        (_, _, Some("booking.scheduled")) => vec![&body["data"]["form_submission"]["email"]],
        (_, _, Some("booking.created" | "booking.approved")) => vec![&body["data"]["contact"]["email"]],
        // OnceHub and TidyCal both call it this, and each has only its own field.
        (_, _, Some("booking.rescheduled")) => vec![&body["data"]["form_submission"]["email"], &body["data"]["contact"]["email"]],
        (None, None, None) => vec![&body["email"]],
        _ => vec![],
    };
    found.into_iter().filter_map(Value::as_str).map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
}

/// The invitee emails in a cancelled booking. Calendly cancels the old invitee when someone reschedules, with
/// `payload.rescheduled` set, so that one isn't a cancellation, and neither is OnceHub's `booking.canceled_then_rescheduled`,
/// whose new booking arrives as `booking.scheduled`. OnceHub's host cancelling to ask for a new time is one.
fn cancelled_emails(body: &Value) -> Vec<String> {
    let found: Vec<&Value> = match (body["event"].as_str(), body["triggerEvent"].as_str(), body["type"].as_str()) {
        (Some("invitee.canceled"), _, _) if body["payload"]["rescheduled"] != true => vec![&body["payload"]["email"]],
        // SavvyCal spells it canceled, Zcal cancelled.
        (_, Some("BOOKING_CANCELLED"), _) | (_, _, Some("event.canceled" | "event.cancelled")) => attendees(body),
        (_, _, Some("booking.canceled" | "booking.canceled_reschedule_requested")) => vec![&body["data"]["form_submission"]["email"]],
        // TidyCal spells it cancelled, OnceHub canceled.
        (_, _, Some("booking.cancelled")) => vec![&body["data"]["contact"]["email"]],
        _ => vec![],
    };
    found.into_iter().filter_map(Value::as_str).map(|e| e.trim().to_lowercase()).filter(|e| e.contains('@')).collect()
}

/// When the booked call starts: Calendly's `payload.scheduled_event.start_time`, Cal.com's `payload.startTime`,
/// SavvyCal's `payload.start_at`, OnceHub's `data.starting_time`, Zcal's `data.startDate`, TidyCal's `data.starts_at`, or
/// a plain `start_time`. None when the tool didn't say.
fn meeting_start(body: &Value) -> Option<Ts> {
    let (payload, data) = (&body["payload"], &body["data"]);
    [&payload["scheduled_event"]["start_time"], &payload["startTime"], &payload["start_at"], &data["starting_time"], &data["startDate"], &data["starts_at"], &body["start_time"]]
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
    let rescheduled =
        body["triggerEvent"] == "BOOKING_RESCHEDULED" || body["type"] == "event.rescheduled" || body["type"] == "booking.rescheduled";
    for lead in &leads {
        // Set before the move below, so its alerts carry the time. Only the request that changes the time gets the lead
        // back, if the tool retries.
        let retimed: Option<Lead> = match at {
            Some(at) => {
                sqlx::query_as("update leads set meeting_at = $2 where id = $1 and meeting_at is distinct from $2 returning *")
                    .bind(lead.id)
                    .bind(at)
                    .fetch_optional(&state.db)
                    .await?
            }
            None => None,
        };
        // A booked call at a new time was moved. Cal.com and SavvyCal say so, Calendly books a new invitee.
        if lead.status == "meeting_booked"
            && lead.meeting_at.is_some()
            && let Some(retimed) = retimed
        {
            hooks::announce_rescheduled(&state, &retimed, lead.meeting_at);
            continue;
        }
        // Otherwise a reschedule only moves the time. Its lead was marked when the call was first booked.
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
        // SavvyCal lists the host among the attendees.
        let attendees = json!([{"email": "host@us.com", "is_organizer": true}, {"email": "Jane@Acme.com", "is_organizer": false}]);
        let savvycal = json!({"type": "event.created", "id": "payload_1", "payload": {"attendees": attendees, "start_at": "2026-11-10T14:00:00Z"}});
        assert_eq!(booked_emails(&savvycal), ["jane@acme.com"]);
        assert_eq!(booked_emails(&json!({"type": "event.rescheduled", "payload": {"attendees": attendees}})), ["jane@acme.com"]);
        assert!(booked_emails(&json!({"type": "event.requested", "payload": {"attendees": attendees}})).is_empty());
        assert!(booked_emails(&json!({"type": "event.canceled", "payload": {"attendees": attendees}})).is_empty());
        assert_eq!(meeting_start(&savvycal).map(|t| t.to_rfc3339()), Some("2026-11-10T14:00:00+00:00".into()));
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
        let savvycal = json!({"type": "event.canceled", "payload": {"attendees": [{"email": "host@us.com", "is_organizer": true}, {"email": "d@w.com"}]}});
        assert_eq!(cancelled_emails(&savvycal), ["d@w.com"]);
    }

    #[test]
    fn reads_oncehub_bookings() {
        let booking = |kind: &str| {
            let form = json!({"name": "Carrie", "email": "Carrie@Customer.com"});
            json!({"object": "event", "type": kind, "data": {"starting_time": "2026-11-10T16:45:00.000Z", "form_submission": form}})
        };
        for kind in ["booking.scheduled", "booking.rescheduled"] {
            assert_eq!(booked_emails(&booking(kind)), ["carrie@customer.com"]);
            assert!(cancelled_emails(&booking(kind)).is_empty());
        }
        for kind in ["booking.canceled", "booking.canceled_reschedule_requested"] {
            assert_eq!(cancelled_emails(&booking(kind)), ["carrie@customer.com"]);
            assert!(booked_emails(&booking(kind)).is_empty());
        }
        for kind in ["booking.canceled_then_rescheduled", "booking.completed", "booking.no_show"] {
            assert!(booked_emails(&booking(kind)).is_empty() && cancelled_emails(&booking(kind)).is_empty(), "{kind}");
        }
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-10T16:45:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(meeting_start(&booking("booking.scheduled")), Some(at));
    }

    #[test]
    fn reads_zcal_bookings() {
        // Zcal's documented sample: the invitee and a guest are attendees, the host isn't.
        let event = |kind: &str| {
            let attendees = json!([{"name": "Jane", "email": "Jane@Acme.com", "type": "invitee"}, {"email": "guest@acme.com", "type": "guest"}]);
            let hosts = json!([{"name": "Host", "email": "host@us.com"}]);
            json!({"type": kind, "created_at": "2025-02-12T19:00:00.000Z", "data": {"startDate": "2025-02-12T20:30:00.000Z", "hosts": hosts, "attendees": attendees}})
        };
        for kind in ["event.created", "event.rescheduled"] {
            assert_eq!(booked_emails(&event(kind)), ["jane@acme.com", "guest@acme.com"]);
            assert!(cancelled_emails(&event(kind)).is_empty());
        }
        assert_eq!(cancelled_emails(&event("event.cancelled")), ["jane@acme.com", "guest@acme.com"]);
        assert!(booked_emails(&event("event.cancelled")).is_empty());
        let at = chrono::DateTime::parse_from_rfc3339("2025-02-12T20:30:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(meeting_start(&event("event.created")), Some(at));
    }

    #[test]
    fn reads_tidycal_bookings() {
        // TidyCal's documented booking.created, trimmed. The other booking events have the same shape.
        let booking = |kind: &str| {
            let contact = json!({"id": 55, "name": "Ada Lovelace", "email": "Ada@Example.com", "timezone": "Europe/London"});
            let data = json!({"id": 101, "starts_at": "2026-10-28T16:00:00.000000Z", "contact": contact, "hosts": [{"id": 3, "name": "Grace"}]});
            json!({"id": "6f1b7c52", "type": kind, "created_at": "2026-10-21T15:04:05+00:00", "data": data})
        };
        for kind in ["booking.created", "booking.approved", "booking.rescheduled"] {
            assert_eq!(booked_emails(&booking(kind)), ["ada@example.com"], "{kind}");
            assert!(cancelled_emails(&booking(kind)).is_empty(), "{kind}");
        }
        assert_eq!(cancelled_emails(&booking("booking.cancelled")), ["ada@example.com"]);
        assert!(booked_emails(&booking("booking.cancelled")).is_empty());
        for kind in ["contact.created", "payment.completed", "webhook.test"] {
            assert!(booked_emails(&booking(kind)).is_empty() && cancelled_emails(&booking(kind)).is_empty(), "{kind}");
        }
        let at = chrono::DateTime::parse_from_rfc3339("2026-10-28T16:00:00Z").unwrap().with_timezone(&chrono::Utc);
        assert_eq!(meeting_start(&booking("booking.created")), Some(at));
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
