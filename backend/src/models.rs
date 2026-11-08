//! Row types shared by routes and workers. Field names match the columns in
//! migrations/0001_init.sql and the TypeScript types in frontend/src/lib/types.ts.
//! Secret columns are either left out of these structs or marked #[serde(skip)].

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::FromRow;
use uuid::Uuid;

pub type Ts = DateTime<Utc>;

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Organization {
    pub id: Uuid,
    pub name: String,
    pub created_at: Ts,
}

/// How long an invite link works, so a forwarded or forgotten link can't be used to join months later.
pub const INVITE_TTL: &str = "7 days";

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Invite {
    pub id: Uuid,
    pub org_id: Uuid,
    pub email: String,
    pub role: String,
    #[serde(skip)]
    pub token: String,
    pub invited_by: Option<Uuid>,
    pub accepted_at: Option<Ts>,
    pub created_at: Ts,
    /// Not a column: queries select it as created_at + INVITE_TTL.
    pub expires_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ApiKey {
    pub id: Uuid,
    pub org_id: Uuid,
    pub name: String,
    pub prefix: String,
    pub last_used_at: Option<Ts>,
    pub revoked_at: Option<Ts>,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct NotificationPrefs {
    pub user_id: Uuid,
    pub hot_lead_email: bool,
    pub daily_digest: bool,
    pub weekly_report: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Project {
    pub id: Uuid,
    pub org_id: Option<Uuid>,
    pub domain: Option<String>,
    pub business_description: Option<String>,
    pub name: String,
    pub status: String,
    pub company: Value,
    pub created_by: Option<Uuid>,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProjectSettings {
    pub project_id: Uuid,
    pub daily_email_limit: i32,
    pub timezone: String,
    pub send_days: Vec<i32>,
    pub send_start_hour: i32,
    pub send_end_hour: i32,
    pub follow_ups: Value,
    pub booking_link: String,
    pub auto_reply_mode: String,
    pub auto_reply_instructions: String,
    pub sender_name: String,
    pub signature: String,
    pub tone: String,
    pub email_language: String,
    pub email_instructions: String,
    pub cta: String,
    pub max_words: i32,
    pub channels: Value,
    pub targeting: Value,
    pub webhook_url: String,
    pub slack_webhook_url: String,
    pub webhook_secret: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Competitor {
    pub id: Uuid,
    pub project_id: Uuid,
    pub domain: String,
    pub name: String,
    pub note: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Segment {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub description: String,
    pub icon: String,
    pub est_size: i64,
    pub fit_score: i32,
    pub status: String,
    pub targeting: Value,
    pub pain_points: String,
    pub angle: String,
    pub position: i32,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Company {
    pub id: Uuid,
    pub project_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub domain: String,
    pub name: String,
    pub description: String,
    pub city: String,
    pub country_code: String,
    pub employees: String,
    pub fit_score: i32,
    pub fit_reason: String,
    pub source_url: String,
    pub status: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Lead {
    pub id: Uuid,
    pub project_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub company_id: Option<Uuid>,
    pub full_name: String,
    pub first_name: String,
    pub last_name: String,
    pub title: String,
    pub email: Option<String>,
    pub email_status: String,
    pub linkedin_url: String,
    pub x_url: String,
    pub company_name: String,
    pub company_domain: Option<String>,
    pub city: String,
    pub country_code: String,
    pub source: String,
    pub status: String,
    pub personalization: Value,
    #[serde(skip)]
    pub unsubscribe_token: String,
    pub last_contacted_at: Option<Ts>,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Mailbox {
    pub id: Uuid,
    pub org_id: Uuid,
    pub email: String,
    pub from_name: String,
    pub smtp_host: String,
    pub smtp_port: i32,
    pub smtp_security: String,
    pub smtp_username: String,
    #[serde(skip)]
    pub smtp_password_enc: String,
    pub imap_host: String,
    pub imap_port: i32,
    pub imap_username: String,
    #[serde(skip)]
    pub imap_password_enc: String,
    pub daily_limit: i32,
    pub cost_per_email_micros: i64,
    pub status: String,
    pub last_error: String,
    pub imap_last_uid: i64,
    pub dns: Value,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct EmailMessage {
    pub id: Uuid,
    pub project_id: Uuid,
    pub lead_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub mailbox_id: Option<Uuid>,
    pub direction: String,
    pub kind: String,
    pub step: i32,
    pub subject: String,
    pub body: String,
    pub status: String,
    pub scheduled_for: Option<Ts>,
    pub sent_at: Option<Ts>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub from_email: Option<String>,
    pub to_email: Option<String>,
    pub classification: Option<String>,
    pub classification_reason: String,
    pub error: String,
    pub cost_micros: i64,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct LinkedinMessage {
    pub id: Uuid,
    pub lead_id: Uuid,
    pub project_id: Uuid,
    pub step: i32,
    pub kind: String,
    pub body: String,
    pub status: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Material {
    pub id: Uuid,
    pub project_id: Uuid,
    pub kind: String,
    pub title: String,
    pub content: String,
    pub url: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Suppression {
    pub id: Uuid,
    pub org_id: Uuid,
    pub value: String,
    pub kind: String,
    pub reason: String,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Import {
    pub id: Uuid,
    pub project_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub filename: String,
    pub total: i32,
    pub imported: i32,
    pub skipped: i32,
    pub errors: Value,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AgentRun {
    pub id: Uuid,
    pub project_id: Option<Uuid>,
    pub org_id: Option<Uuid>,
    pub kind: String,
    pub input: Value,
    pub status: String,
    pub step: i32,
    pub output: Value,
    pub error: String,
    pub cost_usd: f64,
    pub created_at: Ts,
    pub started_at: Option<Ts>,
    pub finished_at: Option<Ts>,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct AgentEvent {
    pub id: i64,
    pub run_id: Uuid,
    pub step: i32,
    pub kind: String,
    pub message: String,
    pub data: Value,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SupportMessage {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub role: String,
    pub content: String,
    pub created_at: Ts,
}
