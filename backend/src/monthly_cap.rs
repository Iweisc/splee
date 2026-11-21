//! A project's monthly cap (migration 0190), explee.com's `monthly-cap`: the most the project may spend on emails in a calendar
//! month (UTC), counted from what its sent emails cost at their mailbox's price per email. Once the month's spend reaches it, the
//! sender holds first emails and follow-ups until the 1st or a higher cap. Replies to leads still go out, as with the daily limit.

use serde::Serialize;
use sqlx::PgPool;
use uuid::Uuid;

use crate::autopilot::{month_start, next_month};
use crate::models::Ts;

#[derive(Serialize, Debug, PartialEq)]
pub struct Cap {
    pub project_id: Uuid,
    /// Null for no cap.
    pub monthly_cap_micros: Option<i64>,
    /// What the project's sent emails cost since the 1st (UTC), with or without a cap.
    pub spent_this_month_micros: i64,
    /// First emails and follow-ups wait until `resumes_at`.
    pub reached: bool,
    /// The 1st of next month (UTC) while the cap is reached, else null.
    pub resumes_at: Option<Ts>,
}

/// What the project's sent emails cost since `since`.
pub async fn spent_since(db: &PgPool, project_id: Uuid, since: Ts) -> sqlx::Result<i64> {
    sqlx::query_scalar(
        "select coalesce(sum(cost_micros), 0)::bigint from email_messages
         where project_id = $1 and direction = 'outbound' and status = 'sent' and sent_at >= $2",
    )
    .bind(project_id)
    .bind(since)
    .fetch_one(db)
    .await
}

/// What one email cost, 0 until it was sent.
pub async fn cost_of(db: &PgPool, email_id: Uuid) -> sqlx::Result<i64> {
    let cost: Option<i64> = sqlx::query_scalar("select cost_micros from email_messages where id = $1 and status = 'sent'")
        .bind(email_id)
        .fetch_optional(db)
        .await?;
    Ok(cost.unwrap_or(0))
}

/// The project's cap and this month's spend, at `now`.
pub async fn status(db: &PgPool, project_id: Uuid, now: Ts) -> sqlx::Result<Cap> {
    let cap: Option<i64> = sqlx::query_scalar("select monthly_cap_micros from project_settings where project_id = $1")
        .bind(project_id)
        .fetch_optional(db)
        .await?
        .flatten();
    let spent = spent_since(db, project_id, month_start(now)).await?;
    let reached = cap.is_some_and(|cap| spent >= cap);
    Ok(Cap { project_id, monthly_cap_micros: cap, spent_this_month_micros: spent, reached, resumes_at: reached.then(|| next_month(now)) })
}
