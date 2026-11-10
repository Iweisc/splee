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
    /// Signs the requests to every webhook endpoint.
    pub webhook_secret: String,
    /// Posts the last 7 days' numbers to the project's chats every Monday.
    pub chat_weekly_report: bool,
    /// Gets a copy of every email the project sends, like a CRM's BCC logging address. Empty for none.
    pub bcc_email: String,
    pub bcc_error: Option<String>,
    pub bcc_failed_at: Option<Ts>,
    pub paused: bool,
    /// Which of hooks::EVENTS the webhook gets.
    pub webhook_events: Vec<String>,
    /// The secret in this project's booking webhook URL, /api/bookings/{booking_token}.
    pub booking_token: String,
    /// The secret in this project's reminders calendar feed URL, /api/calendar/{calendar_token}.ics.
    pub calendar_token: String,
    /// The secret in this project's form webhook URL, /api/forms/{form_token}.
    pub form_token: String,
    /// The CRM tokens themselves ({crm}_token_enc) stay out of this struct, so they never reach the browser.
    pub hubspot_connected: bool,
    pub hubspot_error: Option<String>,
    pub hubspot_failed_at: Option<Ts>,
    pub pipedrive_connected: bool,
    pub pipedrive_error: Option<String>,
    pub pipedrive_failed_at: Option<Ts>,
    pub attio_connected: bool,
    pub attio_error: Option<String>,
    pub attio_failed_at: Option<Ts>,
    pub close_connected: bool,
    pub close_error: Option<String>,
    pub close_failed_at: Option<Ts>,
    pub folk_connected: bool,
    pub folk_error: Option<String>,
    pub folk_failed_at: Option<Ts>,
    pub capsule_connected: bool,
    pub capsule_error: Option<String>,
    pub capsule_failed_at: Option<Ts>,
    pub brevo_connected: bool,
    pub brevo_error: Option<String>,
    pub brevo_failed_at: Option<Ts>,
    pub zendesk_sell_connected: bool,
    pub zendesk_sell_error: Option<String>,
    pub zendesk_sell_failed_at: Option<Ts>,
    pub lacrm_connected: bool,
    pub lacrm_error: Option<String>,
    pub lacrm_failed_at: Option<Ts>,
    pub salesflare_connected: bool,
    pub salesflare_error: Option<String>,
    pub salesflare_failed_at: Option<Ts>,
    pub affinity_connected: bool,
    pub affinity_error: Option<String>,
    pub affinity_failed_at: Option<Ts>,
    pub freshsales_connected: bool,
    pub freshsales_error: Option<String>,
    pub freshsales_failed_at: Option<Ts>,
    pub copper_connected: bool,
    pub copper_error: Option<String>,
    pub copper_failed_at: Option<Ts>,
    pub insightly_connected: bool,
    pub insightly_error: Option<String>,
    pub insightly_failed_at: Option<Ts>,
    pub twenty_connected: bool,
    pub twenty_error: Option<String>,
    pub twenty_failed_at: Option<Ts>,
    pub twenty_self_connected: bool,
    pub twenty_self_error: Option<String>,
    pub twenty_self_failed_at: Option<Ts>,
    pub espocrm_connected: bool,
    pub espocrm_error: Option<String>,
    pub espocrm_failed_at: Option<Ts>,
    pub odoo_connected: bool,
    pub odoo_error: Option<String>,
    pub odoo_failed_at: Option<Ts>,
    pub notion_connected: bool,
    pub notion_error: Option<String>,
    pub notion_failed_at: Option<Ts>,
    pub airtable_connected: bool,
    pub airtable_error: Option<String>,
    pub airtable_failed_at: Option<Ts>,
    pub baserow_connected: bool,
    pub baserow_error: Option<String>,
    pub baserow_failed_at: Option<Ts>,
    pub nocodb_connected: bool,
    pub nocodb_error: Option<String>,
    pub nocodb_failed_at: Option<Ts>,
    pub grist_connected: bool,
    pub grist_error: Option<String>,
    pub grist_failed_at: Option<Ts>,
    pub teable_connected: bool,
    pub teable_error: Option<String>,
    pub teable_failed_at: Option<Ts>,
    /// Unipile sends due LinkedIn steps from this account. Its token (unipile_token_enc) stays out of this struct.
    pub unipile_connected: bool,
    pub unipile_account_name: String,
    pub unipile_error: Option<String>,
    pub unipile_failed_at: Option<Ts>,
    /// The X account social posts go out from. Its cookies (x_auth_token_enc, x_ct0_enc) stay out of this struct.
    pub x_connected: bool,
    pub x_handle: String,
    pub x_error: Option<String>,
    pub x_failed_at: Option<Ts>,
    /// The Reddit account social posts go out from. The app's secret and the password stay out of this struct.
    pub reddit_connected: bool,
    pub reddit_username: String,
    pub reddit_error: Option<String>,
    pub reddit_failed_at: Option<Ts>,
    /// The Bluesky account social posts go out from. Its app password stays out of this struct.
    pub bluesky_connected: bool,
    pub bluesky_handle: String,
    pub bluesky_error: Option<String>,
    pub bluesky_failed_at: Option<Ts>,
    /// The Mastodon account social posts go out from, as user@instance, and the instance's post limit. Its token stays out
    /// of this struct.
    pub mastodon_connected: bool,
    pub mastodon_url: String,
    pub mastodon_handle: String,
    pub mastodon_max_chars: i32,
    pub mastodon_error: Option<String>,
    pub mastodon_failed_at: Option<Ts>,
    /// "neverbounce", "millionverifier", "hunter", "kickbox", "bouncer", "emailable", "clearout", "reoon", "debounce" or "" for none. Its key (verifier_key_enc) stays out of this struct.
    pub verifier: String,
    pub verifier_error: Option<String>,
    pub verifier_failed_at: Option<Ts>,
    /// "hunter", "findymail", "prospeo", "anymailfinder" or "" for none. Looks up the real address behind a guessed one. Its key
    /// (finder_key_enc) stays out of this struct.
    pub finder: String,
    pub finder_error: Option<String>,
    pub finder_failed_at: Option<Ts>,
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
pub struct WebhookDelivery {
    pub id: Uuid,
    pub project_id: Uuid,
    pub event: String,
    pub payload: Value,
    /// None when the endpoint answered 2xx.
    pub error: Option<String>,
    /// The endpoint it went to.
    pub url: String,
    pub created_at: Ts,
}

/// A URL a project sends to: a chat for its alerts (chat_channels), like a Slack, Discord, Teams, Google Chat or Telegram
/// webhook, ntfy or Pushover, or an endpoint for its lead events (webhook_endpoints).
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Endpoint {
    pub id: Uuid,
    pub project_id: Uuid,
    pub url: String,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    pub created_at: Ts,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SavedReply {
    pub id: Uuid,
    pub project_id: Uuid,
    pub title: String,
    pub body: String,
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
    /// When the optimizer paused it. It never pauses a campaign twice, so resuming one sticks.
    pub auto_paused_at: Option<Ts>,
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
    pub notes: String,
    pub remind_at: Option<Ts>,
    /// When the call they booked starts, if their scheduling tool said.
    pub meeting_at: Option<Ts>,
    #[serde(skip)]
    pub unsubscribe_token: String,
    pub last_contacted_at: Option<Ts>,
    pub created_at: Ts,
}

/// How a mailbox's last 30 days went, by lead: emailed from it, and of the mail it received, bounces and replies.
#[derive(Debug, Serialize, FromRow)]
pub struct MailboxStats {
    pub mailbox_id: Uuid,
    pub email: String,
    pub emailed: i64,
    pub bounced: i64,
    pub replied: i64,
}

impl MailboxStats {
    /// Past about 5% bounced, mailbox providers start sending a domain's mail to spam. Under 20 leads a single bounce
    /// crosses that, so it waits for enough sends to mean something. Keep in sync with bouncing in frontend/src/lib/mailbox.ts.
    pub fn bouncing(&self) -> bool {
        self.emailed >= 20 && self.bounced * 20 > self.emailed
    }

    /// Past 10%, twice where `bouncing` warns, the mailbox is hurting its domain, so Splee stops sending from it.
    pub fn too_many_bounces(&self) -> bool {
        self.emailed >= 20 && self.bounced * 10 > self.emailed
    }

    /// Rounded, like the Mailboxes page shows it.
    pub fn bounce_percent(&self) -> i64 {
        let emailed = self.emailed.max(1);
        ((self.bounced * 100 + emailed / 2) / emailed).min(100)
    }
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Mailbox {
    pub id: Uuid,
    pub org_id: Uuid,
    pub email: String,
    pub from_name: String,
    /// The signature and booking link used instead of the project's in the emails Splee writes, when this mailbox sends
    /// them. See sender::as_mailbox.
    pub signature: String,
    pub booking_link: String,
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
    /// INBOX's UIDVALIDITY when imap_last_uid was taken. When the server changes it, the cursor starts over.
    pub imap_uid_validity: Option<i64>,
    pub dns: Value,
    /// Tried first by the DNS check's DKIM lookup, for a provider that signs with a selector of its own. Empty for none.
    pub dkim_selector: String,
    pub created_at: Ts,
    pub recheck_at: Option<Ts>,
    /// Start at a few emails a day and work up to daily_limit. See sender::TODAY_LIMIT.
    pub ramp_up: bool,
    pub first_sent_on: Option<chrono::NaiveDate>,
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
    /// Inbound only: when you marked the reply as needing no answer.
    pub handled_at: Option<Ts>,
    /// Times the receiving server answered "try again later".
    pub deferrals: i32,
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
    /// When the step should be sent: the connect note once it's written, each message a few days after the step before
    /// it was sent. None until then, and for steps that are done or skipped before coming due.
    pub due_at: Option<Ts>,
    pub done_at: Option<Ts>,
    /// "unipile" when Splee sent it, "" when the user did.
    pub sent_via: String,
    /// Why Splee couldn't send it, which leaves it for the user.
    pub send_error: Option<String>,
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

/// A reply or post for a social platform, drafted by a social run or written by the user, posted once approved.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SocialPost {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Option<Uuid>,
    /// "x", "reddit", "bluesky", "mastodon" or "linkedin".
    pub platform: String,
    /// The link of the post or comment it answers, None for an original post.
    pub in_reply_to: Option<String>,
    /// The text it answers, for the reviewer.
    pub context: String,
    /// Reddit only: where an original post goes, or the thread's subreddit.
    pub subreddit: String,
    /// Reddit original posts only.
    pub title: String,
    pub body: String,
    /// Why the agent picked the thread.
    pub reason: String,
    /// draft, approved, posted, failed or skipped.
    pub status: String,
    /// The handle or username it went out from, or the Unipile account id for LinkedIn.
    pub account: String,
    pub url: String,
    pub error: Option<String>,
    pub posted_at: Option<Ts>,
    pub created_at: Ts,
    pub updated_at: Ts,
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
