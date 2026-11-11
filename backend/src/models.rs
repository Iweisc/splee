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
    /// The Threads account social posts go out from. Its token stays out of this struct.
    pub threads_connected: bool,
    pub threads_username: String,
    pub threads_error: Option<String>,
    pub threads_failed_at: Option<Ts>,
    /// The YouTube channel comments go out as, by its @handle or title. The OAuth client's secret and the refresh token stay
    /// out of this struct.
    pub youtube_connected: bool,
    pub youtube_channel: String,
    pub youtube_error: Option<String>,
    pub youtube_failed_at: Option<Ts>,
    /// The Stack Exchange user answers go out as, by display name, and the site's host. The token stays out of this struct.
    pub stackexchange_connected: bool,
    pub stackexchange_user: String,
    pub stackexchange_site: String,
    pub stackexchange_error: Option<String>,
    pub stackexchange_failed_at: Option<Ts>,
    /// The DEV account articles go out from. Its API key stays out of this struct.
    pub dev_connected: bool,
    pub dev_username: String,
    pub dev_error: Option<String>,
    pub dev_failed_at: Option<Ts>,
    /// The Lemmy account posts go out from, as name@instance, and its instance. Its password and JWT stay out of this struct.
    pub lemmy_connected: bool,
    pub lemmy_url: String,
    pub lemmy_handle: String,
    pub lemmy_error: Option<String>,
    pub lemmy_failed_at: Option<Ts>,
    /// The Nostr key notes go out as, by its npub, and the relays they go to. The secret key stays out of this struct.
    pub nostr_connected: bool,
    pub nostr_npub: String,
    pub nostr_relays: Vec<String>,
    pub nostr_error: Option<String>,
    pub nostr_failed_at: Option<Ts>,
    /// The Tumblr account, its blogs as [{name, title, url, uuid}], and the one that posts. Its tokens stay out of this struct.
    pub tumblr_connected: bool,
    pub tumblr_username: String,
    pub tumblr_blogs: Value,
    pub tumblr_blog: String,
    pub tumblr_error: Option<String>,
    pub tumblr_failed_at: Option<Ts>,
    /// The Hashnode account, its publications as [{id, title, url}], and the one articles go in. Its token stays out of this struct.
    pub hashnode_connected: bool,
    pub hashnode_username: String,
    pub hashnode_publications: Value,
    pub hashnode_publication_id: String,
    pub hashnode_error: Option<String>,
    pub hashnode_failed_at: Option<Ts>,
    /// The Discord bot posts go out as, by username, and its id, which its invite link needs. Its token stays out of this struct.
    pub discord_connected: bool,
    pub discord_bot_id: String,
    pub discord_bot: String,
    pub discord_error: Option<String>,
    pub discord_failed_at: Option<Ts>,
    /// The Telegram bot posts go out as, by username, and whether its privacy mode is off. Its token stays out of this struct.
    pub telegram_connected: bool,
    pub telegram_bot: String,
    pub telegram_reads_all: bool,
    pub telegram_error: Option<String>,
    pub telegram_failed_at: Option<Ts>,
    /// "neverbounce", "millionverifier", "hunter", "kickbox", "bouncer", "emailable", "clearout", "reoon", "debounce" or "" for none. Its key (verifier_key_enc) stays out of this struct.
    pub verifier: String,
    pub verifier_error: Option<String>,
    pub verifier_failed_at: Option<Ts>,
    /// "hunter", "findymail", "prospeo", "anymailfinder" or "" for none. Looks up the real address behind a guessed one. Its key
    /// (finder_key_enc) stays out of this struct.
    pub finder: String,
    pub finder_error: Option<String>,
    pub finder_failed_at: Option<Ts>,
    /// Keeps working campaigns stocked with written emails within the monthly budget and lead cap (see autopilot.rs).
    pub autopilot: bool,
    /// Claude spend a calendar month (UTC) may reach, counting runs started by hand.
    pub autopilot_budget_micros: i64,
    /// New leads emailed a month. Past it, first emails wait for the next month while autopilot is on.
    pub autopilot_lead_cap: i32,
    /// Autopilot's emails go out without approval.
    pub autopilot_auto_send: bool,
    /// Texts after the first email, as [{channel, delay_days, instructions}]. Sent through the org's Twilio account.
    pub text_steps: Value,
    /// Texts sent in any 24 hours. Replies count toward it but are never held back, like emails.
    pub daily_text_limit: i32,
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
    /// A/B test auto-pick: once every variant has this many sends and one is clearly ahead, the others are paused.
    pub variant_pick_after: Option<i32>,
    pub variant_winner_id: Option<Uuid>,
    pub variant_picked_at: Option<Ts>,
    /// Why auto-pick kept the winner, in words for the campaign page.
    pub variant_pick_note: String,
}

/// One angle of a campaign's A/B test. Its instructions go to the writer on top of the campaign brief.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Variant {
    pub id: Uuid,
    pub segment_id: Uuid,
    pub name: String,
    pub instructions: String,
    pub position: i32,
    /// New leads skip a paused variant. Leads that already have it keep it.
    pub paused_at: Option<Ts>,
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
    /// Its best lead's score, or its own while it has none. See score.rs.
    #[sqlx(default)]
    pub score: i16,
    #[sqlx(default)]
    pub score_fit: i16,
    #[sqlx(default)]
    pub score_intent: i16,
    #[sqlx(default)]
    pub score_reasons: sqlx::types::Json<Vec<crate::score::Reason>>,
    #[sqlx(default)]
    pub scored_at: Option<Ts>,
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
    /// In E.164, or "" for none. See phone.rs.
    pub phone: String,
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
    /// The campaign's A/B test variant, picked when its first email is written and kept from then on.
    pub variant_id: Option<Uuid>,
    /// How finding an address for a lead without one went. See waterfall.rs and migration 0103.
    pub email_search: String,
    pub email_source: String,
    pub email_search_note: String,
    pub email_search_lookups: i32,
    pub email_search_credits: Option<i32>,
    pub email_searched_at: Option<Ts>,
    /// 0-100, fit plus intent, from score.rs. Reasons are the strongest first.
    #[sqlx(default)]
    pub score: i16,
    #[sqlx(default)]
    pub score_fit: i16,
    #[sqlx(default)]
    pub score_intent: i16,
    #[sqlx(default)]
    pub score_reasons: sqlx::types::Json<Vec<crate::score::Reason>>,
    #[sqlx(default)]
    pub scored_at: Option<Ts>,
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

/// Past about 5% bounced, mailbox providers start sending a domain's mail to spam. Under 20 leads a single bounce crosses
/// that, so it waits for enough sends to mean something. Keep in sync with bouncing in frontend/src/lib/mailbox.ts.
pub fn bouncing(emailed: i64, bounced: i64) -> bool {
    emailed >= 20 && bounced * 20 > emailed
}

impl MailboxStats {
    pub fn bouncing(&self) -> bool {
        bouncing(self.emailed, self.bounced)
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
    /// Trade warmup emails with the other warming mailboxes in this Splee. See mail::warmup.
    pub warmup: bool,
    pub warmup_started_on: Option<chrono::NaiveDate>,
    #[serde(skip)]
    pub warmup_next_at: Option<Ts>,
    pub warmup_daily_cap: i32,
    /// Campaign plus warmup emails a day, while warmup is on.
    pub total_daily_limit: i32,
    /// No campaign emails until warmup has run 14 days.
    pub hold_until_warm: bool,
    #[serde(skip)]
    pub warmup_junk_uid: i64,
    /// "password", or "google" and "microsoft" for a mailbox connected by signing in. Its tokens are only read by
    /// mail::oauth::access_token.
    pub provider: String,
    /// The provider signed Splee out, so it waits for someone to sign in again.
    pub needs_reconnect: bool,
}

/// A mailbox's booking page at /book/{slug}. See crate::booking.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct BookingPage {
    pub id: Uuid,
    pub org_id: Uuid,
    pub mailbox_id: Uuid,
    pub slug: String,
    pub name: String,
    pub duration_minutes: i32,
    pub timezone: String,
    /// Monday first: None for a day off, else [start, end] in minutes after midnight.
    pub hours: sqlx::types::Json<Vec<Option<[i32; 2]>>>,
    pub buffer_minutes: i32,
    pub notice_minutes: i32,
    pub days_ahead: i32,
    pub location: String,
    #[serde(skip)]
    pub calendar_url_enc: String,
    pub calendar_error: String,
    pub created_at: Ts,
}

/// A call booked on a booking page. Its token_hash and ip columns are left out.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct Booking {
    pub id: Uuid,
    pub page_id: Uuid,
    pub starts_at: Ts,
    pub ends_at: Ts,
    pub name: String,
    pub email: String,
    pub note: String,
    pub timezone: String,
    pub sequence: i32,
    pub cancelled_at: Option<Ts>,
    pub created_at: Ts,
}

/// A domain bought through the org's registrar to send from. `mailboxes` and `warming` count the org's mailboxes on it
/// and those with warmup on. See crate::domains.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SendingDomain {
    pub id: Uuid,
    pub org_id: Uuid,
    pub domain: String,
    pub status: String,
    pub price_micros: i64,
    pub order_id: String,
    pub error: String,
    pub expires_on: Option<chrono::NaiveDate>,
    pub registered_at: Option<Ts>,
    pub provider: String,
    pub forward_to: String,
    pub dmarc_email: String,
    pub dns_status: String,
    pub records: Value,
    pub dns: Value,
    pub created_at: Ts,
    pub mailboxes: i64,
    pub warming: i64,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct RegistrarAccount {
    pub provider: String,
    pub account: String,
    #[serde(skip)]
    pub api_key_enc: String,
    #[serde(skip)]
    pub secret_enc: String,
    pub created_at: Ts,
}

/// GET /mailboxes/warmup: one of the org's mailboxes with warmup on, over the last 14 days. `inbox` and `spam` are
/// where its warmup emails landed, as the receiving mailboxes saw them.
#[derive(Debug, Serialize, FromRow)]
pub struct WarmupHealth {
    pub mailbox_id: Uuid,
    /// Day 1 is the day of its first warmup email. None before that.
    pub day: Option<i32>,
    pub sent_today: i64,
    #[sqlx(skip)]
    pub target_today: i32,
    #[serde(skip)]
    pub cap: i32,
    pub sent: i64,
    pub received: i64,
    pub inbox: i64,
    pub spam: i64,
    /// Whether another mailbox in this Splee is warming up to trade emails with. Never which one.
    pub has_peers: bool,
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
    /// The A/B test variant it was written for. Follow-ups keep the first email's.
    pub variant_id: Option<Uuid>,
    /// "email", or "sms" or "whatsapp" for a lead's answer to a text. Splee's own texts are in text_messages.
    pub channel: String,
}

#[derive(Debug, Clone, Serialize, FromRow)]
pub struct TextMessage {
    pub id: Uuid,
    pub project_id: Uuid,
    pub lead_id: Uuid,
    pub segment_id: Option<Uuid>,
    pub channel: String,
    /// "sequence" for a step of the project's text_steps, "reply" for one typed in the inbox.
    pub kind: String,
    pub step: i32,
    pub to_number: String,
    /// The text, or for a WhatsApp template its text with the lead's details filled in, as a preview.
    pub body: String,
    /// A WhatsApp template's Content SID and its variables, sent instead of the body.
    pub template_sid: String,
    pub template_vars: Value,
    pub status: String,
    pub due_at: Option<Ts>,
    pub sent_at: Option<Ts>,
    pub twilio_sid: Option<String>,
    pub error: String,
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

/// Campaigns being brought over from another outreach tool (outreach/). The key is never sent back.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct OutreachImport {
    pub id: Uuid,
    pub project_id: Uuid,
    pub tool: String,
    pub status: String,
    pub campaigns: Value,
    pub include_blocklist: bool,
    pub blocklist_added: i32,
    pub error: String,
    pub created_at: Ts,
    pub finished_at: Option<Ts>,
}

/// One email of a campaign brought over from another tool.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SegmentStep {
    pub position: i32,
    pub delay_days: i32,
    pub subject: String,
    pub body: String,
}

/// A reply or post for a social platform, drafted by a social run or written by the user, posted once approved.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SocialPost {
    pub id: Uuid,
    pub project_id: Uuid,
    pub run_id: Option<Uuid>,
    /// "x", "reddit", "bluesky", "mastodon", "linkedin", "threads", "youtube", "stackexchange", "dev", "discourse", "lemmy", "nostr",
    /// "tumblr", "hashnode", "discord" or "telegram".
    pub platform: String,
    /// The link of the post or comment it answers, None for an original post.
    pub in_reply_to: Option<String>,
    /// The text it answers, for the reviewer.
    pub context: String,
    /// Reddit: where an original post goes, or the thread's subreddit. Lemmy: the same for its community, as name@instance.
    pub subreddit: String,
    /// Reddit, Lemmy and Discourse original posts and DEV and Hashnode articles only.
    pub title: String,
    /// DEV articles, at most 4, Hashnode articles, at most 5, and Tumblr posts, at most 10.
    pub tags: Vec<String>,
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
    /// Discourse only: the forum it goes to. None once that forum is removed.
    pub forum_id: Option<Uuid>,
    /// Discord and Telegram only: the channel it goes to. None once the bot can't see it anymore.
    pub channel_id: Option<Uuid>,
}

/// A Discord text channel or a Telegram group or channel the project's bot can see. Picked ones are read and posted in.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct SocialChannel {
    pub id: Uuid,
    pub project_id: Uuid,
    /// "discord" or "telegram".
    pub platform: String,
    /// Discord's channel id, or Telegram's chat id.
    pub chat_id: String,
    /// The Discord server. Empty on Telegram.
    pub guild_id: String,
    pub guild_name: String,
    pub name: String,
    /// "text" or "announcement" on Discord, "group", "supergroup" or "channel" on Telegram.
    pub kind: String,
    /// A public Telegram chat's @username.
    pub username: String,
    pub can_read: bool,
    pub can_send: bool,
    pub picked: bool,
    #[serde(skip)]
    pub last_message_id: String,
    pub created_at: Ts,
}

impl SocialChannel {
    /// "#general in Acme" on Discord, the chat's title on Telegram.
    pub fn label(&self) -> String {
        if self.platform == "discord" { format!("#{} in {}", self.name, self.guild_name) } else { self.name.clone() }
    }
}

/// A Discourse forum the project joins. Its key stays out of the JSON.
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct DiscourseForum {
    pub id: Uuid,
    pub project_id: Uuid,
    /// Like https://forum.example.com, or https://example.com/forum for a forum under a path.
    pub url: String,
    pub username: String,
    /// "user" for a User API Key, "admin" for an API key an admin made, which acts as `username`.
    pub key_kind: String,
    #[serde(skip)]
    pub key_enc: String,
    /// {host and path}/u/{username}, lowercase, which the daily cap counts by.
    pub account: String,
    /// Where new topics go. None for replies only.
    pub category_id: Option<i64>,
    pub category_name: String,
    pub error: Option<String>,
    pub failed_at: Option<Ts>,
    /// Posting waits until then, after the forum answered 429 with a Retry-After.
    pub held_until: Option<Ts>,
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
