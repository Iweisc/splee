// Mirrors backend/src/models.rs and the response shapes in SPEC.md. Change them together.

import type { CampaignLanguage, LanguageCode, LanguageSource } from "./languages";

export type UUID = string;
export type Ts = string;

export interface User {
  id: UUID;
  email: string;
  name: string;
  created_at: Ts;
}

export type Role = "owner" | "admin" | "member";

export interface Organization {
  id: UUID;
  name: string;
  created_at: Ts;
}

export interface Me {
  user: User;
  org: Organization;
  role: Role;
  orgs: { id: UUID; name: string; role: Role }[];
  public_url: string;
  /** In the backend's OPERATOR_EMAILS, so the Companies page offers to load countries. */
  operator: boolean;
  /** The providers the server has an OAuth app for, so the Mailboxes page offers to connect by signing in. */
  mailbox_sign_in: SignInProvider[];
  /** The server has a Tumblr app, so the integrations page offers to connect Tumblr. */
  tumblr_sign_in: boolean;
  /** The server has a Meta app, so the integrations page offers to sign in with Facebook for Pages and Instagram. */
  meta_sign_in: boolean;
  /** The CRMs that sign in which the server has an app for, so the integrations page offers them. */
  crm_sign_in: SignInCrm[];
  /** Round robin skips you while it's on. */
  away: boolean;
  /** The owner filter each page was left on. */
  owner_filters: Partial<Record<OwnerView, OwnerFilter>>;
}

export type OwnerView = "inbox" | "leads" | "pipeline";
export type OwnerFilter = "everyone" | "mine" | "unassigned";

export type SignInProvider = "google" | "microsoft";

export interface Member {
  user_id: UUID;
  email: string;
  name: string;
  role: Role;
  away: boolean;
  /** Only sent to admins. */
  two_factor?: boolean;
  created_at: Ts;
}

/** What /auth/login and /auth/login/code answer. With two-factor on, a right password gets a token to send with the code. */
export type SignIn = Me | { two_factor_token: string };

/** The providers this Splee has an app for, from GET /auth/sso. */
export type SsoProvider = "google" | "microsoft";

/** A Google or Microsoft account the user signs in with. */
export interface Identity {
  id: UUID;
  provider: SsoProvider;
  /** The address the provider gave when it was connected. */
  email: string;
  created_at: Ts;
}

/** GET /auth/sign-in-methods. `password` is false for an account made by signing in with Google or Microsoft. */
export interface SignInMethods {
  password: boolean;
  identities: Identity[];
  providers: SsoProvider[];
}

/** A passkey the user signs in with, from GET /auth/passkeys. */
export interface Passkey {
  id: UUID;
  name: string;
  /** Adding it checked a PIN, fingerprint or face. */
  user_verified: boolean;
  created_at: Ts;
  last_used_at: Ts | null;
}

export interface TwoFactorStatus {
  enabled: boolean;
  backup_codes_left: number;
}

export interface TwoFactorSetup {
  /** Base32, for typing into the app instead of scanning. */
  secret: string;
  uri: string;
  /** The QR code as an SVG data URL. */
  qr: string;
}

export interface Invite {
  id: UUID;
  org_id: UUID;
  email: string;
  role: "admin" | "member";
  invited_by: UUID | null;
  accepted_at: Ts | null;
  created_at: Ts;
  expires_at: Ts;
}

export interface ApiKey {
  id: UUID;
  org_id: UUID;
  name: string;
  prefix: string;
  last_used_at: Ts | null;
  revoked_at: Ts | null;
  created_at: Ts;
}

/** An audit log entry (backend/src/audit.rs lists the actions). `actor_name` is empty when the member or key is gone. */
export interface AuditEvent {
  id: UUID;
  action: string;
  actor_user_id: UUID | null;
  actor_key_id: UUID | null;
  actor_name: string;
  target_type: string;
  target_id: UUID | null;
  target_label: string;
  details: Record<string, unknown>;
  ip: string | null;
  created_at: Ts;
}

/** An app someone allowed on the OAuth consent page, like ChatGPT or Claude.ai. */
export interface ConnectedApp {
  id: UUID;
  name: string;
  client_id: string;
  user_name: string;
  user_email: string;
  mine: boolean;
  created_at: Ts;
  last_used_at: Ts | null;
}

export interface NotificationPrefs {
  user_id: UUID;
  hot_lead_email: boolean;
  daily_digest: boolean;
  weekly_report: boolean;
}

/** A browser the member turned push notifications on in. The server keeps its endpoint and keys to itself. */
export interface PushDevice {
  id: UUID;
  /** Like "Chrome on macOS". */
  user_agent: string;
  created_at: Ts;
  last_success_at: Ts | null;
  /** Sends in a row that didn't go through. */
  failures: number;
}

/** The organization's Slack workspace and the channel each project posts to. `available` is false without a Slack app on the server. */
export interface SlackStatus {
  available: boolean;
  workspace: { team_id: string; team_name: string; installed_by: string | null; created_at: Ts } | null;
  projects: SlackProjectChannel[];
}

export interface SlackProjectChannel {
  project_id: UUID;
  project_name: string;
  channel_id: string | null;
  channel_name: string | null;
  /** Why the last post to the channel failed, cleared by the next one that works. */
  error: string | null;
  failed_at: Ts | null;
}

/** Public channels, and the private ones Splee was invited to. */
export interface SlackChannel {
  id: string;
  name: string;
  private: boolean;
}

export interface CompanyProfile {
  name?: string;
  description?: string;
  employees?: string;
  city?: string;
  country_code?: string;
  industry?: string;
  products?: string[];
  value_props?: string[];
  language?: string;
}

export type ProjectStatus = "researching" | "ready" | "active" | "paused" | "failed";

export interface Project {
  id: UUID;
  org_id: UUID | null;
  domain: string | null;
  business_description: string | null;
  name: string;
  status: ProjectStatus;
  company: CompanyProfile;
  created_by: UUID | null;
  created_at: Ts;
}

export interface FollowUp {
  delay_days: number;
  enabled: boolean;
  instructions: string;
  /** A call step opens a call for the lead's owner instead of sending an email. Missing means email. */
  kind?: "email" | "call";
}

export interface Targeting {
  industries?: string[];
  roles?: string[];
  company_sizes?: string[];
  countries?: string[];
  exclude_countries?: string[];
  keywords?: string[];
  exclude_keywords?: string[];
  /** Project targeting only: job titles that make a hiring signal. Unset means the campaigns' roles, empty turns it off. */
  hiring_roles?: string[];
}

/** Every CRM, in the order the integrations page lists them. Keep in sync with Crm::ALL in backend/src/crm/mod.rs. */
export const CRM_NAMES = ["hubspot", "salesforce", "zoho", "dynamics", "pipedrive", "attio", "close", "folk", "capsule", "brevo", "zendesk_sell", "lacrm", "salesflare", "affinity", "freshsales", "copper", "insightly", "twenty", "twenty_self", "espocrm", "odoo", "notion", "airtable", "baserow", "nocodb", "grist", "teable"] as const;
export type CrmName = (typeof CRM_NAMES)[number];

/** The CRMs that connect by signing in. Keep in sync with Crm::signs_in in backend/src/crm/mod.rs. */
export const SIGN_IN_CRMS = ["salesforce", "zoho", "dynamics"] as const;
export type SignInCrm = (typeof SIGN_IN_CRMS)[number];
export const signsIn = (crm: CrmName): crm is SignInCrm => (SIGN_IN_CRMS as readonly string[]).includes(crm);

/** Each CRM's connection and last failed sync, as {crm}_connected, {crm}_error and {crm}_failed_at. Its token is write-only,
 * through PUT /integrations/{crm}. */
type CrmStatus = { [C in CrmName as `${C}_connected`]: boolean } & { [C in CrmName as `${C}_error` | `${C}_failed_at`]: string | null };

export interface ProjectSettings extends CrmStatus {
  project_id: UUID;
  daily_email_limit: number;
  timezone: string;
  send_days: number[]; // 1 = Monday ... 7 = Sunday
  send_start_hour: number;
  send_end_hour: number;
  /** Dates like "2026-12-25" when no first emails, follow-ups, texts or LinkedIn steps go out, in the send window's time zone. */
  days_off: string[];
  follow_ups: FollowUp[];
  booking_link: string;
  auto_reply_mode: "off" | "approve" | "auto";
  auto_reply_instructions: string;
  sender_name: string;
  signature: string;
  tone: string;
  email_language: string;
  email_instructions: string;
  cta: string;
  max_words: number;
  channels: { email: boolean; linkedin: boolean };
  targeting: Targeting;
  /** Signs the requests to every webhook endpoint. */
  webhook_secret: string;
  /** Posts the last 7 days' numbers to the project's chats every Monday. */
  chat_weekly_report: boolean;
  /** Writes a brief before each booked call and posts it. */
  meeting_briefs: boolean;
  nudge_quiet_deals: boolean;
  deal_health: boolean;
  qualify_deals: boolean;
  pipeline_forecast: boolean;
  coach_calls: boolean;
  prep_calls: boolean;
  win_back_lost_deals: boolean;
  /** Writes a battlecard for each competitor a deal was won or lost against. See backend/src/agent/battlecards.rs. */
  write_battlecards: boolean;
  /** Drafts an email to a customer whose company shows room to buy more. See backend/src/agent/expansions.rs. */
  grow_accounts: boolean;
  /** Asks happy customers for introductions. See backend/src/agent/referrals.rs. */
  ask_for_referrals: boolean;
  watch_churn: boolean;
  prep_renewals: boolean;
  /** Gets a copy of every email the project sends, like a CRM's BCC address. Empty for none. */
  bcc_email: string;
  bcc_error: string | null;
  bcc_failed_at: string | null;
  paused: boolean;
  webhook_events: WebhookEvent[];
  /** The secret in the booking webhook URL, /api/bookings/{booking_token}. */
  booking_token: string;
  /** The secret in the reminders calendar feed URL, /api/calendar/{calendar_token}.ics. */
  calendar_token: string;
  /** The secret in the form webhook URL, /api/forms/{form_token}. */
  form_token: string;
  /** Its key is write-only, through PUT /integrations/verifier. */
  verifier: "" | "neverbounce" | "millionverifier" | "hunter" | "kickbox" | "bouncer" | "emailable" | "clearout" | "reoon" | "debounce" | "emaillistverify" | "bouncify" | "quickemailverification" | "abstract" | "mailboxvalidator" | "myemailverifier";
  verifier_error: string | null;
  verifier_failed_at: string | null;
  /** Unipile sends due LinkedIn steps from this account. Its token is write-only. */
  unipile_connected: boolean;
  unipile_account_name: string;
  unipile_error: string | null;
  unipile_failed_at: string | null;
  /** The X account social posts go out from. Its cookies are write-only, through PUT /integrations/x. */
  x_connected: boolean;
  x_handle: string;
  x_error: string | null;
  x_failed_at: string | null;
  /** The Reddit account social posts go out from. Its secret and password are write-only, through PUT /integrations/reddit. */
  reddit_connected: boolean;
  reddit_username: string;
  reddit_error: string | null;
  reddit_failed_at: string | null;
  /** The Bluesky account social posts go out from. Its app password is write-only, through PUT /integrations/bluesky. */
  bluesky_connected: boolean;
  bluesky_handle: string;
  bluesky_error: string | null;
  bluesky_failed_at: string | null;
  /** The Mastodon account social posts go out from, as user@instance. Its token is write-only, through PUT /integrations/mastodon. */
  mastodon_connected: boolean;
  mastodon_url: string;
  mastodon_handle: string;
  /** The instance's post limit, read on connect. */
  mastodon_max_chars: number;
  mastodon_error: string | null;
  mastodon_failed_at: string | null;
  /** The Threads account social posts go out from. Its token is write-only, through PUT /integrations/threads. */
  threads_connected: boolean;
  threads_username: string;
  threads_error: string | null;
  threads_failed_at: string | null;
  /** The YouTube channel comments go out as, by its @handle or title. The OAuth secret and refresh token are write-only, through PUT /integrations/youtube. */
  youtube_connected: boolean;
  youtube_channel: string;
  youtube_error: string | null;
  youtube_failed_at: string | null;
  /** The Stack Exchange user answers go out as, and the site's host. The access token is write-only, through PUT /integrations/stackexchange. */
  stackexchange_connected: boolean;
  stackexchange_user: string;
  stackexchange_site: string;
  stackexchange_error: string | null;
  stackexchange_failed_at: string | null;
  /** The DEV account articles go out from. Its API key is write-only, through PUT /integrations/dev. */
  dev_connected: boolean;
  dev_username: string;
  dev_error: string | null;
  dev_failed_at: string | null;
  /** The Lemmy account posts go out from, as name@instance. The password is write-only, through PUT /integrations/lemmy. */
  lemmy_connected: boolean;
  lemmy_url: string;
  lemmy_handle: string;
  lemmy_error: string | null;
  lemmy_failed_at: string | null;
  /** The Nostr key notes go out as, by its npub, and the relays they go to. The secret key is write-only, through PUT /integrations/nostr. */
  nostr_connected: boolean;
  nostr_npub: string;
  nostr_relays: string[];
  nostr_error: string | null;
  nostr_failed_at: string | null;
  /** Salesforce, Zoho CRM and Dynamics 365 Sales connect by signing in. Each has the host it's signed in to, and
   * needs_reconnect once it signed Splee out. */
  salesforce_site: string;
  salesforce_sandbox: boolean;
  salesforce_needs_reconnect: boolean;
  /** The org's active Lead Status values, and the one booked leads get. Empty leaves the status alone. */
  salesforce_statuses: { value: string; label: string }[];
  salesforce_booked_status: string;
  zoho_site: string;
  zoho_needs_reconnect: boolean;
  dynamics_site: string;
  dynamics_needs_reconnect: boolean;
  /** The Tumblr account, connected by signing in, its blogs and the one that posts. */
  tumblr_connected: boolean;
  tumblr_username: string;
  tumblr_blogs: TumblrBlog[];
  tumblr_blog: string;
  tumblr_error: string | null;
  tumblr_failed_at: string | null;
  /** The Hashnode account, its publications and the one articles go in. The token is write-only, through PUT /integrations/hashnode. */
  hashnode_connected: boolean;
  hashnode_username: string;
  hashnode_publications: HashnodePublication[];
  hashnode_publication_id: string;
  hashnode_error: string | null;
  hashnode_failed_at: string | null;
  /** The Discord bot posts go out as, by username, and its id, which its invite link needs. The token is write-only, through PUT /integrations/discord. */
  discord_connected: boolean;
  discord_bot_id: string;
  discord_bot: string;
  discord_error: string | null;
  discord_failed_at: string | null;
  /** The Telegram bot posts go out as, by username, and whether its group privacy mode is off. The token is write-only, through PUT /integrations/telegram. */
  telegram_connected: boolean;
  telegram_bot: string;
  telegram_reads_all: boolean;
  telegram_error: string | null;
  telegram_failed_at: string | null;
  /** Who signed in with Facebook for the Pages and Instagram accounts. meta_reconnect is when Facebook signed Splee out and they
   *  have to sign in again. The Pages' tokens never leave the backend. */
  meta_connected: boolean;
  meta_reconnect: boolean;
  meta_name: string;
  facebook_error: string | null;
  facebook_failed_at: string | null;
  instagram_error: string | null;
  instagram_failed_at: string | null;
  /** Keeps working campaigns stocked with written emails within the monthly budget and lead cap. */
  autopilot: boolean;
  /** Claude spend a calendar month (UTC) may reach, counting runs started by hand. */
  autopilot_budget_micros: number;
  /** New leads emailed a month. Past it, first emails wait for the next month while autopilot is on. */
  autopilot_lead_cap: number;
  /** Autopilot's emails go out without approval. */
  autopilot_auto_send: boolean;
  /** Drafts nobody reviewed or edited go out this many hours after they were written, 1 to 72. null keeps them waiting. */
  review_window_hours: number | null;
  /** The most the project may spend on emails in a calendar month (UTC), $1 to $1,000,000. null is no cap. */
  monthly_cap_micros: number | null;
  /** A first email to a lead whose mail goes to Google or Microsoft goes out from a mailbox on the same provider when one has room. */
  match_provider: boolean;
  /** Send days and hours are counted in each lead's time zone, from their phone number or country, else in `timezone`. */
  lead_time_zone: boolean;
  /** Once a lead replies, the sequence stops for the project's other leads at the same company (same company or website). */
  stop_company_on_reply: boolean;
  /** The mailboxes that send its first emails, or null for all of the organization's. Threads keep their mailbox. */
  mailbox_ids: string[] | null;
  /** The most first emails a day to one company (company row or website), or null for no limit. Follow-ups never wait. */
  company_daily_limit: number | null;
  /** Texts after the first email, through the organization's Twilio account. */
  text_steps: TextStep[];
  /** Texts sent in any 24 hours. Replies count toward it but are never held back. */
  daily_text_limit: number;
  /** Who gets a lead that has no owner when it first replies, books or is captured by the chat. */
  assign_rule: AssignRule;
  /** Round robin's members, in the order it goes through them. */
  assign_members: UUID[];
  /** Who covers which countries, for the territory rule. A lead from anywhere else takes its turn among assign_members. */
  assign_territories: Territory[];
}

export type AssignRule = "off" | "round_robin" | "mailbox" | "territory";

/** A member and the countries whose leads they get, as ISO 3166-1 alpha-2 codes. */
export interface Territory {
  owner: UUID;
  countries: string[];
}

/** GET /projects/:id/autopilot. Spend and leads count the current calendar month (UTC). */
/** GET /projects/:id/monthly-cap: the cap, what the month's sent emails cost, and whether cold emails wait for `resumes_at`. */
export interface MonthlyCap {
  project_id: UUID;
  monthly_cap_micros: number | null;
  spent_this_month_micros: number;
  reached: boolean;
  resumes_at: string | null;
}

export interface AutopilotStatus {
  enabled: boolean;
  auto_send: boolean;
  budget_micros: number;
  spent_micros: number;
  lead_cap: number;
  contacted: number;
  /** First emails in sending campaigns that wait for approval. */
  awaiting_approval: number;
  /** Why it's waiting, from its last check. Empty right after it started a run. */
  note: string;
  checked_at: Ts | null;
  /** Its run going now, else its last one. */
  run: {
    id: UUID;
    kind: RunKind;
    status: AgentRun["status"];
    campaign: string | null;
    created_at: Ts;
    finished_at: Ts | null;
  } | null;
}

export type WebhookEvent =
  | "hot_lead"
  | "lead_replied"
  | "meeting_booked"
  | "meeting_rescheduled"
  | "meeting_cancelled"
  | "lead_unsubscribed"
  | "lead_bounced"
  | "email_sent"
  | "email.opened"
  | "email.clicked"
  | "follow_up_due"
  | "form_submitted"
  | "status_changed"
  | "deal.created"
  | "deal.stage_changed"
  | "deal.won"
  | "deal.lost"
  | "lead_erased";

export interface ProjectSummary extends Project {
  sent: number;
  replies: number;
  hot: number;
  active_segments: number;
  paused: boolean;
}

export interface ProjectDetail {
  project: Project;
  settings: ProjectSettings;
  latest_run: AgentRun | null;
}

export interface ProjectStats {
  totals: {
    leads: number;
    drafts: number;
    scheduled: number;
    sent: number;
    contacted: number; // leads with at least one email sent
    replies: number;
    hot: number;
    meetings: number;
    ai_cost_micros: number; // what Claude reports for the agent's runs
    email_cost_micros: number; // sent emails times each mailbox's cost_per_email_micros
    spend_micros: number; // ai + email
    cost_per_hot_micros: number | null;
  };
  daily: { date: string; sent: number; replies: number; hot: number }[];
  segments: SegmentWithStats[];
}

/** GET /projects/:id/analytics. Rates are null when nobody was emailed. Money is in micro-dollars, deal values in cents. */
export interface AnalyticsTotals extends Engagement {
  sent: number; // first emails and follow-ups
  contacted: number; // leads emailed
  delivered: number;
  replies: number;
  reply_rate: number | null; // replies / contacted
  hot: number;
  hot_rate: number | null;
  meetings: number;
  deals_won: number;
  won_value: { currency: string; value_cents: number }[]; // the organization's currency first
  unsubscribes: number;
  bounced: number;
  bounce_rate: number | null;
  claude_cost_micros: number; // 0 with a mailbox filter, since runs don't belong to a mailbox
  email_cost_micros: number;
  spend_micros: number;
  cost_per_hot_micros: number | null;
  cost_per_meeting_micros: number | null;
  /** Payments less refunds, in cents of the organization's currency. 0 with a mailbox filter. */
  revenue_cents: number;
  refunded_cents: number;
  payments: number;
  paying_leads: number;
  /** Revenue over meetings, null without meetings. */
  revenue_per_meeting_cents: number | null;
  /** Revenue over spend, null when nothing was spent or the organization's currency isn't USD. */
  return_on_spend: number | null;
}

export interface AnalyticsPoint {
  date: string; // the day, or the Monday that starts the week
  sent: number;
  replies: number;
  hot: number;
  meetings: number;
}

/**
 * Emails opened and clicked, out of the emails sent in the range that tracked it, scanners left out. A rate is null when
 * none did. Opens are an estimate: Apple Mail opens every email itself and some people block the pixel.
 */
export interface Engagement {
  opened: number;
  open_rate: number | null;
  clicked: number;
  click_rate: number | null;
}

export interface Analytics {
  from: string;
  to: string;
  previous_from: string;
  previous_to: string;
  timezone: string;
  /** The organization's currency, which revenue is in. */
  currency: string;
  /** A payment provider is connected, or was, so the page shows revenue. */
  revenue_tracked: boolean;
  /** Payments in the range in other currencies, which revenue leaves out. */
  ignored_payments: number;
  ignored_currencies: string[];
  interval: "day" | "week";
  totals: AnalyticsTotals;
  previous: AnalyticsTotals;
  series: AnalyticsPoint[];
  campaigns: ({
    segment_id: UUID | null;
    name: string;
    sent: number;
    contacted: number;
    replies: number;
    reply_rate: number | null;
    hot: number;
    meetings: number;
    deals_won: number;
    bounced: number;
    bounce_rate: number | null;
    spend_micros: number;
    cost_per_hot_micros: number | null;
    revenue_cents: number;
    return_on_spend: number | null;
  } & Engagement)[];
  /** The A/B test angles of the campaigns. Left out of shared reports. */
  angles?: ({
    variant_id: UUID;
    segment_id: UUID;
    name: string;
    campaign: string;
    sent: number;
    contacted: number;
    replies: number;
    reply_rate: number | null;
  } & Engagement)[];
  mailboxes: ({
    mailbox_id: UUID | null;
    email: string;
    sent: number;
    contacted: number;
    replies: number;
    reply_rate: number | null;
    hot: number;
    bounced: number;
    bounce_rate: number | null;
    bouncing: boolean;
  } & Engagement)[];
  /** Step 0 is the first email. The rate is out of the step's emails. */
  steps: ({ step: number; sent: number; replies: number; reply_rate: number | null } & Engagement)[];
  /** weekday 1 (Monday) to 7, hour 0 to 23, in the project's time zone. Empty cells are left out. */
  heatmap: { weekday: number; hour: number; sent: number; replies: number }[];
  /** In each lead's own time zone. */
  call_heatmap: { weekday: number; hour: number; calls: number; connected: number }[];
  /** null leads or replies are ones Splee doesn't record for the channel. */
  channels: {
    channel: "email" | "linkedin" | "sms" | "whatsapp" | "call" | "social";
    /** For calls, the calls made. */
    sent: number;
    leads: number | null;
    replies: number | null;
    reply_rate: number | null;
    /** Calls only: the share where someone picked up and talked, and the meetings booked on a call. */
    connect_rate: number | null;
    meetings: number | null;
  }[];
  /** By the language of the emails leads got, most leads first. The rate is by lead. */
  languages: { language: LanguageCode; name: string; sent: number; contacted: number; replies: number; reply_rate: number | null }[];
}

export type ReportRange = "last_7" | "last_30" | "last_90" | "month_to_date";

/** A client report link (GET /projects/{id}/report-links). Its token was only shown when it was made. */
export interface ReportLink {
  id: UUID;
  project_id: UUID;
  title: string;
  intro: string;
  brand: "project" | "org";
  logo_url: string;
  range_preset: ReportRange;
  show_spend: boolean;
  show_mailboxes: boolean;
  show_leads: boolean;
  email_schedule: "none" | "weekly" | "monthly";
  recipients: { email: string; unsubscribed: boolean }[];
  expires_at: Ts | null;
  last_viewed_at: Ts | null;
  created_at: Ts;
}

/** A lead on a shared report: first name and company only. */
export interface ReportPerson {
  first_name: string;
  company: string;
  at: Ts;
  meeting_at: Ts | null;
  excerpt: string;
}

/**
 * A shared report (GET /shared-reports/{token}). The spend fields of `analytics` are missing unless `show_spend`, and its
 * mailboxes unless `show_mailboxes`. `meetings` and `hot_replies` are null unless `show_leads`.
 */
export interface SharedReport {
  name: string;
  logo_url: string;
  title: string;
  intro: string;
  range_preset: ReportRange;
  show_spend: boolean;
  show_mailboxes: boolean;
  show_leads: boolean;
  analytics: Analytics;
  meetings: ReportPerson[] | null;
  hot_replies: ReportPerson[] | null;
}

export interface WebhookDelivery {
  id: UUID;
  project_id: UUID;
  event: WebhookEvent;
  /** The JSON body that was posted. */
  payload: { lead?: { full_name?: string } } & Record<string, unknown>;
  /** Null when the endpoint answered 2xx. */
  error: string | null;
  /** The endpoint it went to. */
  url: string;
  created_at: Ts;
}

/** A URL the project sends to: a chat for its alerts (GET /projects/{id}/chat-channels) or an endpoint for its lead
 *  events (GET /projects/{id}/webhook-endpoints). */
export interface Endpoint {
  id: UUID;
  project_id: UUID;
  url: string;
  error: string | null;
  failed_at: Ts | null;
  created_at: Ts;
}

/** What a REST hook subscription can be for. Keep in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs. */
export type HookEvent = "hot_lead" | "lead_replied" | "meeting_booked" | "status_changed" | "deal.won" | "lead_added" | "lead_erased";

/** A Zapier, Make or n8n trigger listening for one lead event (GET /projects/{id}/hook-subscriptions), made with an API key. */
export interface HookSubscription {
  id: UUID;
  /** Null for every project of the organization. */
  project_id: UUID | null;
  event: HookEvent;
  target_url: string;
  key_name: string;
  error: string | null;
  failed_at: Ts | null;
  created_at: Ts;
}

/** A Discourse forum the project joins, as one user. Its key is write-only, through POST /projects/:id/discourse-forums. */
export interface DiscourseForum {
  id: UUID;
  project_id: UUID;
  /** Like https://forum.example.com, or https://example.com/forum for a forum under a path. */
  url: string;
  username: string;
  /** "user" for a User API Key, "admin" for an API key an admin made for the user. */
  key_kind: "user" | "admin";
  account: string;
  /** Where new topics go, null for replies only. */
  category_id: number | null;
  category_name: string;
  error: string | null;
  failed_at: Ts | null;
  /** Posting waits until then, after the forum asked Splee to slow down. */
  held_until: Ts | null;
  created_at: Ts;
}

/** One of the company's own blogs, which approved articles go out on. Its secret is write-only, through POST /projects/:id/blogs. */
export interface Blog {
  id: UUID;
  project_id: UUID;
  kind: "wordpress" | "ghost" | "webflow";
  /** The WordPress site, Ghost's admin address, or the Webflow site's public address. */
  url: string;
  /** The site's title. */
  name: string;
  /** WordPress only: who the application password belongs to. */
  username: string;
  account: string;
  /** Webflow only: the site, the CMS collection articles go in, and the collection's field for each part. */
  site_id: string;
  collection_id: string;
  collection_name: string;
  collection_slug: string;
  title_field: string;
  body_field: string;
  tags_field: string;
  excerpt_field: string;
  /** False saves articles as drafts on the blog. */
  publish: boolean;
  weekly_cap: number;
  /** The blog turned the credentials down, and nothing goes out until it's connected again. */
  paused: boolean;
  error: string | null;
  failed_at: Ts | null;
  created_at: Ts;
}

/** What POST /projects/:id/blogs/test read. Webflow lists the token's sites with their collections and fields instead. */
export interface BlogTest {
  name: string;
  user: string;
  url: string;
  sites: {
    id: string;
    name: string;
    url: string;
    collections: { id: string; name: string; slug: string; fields: { slug: string; name: string; type: string; required: boolean }[] }[];
  }[];
}

export interface SavedReply {
  id: UUID;
  project_id: UUID;
  title: string;
  body: string;
  created_at: Ts;
}

export interface Competitor {
  id: UUID;
  project_id: UUID;
  domain: string;
  name: string;
  note: string;
  created_at: Ts;
}

export type SegmentStatus = "draft" | "scaling" | "working" | "paused";

export interface Segment {
  id: UUID;
  project_id: UUID;
  name: string;
  description: string;
  icon: string;
  est_size: number;
  fit_score: number;
  status: SegmentStatus;
  targeting: Targeting;
  pain_points: string;
  angle: string;
  position: number;
  created_at: Ts;
  /** When the optimizer paused it. It never pauses a campaign twice, so resuming one sticks. */
  auto_paused_at: Ts | null;
  /** A/B test auto-pick: sends per variant before it keeps a clear winner. Null when it's off. */
  variant_pick_after: number | null;
  variant_winner_id: UUID | null;
  variant_picked_at: Ts | null;
  variant_pick_note: string;
  /** Whether its emails' links go through Splee to count clicks. On for new campaigns. */
  track_clicks: boolean;
  /** Whether its emails carry a pixel to count opens. Off for new campaigns, since it can hurt inbox placement. */
  track_opens: boolean;
  /** A personal page for each lead, written with the first email and linked from it. */
  lead_pages: boolean;
  /** What it writes in: English by default, one fixed language, or "lead" for each lead's own. */
  language: CampaignLanguage;
  /** When it was archived. An archived campaign is paused, and lists that add leads to a campaign leave it out. */
  archived_at: Ts | null;
  /** Its own cap on first emails and follow-ups a day, on top of the project's limit. Null leaves it to the project's. */
  daily_limit: number | null;
}

/** One angle of a campaign's A/B test. Its instructions go to the writer on top of the campaign brief. */
export interface Variant {
  id: UUID;
  segment_id: UUID;
  name: string;
  instructions: string;
  position: number;
  /** New leads skip a paused variant. Leads that already have it keep it. */
  paused_at: Ts | null;
  created_at: Ts;
}

/** Counted by lead: leads whose first email of this variant went out, and of those who replied, turned hot, bounced or unsubscribed. */
export interface VariantStats extends Variant {
  sent: number;
  replies: number;
  hot: number;
  bounced: number;
  unsubscribed: number;
}

/** GET /segments/{id}/variants. Variants are compared on hot rate, or reply rate while none has a hot lead. */
export interface AbTest {
  variants: VariantStats[];
  metric: "hot" | "reply";
  verdict: "too_few" | "unclear" | "winner";
  leader_id: UUID | null;
  /** The verdict in plain words. */
  note: string;
  pick_after: number | null;
  winner_id: UUID | null;
  picked_at: Ts | null;
  pick_note: string;
}

/** A first email's two texts, both kept whatever the mode. See backend my_copy.rs. */
export interface FirstEmail {
  mode: "instructions" | "my_copy";
  subject: string;
  body: string;
}

/** GET /segments/{id}/my-copy. With source "project", mode, subject and body are the project's. */
export interface CampaignCopy extends FirstEmail {
  campaign_id: UUID;
  project_id: UUID;
  source: "project" | "campaign";
  /** The campaign's angle, which the AI email is written from. */
  instructions: string;
  results: {
    my_copy: { sent: number; replies: number };
    ai: { sent: number; replies: number };
    note: string;
  };
}

/** GET /projects/{id}/my-copy. */
export interface ProjectCopy extends FirstEmail {
  project_id: UUID;
  instructions: string;
  campaign_ids: UUID[];
}

/** POST /segments/{id}/my-copy/preview: the text as each of the first 3 leads gets it. */
export interface CopyPreview {
  mode: FirstEmail["mode"];
  emails: {
    lead_id: UUID;
    lead_name: string;
    lead_company: string;
    /** "ai" for a lead that gets the AI email instead, with why in note. */
    gets: "my_copy" | "ai";
    subject: string;
    body: string;
    sender_name: string;
    note: string | null;
  }[];
}

/** Emails sent and the leads whose first reply came after one of them. See RepliesSection. */
export interface ReplyCounts {
  sent: number;
  replied: number;
}

/** One email of the sequence: step 0 is the first email, step n is follow-up n. */
export interface StepStats extends ReplyCounts {
  step: number;
}

/** One day of the week in the project's time zone, 1 for Monday to 7 for Sunday. */
export interface WeekdayStats extends ReplyCounts {
  weekday: number;
}

/** One hour of the day in the project's time zone, 0 to 23. */
export interface HourStats extends ReplyCounts {
  hour: number;
}

export interface SegmentWithStats extends Segment {
  companies: number;
  leads: number;
  sent: number;
  contacted: number;
  replies: number;
  /** Leads whose address bounced. */
  bounced: number;
  hot: number;
  spend_micros: number;
  cost_per_hot_micros: number | null;
}

/** The first thing holding a campaign, in the order a person would clear them. See backend/src/send_state.rs. */
export type SendStateName =
  | "archived"
  | "not_started"
  | "paused"
  | "project_paused"
  | "no_mailbox"
  | "awaiting_approval"
  | "nothing_queued"
  | "scheduled"
  | "monthly_cap_reached"
  | "lead_cap_reached"
  | "daily_limit_reached"
  | "campaign_limit_reached"
  | "mailboxes_full"
  | "outside_hours"
  | "sending";

/** GET /segments/:id/send-state: whether the campaign sends now and, if not, what holds it. */
export interface SendState {
  campaign_id: string;
  state: SendStateName;
  sending_now: boolean;
  message: string;
  today: {
    sent: number;
    queued: number;
    drafts: number;
    project_sent: number;
    daily_email_limit: number;
    /** The campaign's own cap on `sent`, or null when only the project's limit applies. */
    campaign_limit: number | null;
    /** When its next email can go out, or null while it sends now or nothing is queued. */
    next_send_at: string | null;
  };
}

/** Why a lead or company scores what it does, the strongest first. "against" is what holds it down. */
export interface ScoreReason {
  text: string;
  kind: "fit" | "intent" | "against";
}

/** A 0-100 score: fit (0-50, how well the company matches the targeting) plus intent (0-50, what it did lately). */
export interface Scored {
  score: number;
  score_fit: number;
  score_intent: number;
  score_reasons: ScoreReason[];
  /** Null until the score worker first reaches it, a few minutes after it's added. */
  scored_at: Ts | null;
}

/** One input to the score, which the project can turn off. */
export interface ScoreSignal {
  key: string;
  label: string;
  part: "fit" | "intent";
  on: boolean;
}

export interface Company extends Scored {
  id: UUID;
  project_id: UUID;
  segment_id: UUID | null;
  domain: string;
  name: string;
  description: string;
  city: string;
  country_code: string;
  employees: string;
  fit_score: number;
  fit_reason: string;
  source_url: string;
  status: "new" | "people_found" | "no_people" | "excluded";
  created_at: Ts;
}

/** A business from the shared company database. `category` is Overture's snake_case, like dental_clinic. */
export interface DirectoryCompany {
  domain: string;
  name: string;
  website: string;
  phone: string | null;
  email: string | null;
  address: string | null;
  city: string | null;
  region: string | null;
  postcode: string | null;
  country_code: string;
  category: string | null;
  /** The tools its tech scan found. Only companies some project has get scanned. */
  tech: string[];
}

/** A customer the lookalikes are matched to: a won deal's company, or one added by hand. */
export interface LookalikeSeed {
  domain: string;
  name: string;
  /** From a won deal. */
  won: boolean;
  in_database: boolean;
  /** Splee has a company database row, a company in the project or a tech scan for it. */
  known: boolean;
}

export interface LookalikeTrait {
  value: string;
  label: string;
  /** How many of the seeds have it. */
  seeds: number;
}

export interface LookalikePart {
  key: "category" | "tech" | "keywords" | "hiring" | "country" | "region";
  label: string;
  /** Switched on and has values. */
  on: boolean;
  /** The seeds Splee has this kind of data for. */
  known: number;
  values: LookalikeTrait[];
}

export interface LookalikeMatch {
  domain: string;
  name: string;
  website: string;
  city: string | null;
  region: string | null;
  country_code: string;
  category: string | null;
  /** 0-100, the share of the profile's points it earns. */
  score: number;
  reasons: string[];
}

export interface Lookalikes {
  profile: { seeds: LookalikeSeed[]; parts: LookalikePart[] };
  results: LookalikeMatch[];
  database_empty: boolean;
  settings: LookalikeSettings;
}

/** The hand-picked seeds and the profile parts switched off, by key. */
export interface LookalikeSettings {
  added: string[];
  removed: string[];
  off: string[];
}

export interface DirectoryPage extends Paginated<DirectoryCompany> {
  /** `total` stopped counting at 10,000 and there are more. */
  capped: boolean;
}

/** A company database search saved on the Companies page. Splee checks it hourly for companies that match and didn't before. */
export interface SavedSearch {
  id: string;
  project_id: string;
  /** Who hears about new matches. Null for one saved with an API key. */
  user_id: string | null;
  name: string;
  /** The filters as /directory/companies takes them. Empty ones aren't applied. */
  q: string;
  country: string;
  city: string;
  category: string;
  has_email: boolean;
  has_phone: boolean;
  tech: string;
  chat_alerts: boolean;
  /** The campaign new matches go into, up to 200 a check. */
  add_to_segment_id: string | null;
  campaign_name: string | null;
  baseline_at: string | null;
  checked_at: string | null;
  /** It matched over 10,000 companies, so it isn't checked. */
  too_broad: boolean;
  opened_at: string | null;
  created_at: string;
  /** Companies found since it was last opened, and when the first of them was found. */
  new_matches: number;
  new_since: string | null;
}

/** The companies a saved search found since `since`, newest first. `since` is null when none are new. */
export interface SavedSearchMatches extends Paginated<DirectoryCompany & { found_at: string }> {
  since: string | null;
}

/** What POST /directory/filters made of a description: the search's filters, null or false where it set none, and a sentence
 * on what it couldn't turn into one, or "". */
export interface DescribedFilters {
  filters: {
    q: string | null;
    country: string | null;
    city: string | null;
    category: string | null;
    has_email: boolean;
    has_phone: boolean;
    tech: string | null;
  };
  note: string;
}

/** How many rows Download CSV takes: the matches, up to the 50,000 one download holds. `capped` says more match. */
export interface DirectoryDownload {
  rows: number;
  capped: boolean;
}

/** What "Find people" on the Companies page added from LinkedIn. A skipped company has an empty name. */
export interface FoundPeople {
  added: { name: string; title: string; company: string; linkedin_url: string }[];
  skipped: { name: string; company: string; reason: string }[];
}

/** A business from a Google Maps search on the Companies page. `known` says the project already has it. */
export interface LocalBusiness {
  place_id: string;
  name: string;
  category: string;
  types: string[];
  address: string;
  city: string;
  region: string;
  country_code: string;
  lat: number | null;
  lng: number | null;
  /** E.164 when it could be read, else as Google wrote it, or "". */
  phone: string;
  website: string;
  domain: string | null;
  rating: number | null;
  review_count: number;
  maps_url: string;
  known: "company" | "lead" | null;
}

/** The organization's Google Maps calls this calendar month (UTC). Keep in sync with local_businesses::Usage. */
export interface LocalUsage {
  connected: boolean;
  calls: number;
  cap: number;
  price_per_1000_cents: number;
  cost_cents: number;
}

export interface LocalSearch {
  items: LocalBusiness[];
  /** Businesses found before the filters. */
  found: number;
  /** Calls this search made, 0 when it all came from searches of the last 30 days. */
  calls: number;
  usage: LocalUsage;
  /** Why some places weren't searched, like the monthly cap. */
  note: string | null;
}

/** What "Add to campaign" did with each picked business. */
export interface LocalAdded {
  added: {
    name: string;
    domain: string | null;
    phone: string;
    email: string | null;
    email_kind: "person" | "role" | "";
    contact_name: string;
    note: string;
  }[];
  skipped: { name: string; reason: string }[];
}

export interface DirectoryCountry {
  country_code: string;
  companies: number;
}

export interface DirectoryImport {
  id: UUID;
  country: string;
  status: "queued" | "running" | "done" | "failed";
  /** Overture's row groups that can hold the country. `groups_total` is 0 until every file's index is read. */
  groups_done: number;
  groups_total: number;
  rows_seen: number;
  rows_kept: number;
  error: string;
  requested_by: string | null;
  created_at: Ts;
  started_at: Ts | null;
  finished_at: Ts | null;
}

export type LeadStatus =
  | "new"
  | "queued"
  | "contacted"
  | "replied"
  | "hot"
  | "meeting_booked"
  | "not_interested"
  | "bounced"
  | "unsubscribed"
  | "do_not_contact";

export interface Lead extends Scored {
  id: UUID;
  project_id: UUID;
  segment_id: UUID | null;
  company_id: UUID | null;
  full_name: string;
  first_name: string;
  last_name: string;
  title: string;
  email: string | null;
  email_status: "unknown" | "found" | "guessed" | "verified" | "invalid";
  linkedin_url: string;
  x_url: string;
  /** In E.164, like +447700900123, or "" for none. */
  phone: string;
  /** The finder that found the phone, like "RocketReach", or "" when it came some other way. */
  phone_source: string;
  company_name: string;
  company_domain: string | null;
  city: string;
  country_code: string;
  source: "agent" | "import" | "api" | "form" | "linkedin" | "google_maps" | "chat" | "referral";
  status: LeadStatus;
  personalization: { hook?: string; facts?: string[] };
  notes: string;
  remind_at: Ts | null;
  /** When the call they booked starts, if their scheduling tool said. */
  meeting_at: Ts | null;
  last_contacted_at: Ts | null;
  created_at: Ts;
  /** The campaign's A/B test variant, picked when its first email is written. */
  variant_id: UUID | null;
  /** How finding an address for a lead without one went. "" is never searched. */
  email_search: EmailSearch;
  /** The finder that found it, like "Hunter", or the pattern a guess followed, like "first.last". */
  email_source: string;
  email_search_note: string;
  /** Calls to paid finders and verifiers, and the credits they said they charged, null when none said. */
  email_search_lookups: number;
  email_search_credits: number | null;
  email_searched_at: Ts | null;
  /** The team member who works the lead. */
  owner_id: UUID | null;
  /** A code from LANGUAGES set on the lead, or "" to go by their country and their company's website. */
  language: string;
}

/** The lead's own language, as a campaign set to each lead's language picks it, and where it came from. */
export interface EffectiveLanguage {
  effective_language: LanguageCode;
  effective_language_source: LanguageSource;
}

export const FINDER_NAMES = {
  hunter: "Hunter",
  findymail: "Findymail",
  prospeo: "Prospeo",
  anymailfinder: "Anymail Finder",
  leadmagic: "LeadMagic",
  apollo: "Apollo",
  voilanorbert: "Voila Norbert",
  skrapp: "Skrapp",
  lusha: "Lusha",
  peopledatalabs: "People Data Labs",
  rocketreach: "RocketReach",
  snov: "Snov.io",
  dropcontact: "Dropcontact",
  contactout: "ContactOut",
  kaspr: "Kaspr",
} as const;

export type FinderName = keyof typeof FINDER_NAMES;

/** A connected email finder. Its key is write-only, through PUT /projects/:id/finders/:provider. */
export interface ConnectedFinder {
  provider: FinderName;
  error: string | null;
  failed_at: Ts | null;
  /** Out of credits or past its rate limit, so the email search skips it until then. */
  paused_until: Ts | null;
}

/** The project's email finders, in the order the email search asks them. */
export interface Finders {
  finders: ConnectedFinder[];
  find_phones: boolean;
}

export type EmailSearch = "" | "queued" | "found" | "guessed" | "catch_all" | "not_found" | "suppressed" | "duplicate" | "failed";

/** Leads waiting for an email search, and how many more the organization can search this hour. */
export interface EmailSearchProgress {
  queued: number;
  room: number;
  hourly: number;
}

export interface LeadRow extends Lead, EffectiveLanguage {
  segment_name: string | null;
  last_email_status: string | null;
  /** The watched roles that open posts at their company match, newest first. */
  hiring: string[];
  /** The owner's name, or their email when they have none. */
  owner_name: string | null;
  owner_email: string | null;
}

/** An email's opens, or its clicks on one link, scanners left out. */
export interface EmailEvent {
  email_id: UUID;
  kind: "open" | "click";
  /** The clicked link's host and path, like acme.com/pricing. */
  link: string | null;
  url: string | null;
  count: number;
  first_at: Ts;
  last_at: Ts;
}

export interface LeadDetail extends Lead, EffectiveLanguage {
  segment_name: string | null;
  company: Company | null;
  emails: EmailMessage[];
  /** Opens and clicks of their emails, oldest first. */
  email_events: EmailEvent[];
  /** Texts Splee wrote or sent them. Their answers are in emails, with a text channel. */
  texts: TextMessage[];
  /** Calls to them, open and made, oldest first. */
  calls: Call[];
  /** Sales meetings with them from Fireflies, Fathom, Gong, Zoom or tl;dv, oldest first. */
  meetings: Meeting[];
  hiring: JobPost[];
  /** Their company's funding and news from the last 90 days, newest first. */
  news: CompanySignal[];
  /** Their open deal, or else the one that closed last. */
  deal: Deal | null;
  /** The move that made them a lead here, when a job change did. */
  job_change: JobChangeFrom | null;
  owner_name: string | null;
  owner_email: string | null;
}

/** What someone on a job change watch list did with you at their old company: bought, booked a meeting or replied hot. */
export type Relationship = "won" | "booked" | "hot";

export interface JobChangeFrom {
  old_company: string;
  old_domain: string | null;
  old_lead_id: string | null;
  relationship: Relationship;
  seen_at: string;
}

/** A move LinkedIn showed for someone on the watch list. See backend/src/jobchanges.rs. */
export interface JobChange {
  id: string;
  full_name: string;
  linkedin_url: string;
  relationship: Relationship;
  old_company: string;
  old_domain: string | null;
  old_lead_id: string | null;
  new_company: string;
  new_domain: string | null;
  new_title: string;
  /** The lead Splee made or found at the new company, null once it's deleted. */
  new_lead_id: string | null;
  new_lead_email: string | null;
  segment_id: string | null;
  segment_name: string | null;
  seen_at: string;
}

export interface JobChanges {
  /** LinkedIn connected through Unipile, which the checks need. */
  connected: boolean;
  /** The campaign leads from moves join on their own. */
  segment_id: string | null;
  watching: number;
  last_checked_at: string | null;
  changes: JobChange[];
}

export interface WatchedPerson {
  id: string;
  lead_id: string | null;
  relationship: Relationship;
  full_name: string;
  email: string | null;
  linkedin_url: string;
  company_name: string;
  company_domain: string | null;
  title: string;
  checked_at: string | null;
  check_error: string | null;
  created_at: string;
}

export interface WatchAdded {
  added: number;
  updated: number;
  skipped: number;
  errors: { row: number; error: string }[];
}

export interface CompanyRow extends Company {
  hiring: string[];
  /** The tools its last tech scan found, in catalogue order. */
  tech: string[];
  /** Its leading funding or news signal from the last 90 days: the newest funding one, else the newest. */
  news: SignalBrief | null;
}

/** A tool Splee can spot on a company's site or DNS. `category` is one of `TechCatalogue.categories`' keys. */
export interface TechTool {
  name: string;
  category: string;
}

/** A tool the project looks for: "uses" is a good sign, "competitor" a tool its product could replace. */
export interface TechPick {
  tech: string;
  kind: "uses" | "competitor";
}

export interface TechCatalogue {
  catalogue: TechTool[];
  categories: { key: string; label: string }[];
  picks: TechPick[];
}

/** A project company whose tech scan found some tools. `leads` counts the project's leads there. */
export interface TechCompany {
  id: UUID;
  domain: string;
  name: string;
  segment_id: UUID | null;
  leads: number;
  techs: string[];
  scanned_at: Ts | null;
}

/** A company that visited the project's site in the period. Keep in sync with Visitor in backend/src/routes/visitors.rs. */
export interface Visitor {
  domain: string;
  name: string;
  /** Set when it's one of the project's companies. */
  company_id: UUID | null;
  /** The campaign it, or one of its leads, is in. */
  campaign: string | null;
  leads: number;
  views: number;
  visits: number;
  last_visit: Ts;
  /** The most read pages, at most three. */
  top_paths: string[];
  /** A utm_source or referring site of its first visit, empty for a direct one. */
  source: string;
}

/** The website visitors install card. */
export interface VisitorSite {
  site_key: string;
  domains: string[];
  intent_paths: string[];
  last_hit_at: Ts | null;
  last_hit_path: string | null;
}

/** The website chat's settings. The domains it answers on are VisitorSite's. */
export interface ChatSettings {
  enabled: boolean;
  accent: string;
  greeting: string;
  position: "right" | "left";
  /** Pages it shows on, each with the pages under it. Empty for every page. */
  paths: string[];
  faq: string;
  daily_messages: number;
  monthly_budget_micros: number;
}

export interface ChatSettingsPage {
  /** The name in the chat's header. */
  title: string;
  settings: ChatSettings;
  /** Today's answers from the model and this month's spend, in UTC. */
  usage: { today_messages: number; month_spend_micros: number };
  site: VisitorSite;
}

/** A website chat conversation. `company` is what the visitor typed, else the visiting company's name. */
export interface ChatConversation {
  id: string;
  project_id: string;
  email: string | null;
  name: string;
  company: string;
  domain: string | null;
  lead_id: string | null;
  page: string;
  taken_over: boolean;
  /** A message in the last 10 minutes. */
  live: boolean;
  messages: number;
  last_message: string;
  last_role: ChatRole;
  last_message_at: Ts;
  created_at: Ts;
}

/** visitor: typed on the site. bot: the assistant. team: someone on the team. notice: Splee's fixed text, like the thanks after an email. */
export type ChatRole = "visitor" | "bot" | "team" | "notice";

export interface ChatMessage {
  id: number;
  role: ChatRole;
  body: string;
  /** The teammate's name on team messages. */
  who: string;
  created_at: Ts;
}

export interface JobPost {
  domain: string;
  title: string;
  url: string;
  excerpt: string;
}

/** An open role at one of the project's companies that matches its hiring signals. `company_id` is null for a domain that only
 * leads have, and `leads` counts the project's leads there. */
export interface HiringSignal {
  id: UUID;
  domain: string;
  company_id: UUID | null;
  company_name: string;
  segment_id: UUID | null;
  leads: number;
  title: string;
  /** The watched role the title matched. */
  role: string;
  location: string;
  department: string;
  url: string;
  posted_at: Ts | null;
  first_seen: Ts;
}

export type SignalKind = "funding_filing" | "funding_post" | "launch" | "acquisition" | "expansion" | "leadership";

export interface SignalBrief {
  kind: SignalKind;
  title: string;
  /** US dollars sold, for SEC filings. */
  amount: number | null;
  offered: number | null;
  /** A date, like 2026-10-03. */
  happened_on: string;
  url: string;
  /** "Raised $12M (SEC filing, Oct 3)", the line scores and the email writer use. */
  headline: string;
}

/** A Form D filing from SEC EDGAR or a post on a company's own feed. `checked_at` is when its source last looked at the company. */
export interface CompanySignal extends SignalBrief {
  id: UUID;
  domain: string;
  company_id: UUID | null;
  company_name: string;
  leads: number;
  source: "sec" | "feed";
  industry: string;
  first_seen: Ts;
  checked_at: Ts | null;
}

export interface SignalsOverview {
  signals: CompanySignal[];
  checked: number;
  last_checked_at: Ts | null;
  /** False when the server has no SEC_USER_AGENT, so only company feeds are read. */
  sec: boolean;
  funding_alerts: boolean;
}

export type EmailStatus =
  | "draft"
  | "approved"
  | "scheduled"
  | "sending"
  | "sent"
  | "failed"
  | "cancelled"
  | "received";

export type Classification = "hot" | "question" | "not_interested" | "ooo" | "unsubscribe" | "bounce" | "other";

export interface EmailMessage {
  id: UUID;
  project_id: UUID;
  lead_id: UUID;
  segment_id: UUID | null;
  mailbox_id: UUID | null;
  direction: "outbound" | "inbound";
  /** meeting_follow_up, call_follow_up and no_show_follow_up are the emails the agent drafts after a sales meeting, a call or a missed one. */
  kind: "initial" | "follow_up" | "auto_reply" | "manual_reply" | "inbound" | "meeting_follow_up" | "call_follow_up" | "no_show_follow_up" | "deal_nudge" | "win_back" | "expansion" | "referral_ask" | "retention" | "story_ask" | "feature_shipped";
  step: number;
  subject: string;
  body: string;
  status: EmailStatus;
  scheduled_for: Ts | null;
  sent_at: Ts | null;
  message_id: string | null;
  in_reply_to: string | null;
  from_email: string | null;
  to_email: string | null;
  classification: Classification | null;
  classification_reason: string;
  error: string;
  cost_micros: number;
  created_at: Ts;
  /** Inbound only: when you marked the reply as needing no answer. */
  handled_at: Ts | null;
  deferrals: number; // times the receiving server said to try again later
  /** The A/B test variant it was written for. Follow-ups keep the first email's. */
  variant_id: UUID | null;
  /** "sms" or "whatsapp" for a lead's answer to a text. */
  channel: "email" | TextChannel;
  /** The language Splee wrote it in, "" for English or an email it didn't write. */
  language: string;
  /** Who else a lead's reply went to, or who ours is copied to. Lowercase. */
  cc_emails: string[];
  /** Who approved it, once it was. */
  approved_via: "person" | "review_window" | null;
}

/** A file on an email, without its bytes. Download it from /attachments/:id when `stored`. */
export interface EmailAttachment {
  id: UUID;
  /** null for an upload no reply has taken yet. */
  message_id: UUID | null;
  filename: string;
  content_type: string;
  size_bytes: number;
  /** Shown in the email's body, like a signature's logo, rather than attached under it. */
  inline: boolean;
  /** false for a lead's file over 25 MB, which Splee lists but didn't keep. */
  stored: boolean;
  created_at: Ts;
}

/** An email in an inbox thread, with its files. */
export interface ThreadMessage extends EmailMessage {
  attachments: EmailAttachment[];
}

export type TextChannel = "sms" | "whatsapp";

/** A text Splee sends through the organization's Twilio account: a sequence step, or a reply typed in the inbox. */
export interface TextMessage {
  id: UUID;
  project_id: UUID;
  lead_id: UUID;
  segment_id: UUID | null;
  channel: TextChannel;
  /** "missed_call" is the text a lead who called gets when nobody answered. */
  kind: "sequence" | "reply" | "missed_call";
  step: number;
  to_number: string;
  /** For a WhatsApp template, its text with the lead's details filled in. */
  body: string;
  template_sid: string;
  template_vars: Record<string, string>;
  status: "draft" | "approved" | "sending" | "sent" | "delivered" | "failed" | "cancelled";
  due_at: Ts | null;
  sent_at: Ts | null;
  twilio_sid: string | null;
  error: string;
  created_at: Ts;
}

export interface TextStep {
  channel: TextChannel;
  delay_days: number;
  instructions: string;
}

export type CallOutcome = "connected" | "voicemail" | "no_answer" | "wrong_number" | "not_interested" | "meeting_booked";

/** A call to a lead: an open call step of the sequence, or one someone made or logged. */
export interface Call {
  id: UUID;
  project_id: UUID;
  lead_id: UUID;
  segment_id: UUID | null;
  kind: "sequence" | "manual";
  step: number;
  /** What the call step says to talk about. */
  instructions: string;
  /** Who should make it. Null is anyone on the team. */
  owner_id: UUID | null;
  status: "open" | "done" | "skipped" | "cancelled";
  due_at: Ts;
  closed_at: Ts | null;
  /** Why it was skipped or cancelled, like "They replied". */
  cancel_reason: string;
  to_number: string;
  called_by: UUID | null;
  called_at: Ts | null;
  /** Twilio's word for how the lead's side ended, like completed, busy or no-answer. */
  call_status: string;
  duration_secs: number | null;
  /** Twilio's URL, or Aircall's page for it. Play it through /api/calls/{id}/recording. */
  recording_url: string;
  recording_secs: number | null;
  outcome: CallOutcome | null;
  notes: string;
  transcript: string;
  transcribed_at: Ts | null;
  summary: string;
  next_step: string;
  created_at: Ts;
  /** The call's id in Aircall, for one made or taken there. */
  aircall_id: number | null;
  /** The call's id in Dialpad, for one made or taken there. */
  dialpad_id: number | null;
  /** The call's id in JustCall, for one made or taken there. */
  justcall_id: number | null;
  /** A call the lead made to the team. */
  inbound: boolean;
  /** The follow-up email the agent drafted from the transcript, when someone talked with the lead. */
  follow_up_email_id: string | null;
}

/** GET /projects/{id}/calls. */
export interface CallRow extends Call {
  lead_name: string;
  lead_title: string;
  lead_company: string;
  lead_phone: string;
  owner_name: string | null;
  called_by_name: string | null;
  bucket: "overdue" | "today" | "done";
  time_zone: string;
  /** The time where the lead is, like "14:05". */
  local_time: string;
  /** Why now is outside their calling hours, 8:00 to 20:00 their time. */
  outside_hours: string | null;
  /** The talk track, or null before one was asked for. See backend/src/agent/call_prep.rs. */
  prep: { prep: CallPrep | null; created_at: Ts; status: "ready" | "writing" | "failed" } | null;
  /** The status of the follow-up email the agent drafted, or null without one. */
  follow_up_status: string | null;
}

export interface CallPrep {
  opener: string;
  questions: string[];
  objections: { objection: string; answer: string }[];
  voicemail: string;
  /** From the caller's recent coaching, or empty. */
  reminder: string;
}

/** GET /voicemail: the voicemail you leave in calls from the Calls page. */
export interface Voicemail {
  secs: number;
  created_at: Ts;
}

/** POST /calls/{id}/dial. */
export interface DialToken {
  token: string;
  identity: string;
  to: string;
  expires_at: Ts;
}

/** GET /organization/twilio. */
export interface TwilioStatus {
  connection: {
    account_sid: string;
    account_name: string;
    api_key_sid: string;
    sms_from: string;
    whatsapp_from: string;
    whatsapp_template_sid: string;
    whatsapp_template_body: string;
    whatsapp_template_vars: Record<string, string>;
    /** Calling from the browser is on once this TwiML App is set. */
    twiml_app_sid: string;
    /** The number calls come from. Empty uses the SMS number. */
    voice_from: string;
    call_transcription: boolean;
    /** With local presence on, the numbers calls go out from, nearest the lead first. Empty is off. */
    local_numbers: string[];
    /** Texted to a lead who called when nobody answered. Empty is off. */
    missed_call_text: string;
    error: string | null;
    failed_at: Ts | null;
    created_at: Ts;
  } | null;
  /** Where the number's incoming messages webhook should point. */
  inbound_url: string;
  /** Where Splee points the TwiML App's Voice URL. */
  voice_url: string;
}

export interface EmailRow extends EmailMessage {
  lead_name: string;
  lead_email: string | null;
  company_name: string;
  segment_name: string | null;
  project_name: string;
  variant_name: string | null;
  /** When a draft goes out on its own if nobody reviews it. null when it waits for a person. */
  sends_at: Ts | null;
}

export interface InboxThread {
  lead: LeadRow;
  last_message: EmailMessage;
  pending_auto_reply: boolean;
  classification: Classification | null;
  project_name: string;
}

export interface LinkedinMessage {
  id: UUID;
  lead_id: UUID;
  project_id: UUID;
  step: number;
  kind: "connect" | "message";
  body: string;
  status: "todo" | "done" | "skipped";
  /** When to send it: the connect note once written, each message a few days after the step before was sent. */
  due_at: Ts | null;
  done_at: Ts | null;
  /** "unipile" when Splee sent it through the user's LinkedIn. */
  sent_via: "" | "unipile";
  /** Why Splee couldn't send it, which leaves it for the user. */
  send_error: string | null;
  created_at: Ts;
}

export interface Material {
  id: UUID;
  project_id: UUID;
  kind: "case_study" | "doc" | "faq" | "link" | "note";
  title: string;
  content: string;
  url: string;
  created_at: Ts;
}

export interface DnsCheck {
  ok: boolean;
  record?: string;
  detail?: string;
}

/** The org's registrar account. Porkbun is the only one. */
/** A mailbox or bought domain's tracking host, like track.acme.com (GET /tracking-domains). */
export interface TrackingDomain {
  domain: string;
  host: string;
  verified_at: Ts | null;
  checked_at: Ts | null;
  error: string;
  /** Bought through Porkbun, so Splee set the CNAME itself. */
  porkbun: boolean;
}

export interface TrackingDomains {
  /** What each tracking host's CNAME points to. null while PUBLIC_URL is a local address. */
  target: string | null;
  domains: TrackingDomain[];
}

export interface RegistrarAccount {
  provider: "porkbun";
  /** The Porkbun username the API key belongs to. */
  account: string;
  created_at: Ts;
}

export interface DomainAvailability {
  domain: string;
  /** null when the registry didn't answer in time. */
  available: boolean | null;
  /** What buying it charges, for the registry's minimum term. */
  price_micros: number;
  renewal_micros: number;
  premium: boolean;
}

export type MailProvider = "google" | "microsoft" | "zoho" | "zoho_eu" | "migadu";

/** A record Splee set or tried to. `name` is relative to the domain, "" for the root. Type "URL" is the root's forward. */
export interface DomainRecord {
  type: "MX" | "TXT" | "CNAME" | "URL";
  name: string;
  content: string;
  prio?: number;
  ok: boolean;
  error?: string;
  /** Pasted from the mail provider. */
  added?: boolean;
}

export interface SendingDomain {
  id: UUID;
  org_id: UUID;
  domain: string;
  /** uncertain: no clear answer from Porkbun, so it may have been bought. Never retried on its own. */
  status: "queued" | "registering" | "registered" | "uncertain" | "failed";
  price_micros: number;
  order_id: string;
  error: string;
  /** "2027-11-10" */
  expires_on: string | null;
  registered_at: Ts | null;
  provider: MailProvider;
  forward_to: string;
  dmarc_email: string;
  dns_status: "" | "setting" | "set" | "error";
  records: DomainRecord[];
  dns: Mailbox["dns"];
  created_at: Ts;
  /** The org's mailboxes on this domain, and those warming up. */
  mailboxes: number;
  warming: number;
}

export interface Mailbox {
  id: UUID;
  org_id: UUID;
  email: string;
  /** Whose mailbox it is. The "by mailbox" assignment rule gives them the leads it emailed. */
  owner_id: UUID | null;
  from_name: string;
  /** Used instead of the project's in the emails Splee writes, when this mailbox sends them. Empty keeps the project's. */
  signature: string;
  booking_link: string;
  /** Tried first by Check DNS, for a provider that signs with a selector of its own. Empty for none. */
  dkim_selector: string;
  smtp_host: string;
  smtp_port: number;
  smtp_security: "tls" | "starttls" | "none";
  smtp_username: string;
  imap_host: string;
  imap_port: number;
  imap_username: string;
  daily_limit: number;
  cost_per_email_micros: number;
  status: "active" | "paused" | "error";
  last_error: string;
  imap_last_uid: number;
  imap_uid_validity: number | null;
  dns: { mx?: DnsCheck; spf?: DnsCheck; dmarc?: DnsCheck; dkim?: DnsCheck; checked_at?: Ts };
  created_at: Ts;
  recheck_at: Ts | null;
  ramp_up: boolean;
  /** The least minutes between two emails it sends, 0 for none. Replies don't wait for it. */
  min_gap_minutes: number;
  /** Another of the org's mailboxes that replies go to, by Reply-To. Null for this one. */
  reply_to_mailbox_id: string | null;
  /** "2026-11-09", the UTC day it first sent. */
  first_sent_on: string | null;
  /** Trades warmup emails with the other warming mailboxes in this Splee. */
  warmup: boolean;
  /** The UTC day of its first warmup email. Turning warmup off clears it. */
  warmup_started_on: string | null;
  warmup_daily_cap: number;
  /** Campaign plus warmup emails a day, while warmup is on. */
  total_daily_limit: number;
  /** No campaign emails in the first 14 days of warmup, or while its warmup emails land in spam. */
  hold_until_warm: boolean;
  /** "password" for SMTP and IMAP with a password, or the provider it was connected to by signing in. */
  provider: "password" | SignInProvider;
  /** The provider signed Splee out, so it waits for someone to sign in again. */
  needs_reconnect: boolean;
  /** Set while the Deliverability check holds it from sending. It's paused, with why in last_error. */
  health_held_at: Ts | null;
}

/** From routes/deliverability.rs. */
export type Verdict = "good" | "watch" | "at_risk";

export interface HealthReason {
  code: string;
  level: Verdict;
  text: string;
  /** What to do about it. */
  fix: string;
}

/** Leads emailed in a window, and how many of them bounced, opted out or replied. */
export interface HealthRates {
  emailed: number;
  bounced: number;
  opted_out: number;
  replied: number;
}

export interface BlocklistCheck {
  list: string;
  label: string;
  kind: "domain" | "ip";
  target: string;
  /** "unknown" when the list couldn't be asked, which isn't a listing. */
  status: "clear" | "listed" | "unknown";
  detail: string;
  delist_url: string;
  checked_at: Ts;
}

export interface PostmasterDay {
  domain: string;
  day: string;
  /** "HIGH", "MEDIUM", "LOW", "BAD" or "" when Google gave none. */
  reputation: string;
  spam_rate: number | null;
  spf_rate: number | null;
  dkim_rate: number | null;
  dmarc_rate: number | null;
}

export interface MailboxHealth {
  id: UUID;
  email: string;
  domain: string;
  status: Mailbox["status"];
  held: boolean;
  last_error: string;
  verdict: Verdict;
  reasons: HealthReason[];
  week: HealthRates;
  month: HealthRates;
  ips: string[];
  ip_source: "received" | "smtp_host";
}

export interface DomainHealth {
  domain: string;
  verdict: Verdict;
  reasons: HealthReason[];
  listings: BlocklistCheck[];
  postmaster: PostmasterDay | null;
  mailboxes: number;
}

/** A chart day: the bounce and opt-out rates of the week ending that day, and Gmail's spam rate that day. */
export interface HealthPoint {
  date: string;
  bounce_rate: number | null;
  opt_out_rate: number | null;
  spam_rate: number | null;
}

export interface Deliverability {
  checked_at: Ts | null;
  domains: DomainHealth[];
  mailboxes: MailboxHealth[];
  /** The first is every domain together, with domain "". */
  series: { domain: string; days: HealthPoint[] }[];
  postmaster: {
    /** This Splee has a Google app to sign in with. */
    available: boolean;
    connection: { email: string; domains: string[]; error: string; synced_at: Ts | null; signed_out: boolean } | null;
  };
}

/** Monday first: null for a day off, else [start, end] in minutes after midnight. */
export type WorkingHours = ([number, number] | null)[];

/** A call booked on a booking page. */
export interface Booking {
  id: UUID;
  page_id: UUID;
  starts_at: Ts;
  ends_at: Ts;
  name: string;
  email: string;
  note: string;
  /** The guest's time zone, which their emails use. */
  timezone: string;
  sequence: number;
  cancelled_at: Ts | null;
  created_at: Ts;
}

/** GET /booking-pages: a mailbox's booking page at /book/{slug}. */
export interface BookingPage {
  id: UUID;
  org_id: UUID;
  mailbox_id: UUID;
  slug: string;
  name: string;
  duration_minutes: 15 | 30 | 45 | 60;
  timezone: string;
  hours: WorkingHours;
  buffer_minutes: number;
  notice_minutes: number;
  days_ahead: number;
  /** A video link or where to meet. */
  location: string;
  /** How long before a call the guest gets a reminder email, 0 for none. */
  reminder_hours: number;
  /** Why the calendar couldn't be read the last time, or empty. */
  calendar_error: string;
  created_at: Ts;
  url: string;
  mailbox_email: string;
  host: string;
  /** The saved calendar address's host, like calendar.google.com, or empty for none. The address itself stays secret. */
  calendar_host: string;
  upcoming: Booking[];
}

/** PATCH /booking-pages/{id}. `calendar_url` "" removes the calendar, left out keeps it. */
export type BookingPageInput = Partial<
  Pick<
    BookingPage,
    "slug" | "name" | "duration_minutes" | "timezone" | "hours" | "buffer_minutes" | "notice_minutes" | "days_ahead" | "location" | "reminder_hours"
  >
> & { calendar_url?: string };

/** GET /book/{slug}: what a visitor sees. `location` says "Video call" for a link, which only the invite holds. */
export interface PublicBookingPage {
  slug: string;
  name: string;
  host: string;
  duration_minutes: number;
  location: string;
  slots: Ts[];
}

/** GET /booking/{token}: a booking behind the link in its emails. */
export interface ManagedBooking {
  page: PublicBookingPage;
  name: string;
  starts_at: Ts;
  ends_at: Ts;
  cancelled: boolean;
  started: boolean;
}

/** GET /mailboxes/warmup: one of the org's mailboxes with warmup on, over the last 14 days. */
export interface WarmupHealth {
  mailbox_id: UUID;
  /** Day 1 is the day of its first warmup email. null before that. */
  day: number | null;
  sent_today: number;
  target_today: number;
  sent: number;
  received: number;
  /** Where its warmup emails landed, as the receiving mailboxes saw them. */
  inbox: number;
  spam: number;
  /** Whether another mailbox in this Splee is warming up to trade emails with. */
  has_peers: boolean;
}

/** GET /mailboxes/stats: distinct leads over the last 30 days. */
export interface MailboxStats {
  mailbox_id: UUID;
  email: string;
  emailed: number;
  bounced: number;
  replied: number;
}

export interface MailboxInput {
  email: string;
  from_name: string;
  /** Left out on create, the mailbox is whoever connects it. Null is nobody. */
  owner_id?: string | null;
  /** Used instead of the project's in the emails Splee writes, when this mailbox sends them. Empty keeps the project's. */
  signature: string;
  booking_link: string;
  /** Tried first by Check DNS, for a provider that signs with a selector of its own. Empty for none. */
  dkim_selector: string;
  smtp_host: string;
  smtp_port: number;
  smtp_security: "tls" | "starttls" | "none";
  smtp_username: string;
  smtp_password: string;
  imap_host: string;
  imap_port: number;
  imap_username: string;
  imap_password: string;
  daily_limit: number;
  cost_per_email_micros: number;
  ramp_up: boolean;
  min_gap_minutes: number;
  /** Only saved on edit. */
  reply_to_mailbox_id?: string | null;
  warmup: boolean;
  warmup_daily_cap: number;
  total_daily_limit: number;
  hold_until_warm: boolean;
  status?: "active" | "paused";
}

export interface Suppression {
  id: UUID;
  org_id: UUID;
  value: string;
  kind: "email" | "domain";
  reason: "unsubscribe" | "manual" | "bounce" | "customer";
  created_at: Ts;
}

/** A named do-not-contact list: emails for people, domains for companies. */
export interface SuppressionList {
  id: UUID;
  name: string;
  kind: "people" | "companies";
  /** null covers every project. */
  project_ids: UUID[] | null;
  created_by: UUID | null;
  created_at: Ts;
  entries: number;
}

/** A CSV column the list upload could read, with how many of its cells are emails or domains. */
/** Domains the company database search leaves out. It never stops an email. */
export interface DedupList {
  id: UUID;
  name: string;
  /** null covers every project. */
  project_ids: UUID[] | null;
  created_by: UUID | null;
  created_at: Ts;
  size: number;
}

export interface SuppressionListColumn {
  header: string;
  kind: SuppressionList["kind"] | null;
  values: number;
}

export interface Import {
  id: UUID;
  project_id: UUID;
  segment_id: UUID | null;
  filename: string;
  total: number;
  imported: number;
  skipped: number;
  errors: { row: number; error: string }[];
  created_at: Ts;
}

export type OutreachTool = "instantly" | "smartlead" | "lemlist";

/** A campaign in another outreach tool. `leads` is null when the tool wouldn't say. */
export interface OutreachCampaign {
  id: string;
  name: string;
  status: string;
  leads: number | null;
}

/** A campaign being brought over, with what the import did to it so far. */
export interface OutreachImportCampaign {
  id: string;
  name: string;
  leads: number | null;
  status: "queued" | "running" | "done" | "failed";
  segment_id: UUID | null;
  steps: number;
  added: number;
  duplicates: number;
  suppressed: number;
  invalid: number;
  error: string;
}

export interface OutreachImport {
  id: UUID;
  project_id: UUID;
  tool: OutreachTool;
  status: "queued" | "running" | "done" | "failed";
  campaigns: OutreachImportCampaign[];
  include_blocklist: boolean;
  blocklist_added: number;
  error: string;
  created_at: Ts;
  finished_at: Ts | null;
}

/** The emails of a campaign brought over from another tool. */
export interface SegmentSteps {
  imported_from: string | null;
  steps: { position: number; delay_days: number; subject: string; body: string }[];
}

export type RunKind =
  | "explore"
  | "research_company"
  | "find_companies"
  | "find_people"
  | "write_emails"
  | "classify_reply"
  | "draft_reply"
  | "linkedin_sequence"
  | "explorer"
  | "segment_from_prompt"
  | "social"
  | "write_texts"
  | "summarize_call"
  | "meeting_notes"
  | "research_companies"
  | "meeting_brief"
  | "campaign_insights"
  | "ask"
  | "buying_committee"
  | "deal_nudge"
  | "close_plan"
  | "proposal"
  | "deal_health"
  | "deal_review"
  | "qualify_deal"
  | "pipeline_forecast"
  | "buyer_profile"
  | "coach_call"
  | "call_prep"
  | "win_back"
  | "battlecard"
  | "expansion"
  | "referral_ask"
  | "churn_risk"
  | "practice"
  | "one_on_one"
  | "renewal"
  | "playbook"
  | "no_show_follow_up";

export interface TumblrBlog {
  name: string;
  title: string;
  url: string;
  uuid: string;
}

export interface HashnodePublication {
  id: string;
  title: string;
  url: string;
}

/** A reply or post for a social platform, drafted by a social run or written by hand, posted once approved. */
export interface SocialPost {
  id: UUID;
  project_id: UUID;
  run_id: UUID | null;
  /** LinkedIn posts go out through the project's Unipile connection. */
  platform:
    | "x"
    | "reddit"
    | "bluesky"
    | "mastodon"
    | "linkedin"
    | "threads"
    | "youtube"
    | "stackexchange"
    | "dev"
    | "discourse"
    | "lemmy"
    | "nostr"
    | "tumblr"
    | "hashnode"
    | "discord"
    | "telegram"
    | "facebook"
    | "instagram"
    | "blog";
  /** The link of the post or comment it answers, null for an original post. On Tumblr it's reblogged with the reply as its comment. */
  in_reply_to: string | null;
  /** The text it answers. */
  context: string;
  /** Where a Reddit post goes, or the subreddit of the thread it answers. The same for a Lemmy community, as name@instance. */
  subreddit: string;
  /** Reddit, Lemmy and Discourse posts and DEV, Hashnode and blog articles only. */
  title: string;
  /** DEV articles, at most 4, Hashnode and blog articles, at most 5, and Tumblr posts, at most 10. */
  tags: string[];
  body: string;
  /** Why the agent picked the thread. */
  reason: string;
  status: "draft" | "approved" | "posting" | "posted" | "failed" | "skipped";
  /** The handle or username it went out from, or the Unipile account id for LinkedIn. */
  account: string;
  url: string;
  error: string | null;
  posted_at: Ts | null;
  created_at: Ts;
  updated_at: Ts;
  /** Discourse only: the forum it goes to, null once that forum is removed. */
  forum_id: UUID | null;
  /** Discord, Telegram, Facebook and Instagram only: the channel, Page or account it goes to, null once it's gone. */
  channel_id: UUID | null;
  /** A Facebook post's link, shown as a preview under it. Empty for none. */
  link: string;
  /** An Instagram post's picture. */
  image_url: string;
  /** Blog articles only: the blog it goes to, null once that blog is removed, and its excerpt. */
  blog_id: UUID | null;
  excerpt: string;
  /** The article went in as a draft on the blog, for the user to publish there. */
  saved_as_draft: boolean;
}

/** A Discord text channel or a Telegram group or channel the project's bot can see, or a Facebook Page or Instagram account the
 *  user who signed in with Facebook manages. Picked ones are read and posted in. */
export interface SocialChannel {
  id: UUID;
  project_id: UUID;
  platform: "discord" | "telegram" | "facebook" | "instagram";
  /** Discord's channel id, Telegram's chat id, the Page's id or the Instagram account's. */
  chat_id: string;
  /** The Discord server, or the Page an Instagram account is linked to. Empty on Telegram and for a Page. */
  guild_id: string;
  guild_name: string;
  name: string;
  kind: "text" | "announcement" | "group" | "supergroup" | "channel" | "page" | "instagram";
  /** A public Telegram chat's @username, a Page's or an Instagram account's. */
  username: string;
  /** On Facebook and Instagram, whether the user may answer comments (can_read) and post (can_send). */
  can_read: boolean;
  can_send: boolean;
  picked: boolean;
  created_at: Ts;
}

export interface AgentRun {
  id: UUID;
  project_id: UUID | null;
  org_id: UUID | null;
  kind: RunKind;
  input: Record<string, unknown>;
  status: "queued" | "running" | "succeeded" | "failed" | "cancelled";
  step: number;
  output: Record<string, unknown>;
  error: string;
  cost_usd: number;
  created_at: Ts;
  started_at: Ts | null;
  finished_at: Ts | null;
}

export interface AgentEvent {
  id: number;
  run_id: UUID;
  step: number;
  kind: "phase" | "log" | "tool" | "done" | "error";
  message: string;
  data: Record<string, unknown>;
  created_at: Ts;
}

export interface DemoState {
  project: Project;
  competitors: Competitor[];
  segments: Segment[];
  companies: Company[];
  leads: Lead[];
  emails: EmailMessage[];
  run: AgentRun | null;
  events: AgentEvent[];
}

export interface ExplorerCompany {
  domain: string;
  name: string;
  description: string;
  city: string;
  country_code: string;
  employees: string;
  fit_reason: string;
}

export interface SupportMessage {
  id: UUID;
  conversation_id: UUID;
  role: "user" | "assistant";
  content: string;
  created_at: Ts;
}

export interface Paginated<T> {
  items: T[];
  total: number;
}

export type StageKind = "open" | "won" | "lost";

/** Keep in sync with deals::Stage in backend/src/deals.rs. */
export interface DealStage {
  id: UUID;
  name: string;
  kind: StageKind;
  position: number;
  /** 0 to 100. Weights an open stage's deals in the forecast. */
  probability: number;
  /** Where hot replies open a deal ("hot") or booked meetings move it ("booked"). */
  auto: "hot" | "booked" | null;
  /** Every deal in the stage, however long ago it closed. */
  deals: number;
}

/** Keep in sync with deals::Deal in backend/src/deals.rs. */
export interface Deal {
  id: UUID;
  project_id: UUID;
  project_name: string;
  lead_id: UUID;
  lead_name: string;
  lead_email: string | null;
  lead_title: string;
  company_name: string;
  company_domain: string | null;
  stage_id: UUID;
  stage: string;
  status: StageKind;
  /** In the currency's main unit, like 1200.5 for $1,200.50. */
  value: number;
  currency: string;
  /** YYYY-MM-DD. */
  close_date: string | null;
  /** The lead's owner. Setting it on the deal assigns the lead. */
  owner_id: UUID | null;
  owner_name: string | null;
  notes: string;
  lost_reason: string;
  /** A payment provider when the lead's first payment made the deal. */
  source: "manual" | "hot_reply" | "booking" | "api" | PaymentProvider;
  /** The provider of the lead's first payment, on a won deal it won. */
  paid_through: PaymentProvider | null;
  stage_changed_at: Ts;
  closed_at: Ts | null;
  created_at: Ts;
  /** The lead's latest sales meeting and the agent's summary of it. */
  meeting_id: UUID | null;
  meeting_title: string | null;
  meeting_at: Ts | null;
  meeting_summary: string | null;
  /** The latest email either way, call or stage change. */
  last_touch_at: Ts;
  nudge: DealNudge | null;
  /** How far the close plan got, when it has steps. */
  plan: { done: number; total: number } | null;
  health: DealHealth | null;
  /** A rating is being written now, the first one included. */
  health_writing: boolean;
  /** Once it's won or lost, the agent's review of why. */
  review: DealReview | null;
  /** The agent's latest finished MEDDICC scorecard on an open deal. */
  qualification: { items: QualificationItem[]; confirmed: number; created_at: Ts } | null;
  /** A scorecard is being written now, the first one included. */
  qualification_writing: boolean;
}

export type ForecastCategory = "commit" | "best_case" | "later";

/** The agent's forecast of a project's open deals for the month. Totals are summed from the deals, never written by the agent. */
export interface PipelineForecast {
  headline: string;
  /** YYYY-MM-DD, the last day of the month it forecasts. */
  month_end: string;
  deals: { deal_id: UUID; category: ForecastCategory; why: string }[];
  /** What to do this week, per deal. */
  focus: { deal_id: UUID; text: string }[];
  /** Why each is slipping. */
  slipping: { deal_id: UUID; text: string }[];
}

export interface ForecastView {
  latest: { id: UUID; forecast: PipelineForecast | null; created_at: Ts; status: "ready" | "writing" | "failed" } | null;
}

/** A trait the agent found in the cited won or lost deals. `count` is how many of those deals it cites. */
export interface BuyerTrait {
  trait: string;
  count: number;
  why: string;
  deal_ids: UUID[];
}

export interface BuyersView {
  latest: {
    id: UUID;
    profile: { summary: string; traits: BuyerTrait[]; avoid: BuyerTrait[]; campaign: string } | null;
    /** Won deals it read. */
    won: number;
    created_at: Ts;
    status: "ready" | "writing" | "failed";
  } | null;
  won: number;
  min_won: number;
}

/** One MEDDICC element. `evidence` is empty when unknown, and `ask` is empty when confirmed. */
export interface QualificationItem {
  key: string;
  status: "confirmed" | "partial" | "unknown";
  evidence: string;
  ask: string;
}

/** When to try a lost deal again, and the email for that day. See backend/src/agent/win_backs.rs. */
export interface WinBack {
  id: string;
  deal_id: string;
  lead_id: string;
  lead_name: string;
  company_name: string;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  deal_status: string;
  lost_at: Ts;
  /** The deal review's reason, once it's written. */
  reason: string | null;
  /** YYYY-MM-DD, or null until it's planned. */
  reach_out_on: string | null;
  why: string;
  angle: string;
  /** What brought the day forward, like a visit to the pricing page. */
  came_back: string;
  /** Why there's no email, once the agent decided against one. */
  skipped: string;
  email_id: string | null;
  email_subject: string | null;
  email_body: string | null;
  email_status: EmailRow["status"] | null;
  status: "planning" | "waiting" | "writing" | "drafted" | "skipped" | "failed";
}

export interface WinBackView {
  enabled: boolean;
  items: WinBack[];
}

/** Why the agent thinks a closed deal was won or lost. `reason` is null until it's written, and `writing` says one is on its way. */
export interface DealReview {
  reason: string | null;
  story: string;
  turning_point: string;
  competitor: string;
  lesson: string;
  created_at: Ts;
  writing: boolean;
}

/** The agent's latest finished read on an open deal. */
export interface DealHealth {
  health: "on_track" | "at_risk" | "stalled";
  likelihood: number;
  why: string;
  next_step: string;
  created_at: Ts;
}

/** A nudge the agent drafted while the deal was quiet. A done one without an email was skipped, and `why` says why. */
export interface DealNudge {
  id: UUID;
  why: string;
  created_at: Ts;
  email_id: UUID | null;
  email_status: EmailStatus | null;
  subject: string | null;
  body: string | null;
  status: "writing" | "done" | "failed";
}

/** GET /today: what's waiting on you across the organization's projects. A part's `total` counts past the few listed. */
export interface Today {
  replies: TodayPart<{
    lead_id: UUID;
    project_id: UUID;
    project_name: string;
    name: string;
    company: string;
    classification: Classification | null;
    snippet: string;
    at: Ts;
  }>;
  /** Drafts waiting for approval, by project and kind. Everyone's, since approving is the team's job. */
  approvals: { project_id: UUID; project_name: string; kind: EmailRow["kind"]; count: number }[];
  calls: TodayPart<{ id: UUID; lead_id: UUID; project_id: UUID; name: string; company: string; due_at: Ts }>;
  meetings: TodayPart<{ lead_id: UUID; project_id: UUID; name: string; company: string; meeting_at: Ts; brief: boolean }>;
  quiet: TodayPart<{
    deal_id: UUID;
    lead_id: UUID;
    project_id: UUID;
    name: string;
    company: string;
    stage: string;
    value_cents: number;
    currency: string;
    last_touch_at: Ts;
    nudge_ready: boolean;
  }>;
  /** Practice calls a 1:1 prep assigned to you, on deals still open, until one's scored. */
  practice: TodayPart<PracticeAssignment>;
}

export interface TodayPart<T> {
  total: number;
  items: T[];
}

/** In the order the integrations page offers them. Keep in sync with Provider in backend/src/meetings/mod.rs. */
export const MEETING_PROVIDERS = ["fireflies", "fathom", "gong", "zoom", "tldv"] as const;
export type MeetingProvider = (typeof MEETING_PROVIDERS)[number];
export const MEETING_NAMES: Record<MeetingProvider, string> = { fireflies: "Fireflies", fathom: "Fathom", gong: "Gong", zoom: "Zoom", tldv: "tl;dv" };
/** Read every 15 minutes instead of sending webhooks. Keep in sync with Provider::polled. */
export const MEETING_POLLED: readonly MeetingProvider[] = ["gong", "tldv"];

/** GET /projects/:id/meeting-connections. Keys and secrets never come back. */
export interface MeetingConnection {
  provider: MeetingProvider;
  /** The secret in the webhook URL, /api/meeting-webhooks/{provider}/{webhook_token}. Gong and tl;dv have no webhook. */
  webhook_token: string;
  has_secret: boolean;
  last_event_at: Ts | null;
  /** Gong's or tl;dv's last read, every 15 minutes, and why it failed. */
  synced_at: Ts | null;
  error: string;
  /** Meetings kept from it. */
  meetings: number;
  created_at: Ts;
}

/** A sales meeting with a lead and the agent's notes on it. GET /leads/:id/meetings. */
export interface Meeting {
  id: UUID;
  project_id: UUID;
  lead_id: UUID;
  provider: MeetingProvider;
  external_id: string;
  /** The Splee booking it was. */
  booking_id: UUID | null;
  title: string;
  started_at: Ts;
  duration_secs: number | null;
  /** Their side of the call. */
  attendees: { name: string; email: string }[];
  /** The recorder's own summary, or empty. */
  provider_summary: string;
  recording_url: string;
  summary: string;
  next_steps: string[];
  follow_up_email_id: UUID | null;
  /** Draft until someone approves the follow-up. */
  follow_up_status: EmailMessage["status"] | null;
  suggested_stage_id: UUID | null;
  suggested_stage: string | null;
  stage_reason: string;
  stage_status: "none" | "pending" | "accepted" | "dismissed";
  summarized_at: Ts | null;
  transcript_chars: number;
  created_at: Ts;
}

/** In the order the integrations page offers them. Keep in sync with Provider in backend/src/payments/mod.rs. */
export const PAYMENT_PROVIDERS = ["stripe", "paddle", "lemonsqueezy"] as const;
export type PaymentProvider = (typeof PAYMENT_PROVIDERS)[number];
export const PAYMENT_NAMES: Record<PaymentProvider, string> = { stripe: "Stripe", paddle: "Paddle", lemonsqueezy: "Lemon Squeezy" };

/** GET /projects/{id}/aircall, /dialpad or /justcall, null until it's connected. The API token, key or secret never leaves the server. */
export interface CloudPhoneConnection {
  last_event_at: Ts | null;
  /** Why the last event couldn't be handled, cleared by the next one that is. */
  error: string;
  /** Calls with the project's leads kept from it. */
  calls: number;
  created_at: Ts;
}

/** GET /projects/:id/payments. The key and signing secret never come back. */
export interface PaymentConnection {
  provider: PaymentProvider;
  /** The secret in the webhook URL, /api/payments/{provider}/{webhook_token}. */
  webhook_token: string;
  has_secret: boolean;
  /** The look back over the last 90 days that connecting starts. */
  backfill_status: "queued" | "running" | "done" | "failed";
  backfill_error: string;
  backfill_found: number;
  backfill_matched: number;
  backfilled_at: Ts | null;
  last_event_at: Ts | null;
  created_at: Ts;
}

/** In the order the pages show them. Keep in sync with Platform in backend/src/ads/mod.rs. */
export const AD_PLATFORMS = ["linkedin", "google", "meta"] as const;
export type AdPlatform = (typeof AD_PLATFORMS)[number];
export const AD_PLATFORM_NAMES: Record<AdPlatform, string> = { linkedin: "LinkedIn", google: "Google Ads", meta: "Meta" };

/** A project's sign-in to an ad platform. The tokens never come back. */
export interface AdConnection {
  platform: AdPlatform;
  /** The ad accounts the sign-in can use, to pick from. */
  ad_accounts: { id: string; name: string }[];
  /** Empty until one is picked. */
  ad_account_id: string;
  ad_account_name: string;
  needs_reconnect: boolean;
  error: string | null;
  failed_at: Ts | null;
  /** Until when the platform asked Splee to slow down, while that's still ahead. */
  held_until: Ts | null;
}

export interface AdAudience {
  platform: AdPlatform;
  /** Null for the hot leads' audience. */
  segment_id: string | null;
  name: string;
  /** As the platform last reported it. Null until it does. */
  size: number | null;
  /** The leads Splee has sent it. */
  uploaded: number;
  synced_at: Ts | null;
  error: string | null;
}

/** GET /projects/:id/ads, with the hot leads' audiences, and GET /segments/:id/audiences, with the campaign's. */
export interface AdsStatus {
  /** The platforms this Splee can connect. */
  available: AdPlatform[];
  connections: AdConnection[];
  audiences: AdAudience[];
}

/** The lead fields an import's columns can fill, as the backend's import takes them. */
export const LEAD_IMPORT_FIELDS: { key: string; label: string }[] = [
  { key: "email", label: "Email" },
  { key: "full_name", label: "Full name" },
  { key: "first_name", label: "First name" },
  { key: "last_name", label: "Last name" },
  { key: "title", label: "Job title" },
  { key: "company_name", label: "Company" },
  { key: "company_domain", label: "Company domain" },
  { key: "linkedin_url", label: "LinkedIn URL" },
  { key: "city", label: "City" },
  { key: "country_code", label: "Country code" },
  { key: "phone", label: "Phone" },
  { key: "language", label: "Language" },
];

/** The lead events a Google Sheets export can write. Keep in sync with export::EVENTS in backend/src/sheets/export.rs. */
export const SHEET_EVENTS = { hot_lead: "Hot leads", meeting_booked: "Booked meetings", "deal.won": "Won deals" } as const;
export type SheetEvent = keyof typeof SHEET_EVENTS;

/** A tab a project imports leads from, or writes lead events to. */
export interface SheetConnection {
  id: UUID;
  project_id: UUID;
  kind: "import" | "export";
  spreadsheet_id: string;
  spreadsheet_name: string;
  tab: string;
  /** {field: column header}, for an import. */
  mapping: Record<string, string>;
  segment_id: UUID | null;
  keep_importing: boolean;
  next_import_at: Ts | null;
  events: SheetEvent[];
  last_sync_at: Ts | null;
  /** Leads an import added, or rows an export wrote. */
  rows_added: number;
  error: string | null;
  failed_at: Ts | null;
  created_at: Ts;
}

/** GET /projects/:id/sheets. */
export interface SheetsStatus {
  /** Whether this Splee has a Google app to sign in with. */
  available: boolean;
  /** The Google account the project signed in with. Its tokens never come back. */
  account: { email: string; needs_reconnect: boolean; error: string | null; held_until: Ts | null } | null;
  connections: SheetConnection[];
}

/** The organization's bucket for the daily data export. The secret access key never comes back. */
export interface ExportSettings {
  /** Empty for AWS S3. */
  endpoint: string;
  region: string;
  bucket: string;
  prefix: string;
  path_style: boolean;
  access_key_id: string;
  tables: string[];
  include_bodies: boolean;
  updated_at: Ts;
}

export interface ExportRun {
  id: UUID;
  /** The UTC day the files are under, YYYY-MM-DD. */
  dt: string;
  trigger: "daily" | "manual";
  status: "running" | "done" | "failed";
  files: { table: string; key: string; rows: number; bytes: number }[];
  rows: number;
  bytes: number;
  error: string | null;
  started_at: Ts;
  finished_at: Ts | null;
}

/** GET /organization/exports, for admins. */
export interface ExportStatus {
  settings: ExportSettings | null;
  /** The last 7, newest first. */
  runs: ExportRun[];
  tables: { name: string; label: string }[];
  quiet_hour_utc: number;
  schema_version: number;
}

export interface DealAmounts {
  currency: string;
  open: number;
  weighted: number;
  won_this_month: number;
}

export interface Pipeline {
  /** What new deals are priced in. */
  currency: string;
  stages: DealStage[];
  /** Every open deal, and those that closed in the last 90 days. */
  deals: Deal[];
  forecast: {
    /** One row per currency, the organization's own first. */
    amounts: DealAmounts[];
    /** Won and lost in the last 90 days. */
    won: number;
    lost: number;
    win_rate: number | null;
  };
}

/** A question the agent answers about every company, a column on the Companies page's Research view. */
export interface ResearchQuestion {
  id: string;
  project_id: string;
  /** The column name. */
  label: string;
  question: string;
  answer_type: "yes_no" | "text";
  /** The email writer may use the answers. */
  use_in_emails: boolean;
  position: number;
  created_at: string;
}

/** A queued answer whose run stopped comes back as failed. */
export type ResearchStatus = "queued" | "answered" | "unknown" | "failed";

export interface ResearchAnswer {
  question_id: string;
  domain: string;
  status: ResearchStatus;
  /** "yes" or "no" for a yes or no question, else a short answer. Empty unless answered. */
  answer: string;
  evidence: string;
  source_url: string;
  updated_at: string;
}

export interface ResearchRow {
  id: string;
  domain: string;
  name: string;
  score: number;
  score_fit: number;
  score_intent: number;
  status: Company["status"];
  segment_name: string | null;
  /** By question id. */
  answers: Record<string, ResearchAnswer>;
}

export interface ResearchGrid {
  questions: ResearchQuestion[];
  items: ResearchRow[];
  total: number;
  /** Answers being researched across the project. */
  researching: number;
}

export interface ResearchStarted {
  companies: number;
  answers: number;
  /** Companies past the 200 a request can queue. */
  more: number;
  runs: number;
  /** What a company cost on average in the project's research runs so far. */
  cost_per_company_micros: number | null;
}

/** A lead page as the lead sees it at /for/{token}. */
export interface PublicLeadPage {
  headline: string;
  intro: string;
  points: { title: string; body: string }[];
  proof: string;
  first_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  sender_name: string;
  booking_url: string | null;
  reply_to: string | null;
}

/** A step of a deal's close plan. `side` is whose step it is, `done_by` who ticked it. */
export interface ClosePlanStep {
  id: UUID;
  title: string;
  detail: string;
  side: "us" | "them";
  due_date: string | null;
  done_at: Ts | null;
  done_by: "us" | "them" | null;
  /** Onboarding steps come after the win, from the handoff. */
  phase: "close" | "onboarding";
}

/** GET /deals/:id/plan. The buyer's page is `url`. Views don't count the team's own previews. */
export interface ClosePlan {
  id: UUID;
  token: string;
  url: string;
  goal: string | null;
  /** Set once the deal is won, when the plan carries on as the onboarding plan. */
  onboarding_goal: string | null;
  status: "writing" | "ready" | "failed";
  /** The plan the buyer sees: onboarding while the deal is won and the handoff wrote one. */
  phase: "close" | "onboarding";
  views: number;
  first_viewed_at: Ts | null;
  last_viewed_at: Ts | null;
  steps: ClosePlanStep[];
}

/** What the agent writes for whoever takes a won customer on. A promise's `quote` is word for word from the sale, or empty. */
export interface HandoffContent {
  summary: string;
  bought: string;
  promises: { promise: string; quote: string }[];
  people: { name: string; role: string; note: string }[];
  success: string[];
  risks: string[];
}

/** GET /deals/:id/handoff. */
export interface Handoff {
  id: UUID;
  status: "writing" | "ready" | "failed";
  handoff: HandoffContent | null;
  owner_id: UUID | null;
  owner_name: string | null;
  handed_by: string | null;
  handed_at: Ts | null;
  created_at: Ts;
}

/** A priced line of a proposal. `unit_price` is in the deal's currency's main unit. */
export interface ProposalItem {
  name: string;
  detail: string;
  quantity: number;
  unit_price: number;
  period: "one_time" | "month" | "year";
  /** The price list's product it was picked from. */
  product_id?: UUID;
  /** The product's price when saved. The backend sets it. */
  list_price?: number;
}

/** GET /deals/:id/proposal. The buyer's page is `url`. Once accepted it doesn't change. */
export interface Proposal {
  id: UUID;
  token: string;
  url: string;
  title: string | null;
  intro: string;
  sections: { title: string; body: string }[];
  items: ProposalItem[];
  currency: string;
  valid_until: string | null;
  status: "writing" | "ready" | "failed";
  views: number;
  first_viewed_at: Ts | null;
  last_viewed_at: Ts | null;
  accepted_at: Ts | null;
  accepted_name: string;
  accepted_title: string;
  /** The deepest discount off the price list, in percent. */
  discount_pct: number;
  discount_limit_pct: number | null;
  /** Past the limit with no approval covering it, so the buyer can't see the prices or accept. */
  held: boolean;
  approval: DiscountApproval | null;
}

/** GET /proposals/:token, what the buyer sees. `closed` means the deal ended another way. */
export interface PublicProposal {
  title: string;
  intro: string;
  sections: { title: string; body: string }[];
  items: ProposalItem[];
  currency: string;
  valid_until: string | null;
  accepted_at: Ts | null;
  accepted_name: string;
  closed: boolean;
  first_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  sender_name: string;
  /** Waiting on a discount approval: sections and items are empty until it's approved. */
  held: boolean;
}

/** GET /plans/:token, what the buyer sees. */
export interface PublicClosePlan {
  /** Empty when the plan only ever had an onboarding goal. */
  goal: string;
  onboarding_goal: string | null;
  /** The plan to show: onboarding while the deal is won, with the close steps as titles only. */
  phase: "close" | "onboarding";
  first_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  sender_name: string;
  booking_url: string | null;
  reply_to: string | null;
  steps: ClosePlanStep[];
}

/** The lead's page for the app. Views count people only, once per visit. */
export interface LeadPage {
  token: string;
  headline: string;
  url: string;
  views: number;
  first_viewed_at: string | null;
  last_viewed_at: string | null;
}

export type PlayTrigger = "funding" | "news" | "hiring" | "visit" | "job_change" | "score";
export type PlayAction = "add_to_campaign" | "write_email" | "notify";

/** A "when this happens, do that" rule the project runs every 5 minutes. */
export interface Play {
  id: string;
  project_id: string;
  name: string;
  trigger: PlayTrigger;
  action: PlayAction;
  segment_id: string | null;
  segment_name: string | null;
  min_score: number;
  daily_cap: number;
  enabled: boolean;
  checked_at: string;
  created_at: string;
  fired_today: number;
  fired_month: number;
}

export interface PlayFiring {
  id: string;
  play_id: string;
  play_name: string;
  domain: string | null;
  lead_id: string | null;
  what: string;
  outcome: "done" | "skipped" | "failed";
  note: string;
  run_id: string | null;
  created_at: string;
}

export interface PlaysOverview {
  plays: Play[];
  firings: PlayFiring[];
}

/** What the agent wrote for a call with a lead. See agent/briefs.rs. */
export interface MeetingBriefContent {
  summary: string;
  about: string[];
  so_far: string;
  signals: string[];
  talking_points: string[];
  questions: string[];
  watch_outs: string[];
}

/** A lead's latest meeting brief. `brief` is set once it's ready. */
export interface MeetingBrief {
  id: string;
  meeting_at: Ts | null;
  brief: MeetingBriefContent | null;
  written_at: Ts | null;
  status: "ready" | "writing" | "failed";
}

/** What the agent read in a campaign's replies. See agent/insights.rs. */
export interface CampaignInsights {
  summary: string;
  working: string[];
  /** `count` is how many replies give it, and `quote` is copied from one of them, or empty. */
  objections: { theme: string; count: number; quote: string; answer: string }[];
  who: string[];
  /** A new `value` for the campaign's angle or pain points, with what it said when the replies were read. */
  suggestions: { field: "angle" | "pain_points"; current: string; value: string; why: string }[];
}

export interface CampaignInsightsView {
  latest: {
    id: string;
    insights: CampaignInsights | null;
    replies: number;
    /** Indexes of the suggestions already used. */
    applied: number[];
    created_at: Ts;
    status: "ready" | "writing" | "failed";
  } | null;
  /** Replies there are to read now, and how many it takes. */
  replies: number;
  min_replies: number;
}

export type CommitteeRole = "decision_maker" | "champion" | "influencer" | "user" | "unknown";

export interface BuyingCommittee {
  summary: string;
  members: { lead_id: UUID; role: CommitteeRole; note: string }[];
  /** Roles nobody filled, as job titles to search for. */
  missing: { role: string; why: string }[];
}

export interface BuyingCommitteeView {
  people: { id: UUID; full_name: string; title: string; status: LeadStatus; created_at: Ts }[];
  latest: { id: UUID; committee: BuyingCommittee | null; found: number; created_at: Ts; status: "ready" | "working" | "failed" } | null;
}

/** See backend/src/agent/coaching.rs. A quote is empty when the transcript didn't have the words. */
export interface CallCoaching {
  score: number;
  next_step_agreed: boolean;
  next_step: string;
  strengths: string[];
  improve: { moment: string; quote: string; instead: string }[];
  covered: string[];
}

export interface CoachedCall {
  id: UUID;
  kind: "meeting" | "call";
  title: string;
  happened_at: Ts;
  duration_secs: number | null;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  owner_id: UUID | null;
  owner_name: string | null;
  /** Our share of the words, 0 to 100, or null when the transcript had no speakers. */
  talk_share: number | null;
  questions: number;
  coaching: CallCoaching | null;
  status: "ready" | "writing" | "failed";
}

export interface CoachingView {
  enabled: boolean;
  items: CoachedCall[];
}

/** How to sell against a competitor, written by the agent. See backend/src/agent/battlecards.rs. */
export interface Battlecard {
  summary: string;
  /** Empty when their pricing isn't public. */
  pricing: string;
  strengths: string[];
  wins: { point: string; evidence: string }[];
  losses: { point: string; evidence: string }[];
  /** Questions that bring out where you're stronger. */
  landmines: string[];
  objections: { they_say: string; we_say: string }[];
  /** Buyers' words, checked against their emails and calls. */
  quotes: { quote: string; who: string }[];
}

/** A competitor with its record in reviewed deals and its battlecard. status is null before one was asked for. */
export interface BattlecardItem {
  competitor_id: UUID;
  name: string;
  domain: string;
  won: number;
  lost: number;
  card: Battlecard | null;
  written_at: string | null;
  status: "writing" | "ready" | "failed" | null;
}

export interface BattlecardView {
  enabled: boolean;
  items: BattlecardItem[];
}

/** A time the agent looked for room to grow a won deal's account. See backend/src/agent/expansions.rs. */
export interface Expansion {
  id: UUID;
  deal_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  won_at: string | null;
  /** What changed at their company, or empty when someone asked. */
  signal: string;
  why: string;
  skipped: string;
  email_id: UUID | null;
  email_subject: string | null;
  email_body: string | null;
  email_status: EmailMessage["status"] | null;
  status: "writing" | "drafted" | "sent" | "skipped" | "failed";
  created_at: string;
}

export interface ExpansionView {
  enabled: boolean;
  items: Expansion[];
}

/** Asking a customer for introductions, with the page they forward. See backend/src/agent/referrals.rs. */
export interface Referral {
  id: UUID;
  deal_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  won_at: string | null;
  why: string;
  /** Companies like theirs the email names. */
  companies: { name: string; domain: string; why: string }[];
  skipped: string;
  email_id: UUID | null;
  email_subject: string | null;
  email_body: string | null;
  email_status: EmailMessage["status"] | null;
  status: "writing" | "drafted" | "sent" | "skipped" | "failed";
  token: string;
  views: number;
  last_viewed_at: string | null;
  /** Leads who left their details on the page. */
  referred: number;
  created_at: string;
}

export interface ReferralView {
  enabled: boolean;
  items: Referral[];
  referred: { lead_id: UUID; full_name: string; company_name: string; status: Lead["status"]; referral_id: UUID; created_at: string }[];
}

/** The page a customer forwards, at /r/:token. */
export interface PublicReferralPage {
  first_name: string;
  full_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  description: string;
  value_props: string[];
  sender_name: string;
  booking_url: string | null;
}

/** A deal's page for the buyer. See backend/src/deal_rooms.rs. */
export interface DealRoomContent {
  headline: string;
  intro: string;
  proof: { material_id: UUID; why: string }[];
}

export interface DealRoomProof {
  material_id: UUID;
  title: string;
  content: string;
  why: string;
}

export interface DealRoomQuestion {
  id: UUID;
  name: string;
  question: string;
  /** The agent's answer, for the team to check. */
  draft: string;
  /** What the team published. */
  answer: string | null;
  answered_at: string | null;
  drafting: boolean;
  created_at: string;
}

export interface DealRoom {
  id: UUID;
  deal_id: UUID;
  token: string;
  status: "writing" | "ready" | "failed";
  room: DealRoomContent | null;
  views: number;
  first_viewed_at: string | null;
  last_viewed_at: string | null;
  url: string;
  proof: DealRoomProof[];
  questions: DealRoomQuestion[];
}

export interface PublicDealRoom {
  room: DealRoomContent;
  first_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  sender_name: string;
  booking_url: string | null;
  plan: { goal: string; url: string; steps: ClosePlanStep[] } | null;
  proposal: { title: string; url: string; held: boolean; accepted: boolean } | null;
  proof: DealRoomProof[];
  questions: { name: string; question: string; answer: string; answered_at: string | null }[];
}

/** Where a quote came from: a reply, a meeting transcript, a win/loss review or a deal room question. */
export type FeedbackSource = "email" | "meeting" | "review" | "room";

export interface FeedbackMention {
  id: string;
  lead_id: string;
  name: string;
  company: string;
  source: FeedbackSource;
  quote: string;
  said_at: string;
  deal_id: string | null;
  deal_status: "open" | "won" | "lost" | null;
}

/** Something buyers asked the product to do, with the deals of the people who asked in the organization's currency. */
export interface FeedbackRequest {
  id: string;
  title: string;
  detail: string;
  status: "open" | "planned" | "shipped" | "declined";
  shipped_note: string;
  shipped_at: string | null;
  created_at: string;
  people: number;
  companies: number;
  open_cents: number;
  lost_cents: number;
  won_cents: number;
  last_said_at: string | null;
  /** While the agent drafts the emails saying it shipped. */
  announcing: boolean;
  drafted: number;
  sent: number;
  skipped: number;
  /** The newest 25. */
  mentions: FeedbackMention[];
}

export interface FeedbackView {
  currency: string;
  scan: { status: "scanning" | "idle"; scanned_at: string | null };
  requests: FeedbackRequest[];
}

/** One thing the product is worth to the buyer: quantity units, each worth unit_value, every per, for count count_units. */
export interface CaseLine {
  label: string;
  quantity: number;
  unit: string;
  unit_value: number;
  per: "week" | "month" | "year";
  count: number;
  count_unit: string;
  /** said when the buyer gave the number, estimate when the agent did. */
  basis: "said" | "estimate";
  source: string;
}

export type CaseNumbers = Pick<CaseLine, "quantity" | "unit_value" | "count">;

export interface BusinessCaseContent {
  title: string;
  summary: string;
  problem: string;
  lines: CaseLine[];
  outcomes: string[];
  waiting: string;
}

/** The first year of the proposal once the buyer can read it, else the deal's value. */
export interface CaseInvestment {
  amount: number;
  currency: string;
  from: "proposal" | "deal";
}

export interface BusinessCase {
  id: string;
  deal_id: string;
  token: string;
  url: string;
  /** ready can carry `skipped` when the agent declined to write it again. */
  status: "writing" | "ready" | "skipped" | "failed";
  business_case: BusinessCaseContent | null;
  skipped: string;
  /** The buyer's own numbers, one per line. */
  numbers: CaseNumbers[] | null;
  edited_by: string;
  edited_at: string | null;
  views: number;
  first_viewed_at: string | null;
  last_viewed_at: string | null;
  investment: CaseInvestment | null;
}

export interface PublicBusinessCase {
  business_case: BusinessCaseContent;
  /** Which writing of the case this is. Saving numbers sends it back. */
  revision: string;
  numbers: CaseNumbers[] | null;
  edited_by: string;
  edited_at: string | null;
  first_name: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  sender_name: string;
  investment: CaseInvestment | null;
}

/** A case study the agent drafted from a won deal. The sections never name the customer. See backend/src/agent/stories.rs. */
export interface StoryDraft {
  title: string;
  anonymous_title: string;
  anonymous_name: string;
  summary: string;
  challenge: string;
  solution: string;
  results: string;
  quote: string;
}

export interface CustomerStory {
  id: UUID;
  deal_id: UUID;
  lead_name: string;
  company_name: string;
  status: "writing" | "skipped" | "failed" | "drafted" | "written" | "sent" | "approved" | "declined";
  story: StoryDraft | null;
  skipped: string;
  token: string;
  email_id: UUID | null;
  email_subject: string | null;
  email_body: string | null;
  views: number;
  last_viewed_at: string | null;
  approved_at: string | null;
  approved_name: string;
  approved_title: string;
  named: boolean | null;
  declined_at: string | null;
  material_id: UUID | null;
  created_at: string;
}

/** The page a customer approves their case study on. */
export interface PublicStoryPage {
  first_name: string;
  full_name: string;
  job_title: string;
  company: string;
  company_domain: string | null;
  from_name: string;
  from_domain: string | null;
  story: StoryDraft;
  status: "waiting" | "approved" | "declined";
  named: boolean | null;
  approved_name: string;
  approved_title: string;
}

/** A check on whether a customer might leave. See backend/src/agent/churn.rs. */
export interface ChurnRisk {
  id: UUID;
  deal_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  won_at: string | null;
  /** The warning signs that started it, or empty when someone asked. */
  signal: string;
  risk: "high" | "medium" | "low" | null;
  why: string;
  /** What the team should do. */
  play: string;
  /** Why there's no email for a risk, when there isn't one. */
  note: string;
  skipped: string;
  email_id: UUID | null;
  email_subject: string | null;
  email_body: string | null;
  email_status: EmailMessage["status"] | null;
  status: "writing" | "done" | "skipped" | "failed";
  created_at: string;
}

export interface ChurnRiskView {
  enabled: boolean;
  /** Won deals in the project. */
  customers: number;
  items: ChurnRisk[];
}

/** A won deal on the Customers page. See backend/src/routes/customers.rs. */
export interface Customer {
  deal_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  company_domain: string | null;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  won_at: string | null;
  /** Paid in the last 12 months in the deal's currency, refunds taken off. */
  revenue_cents: number;
  last_paid_at: string | null;
  last_contact_at: string | null;
  /** From the latest check on whether they might leave. */
  risk: ChurnRisk["risk"];
  risk_why: string | null;
  /** The opening behind an expansion draft waiting to be sent. */
  growth: string | null;
  /** Whether the referral ask went out. */
  asked: boolean;
  page_views: number;
  referred: number;
}

/** A practice call on an open deal, with the agent playing the buyer. See backend/src/agent/practice.rs. */
export interface PracticeSession {
  id: UUID;
  deal_id: UUID;
  scenario: "next_call" | "pricing" | "competitor";
  /** Only name, title and company until it's scored. */
  persona: {
    name: string;
    title: string;
    company: string;
    situation?: string;
    cares_about?: string[];
    objections?: string[];
    agrees_if?: string;
  } | null;
  turns: { who: "buyer" | "rep"; text: string }[];
  ended: boolean;
  score: {
    score: number;
    summary: string;
    went_well: string[];
    try_next: string[];
    moments: { you_said: string; try: string }[];
    /** How they did on the skill, when they practiced an assignment. */
    skill?: string;
  } | null;
  user_name: string | null;
  status: "preparing" | "thinking" | "scoring" | "live" | "ended" | "scored" | "failed";
  error: string | null;
  created_at: string;
}

/** A teammate's month against their target, or the deals nobody owns. See backend/src/routes/targets.rs. */
export interface TargetRow {
  user_id: UUID | null;
  name: string;
  target_cents: number | null;
  won_cents: number;
  won_deals: number;
  /** Open deals the latest forecast puts in commit or best case this month. */
  commit_cents: number;
  best_case_cents: number;
  deals: { deal_id: UUID; name: string; company: string; value_cents: number; category: "commit" | "best_case"; why: string }[];
}

/** What the capacity plan assumes, as the query changes it and as history gives it (null when there's too little). See
 * backend/src/routes/capacity.rs. */
export interface CapacityAssumptions {
  win_rate: number | null;
  deal_cents: number | null;
  cycle_days: number | null;
  deals_per_rep: number | null;
  ramp_months: number | null;
  target_cents: number | null;
  growth_pct: number | null;
}

export interface Capacity {
  currency: string;
  reps: number;
  history: CapacityAssumptions;
  used: { [K in keyof CapacityAssumptions]: number };
  plan: {
    per_rep_cents: number;
    months: { month: string; target_cents: number; team_cents: number; hires_cents: number }[];
    hires: { start_by: string; full_speed: string; closing_from: string }[];
    short: string[];
  };
}

/** GET /commissions?month=: what each teammate earned on the deals they won that month. See backend/src/routes/commissions.rs. */
export interface Commissions {
  /** The first of the month, like 2026-10-01. */
  month: string;
  currency: string;
  /** Null when there's no team rate. */
  commission_bps: number | null;
  /** 150 earns 1.5× the rate past target. */
  accelerator_pct: number;
  rows: {
    user_id: UUID;
    name: string;
    target_cents: number | null;
    rate_bps: number | null;
    own_rate: boolean;
    won_cents: number;
    earned_cents: number;
    deals: { deal_id: UUID; name: string; company: string; closed_at: Ts; value_cents: number; earned_cents: number; accelerated: boolean }[];
  }[];
}

export interface Targets {
  currency: string;
  day: number;
  days: number;
  rows: TargetRow[];
}

/** A teammate's 1:1 prep. `month` is their Targets row when it was written. See backend/src/agent/one_on_one.rs. */
export interface OneOnOnePrep {
  headline: string;
  month: { currency: string; day: number; days: number; target_cents: number | null; won_cents: number; commit_cents: number; best_case_cents: number };
  recognize: string[];
  deals: { deal_id: UUID; company: string; why: string; ask: string }[];
  skill: {
    name: string;
    evidence: string;
    drill: string;
    /** The practice call the drill is, when it's one on their open deals. */
    practice?: { deal_id: UUID; company: string; scenario: PracticeSession["scenario"] };
  };
  commitment: string;
}

/** A practice call a 1:1 prep assigned, with the latest scored session, else the latest started. See ASSIGNED in backend/src/agent/practice.rs. */
export interface PracticeAssignment {
  id: UUID;
  deal_id: UUID;
  project_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company: string;
  scenario: PracticeSession["scenario"];
  skill: string;
  drill: string;
  /** The score that passes it, for a ramp's. A 1:1's is done once it's scored. */
  pass_score: number | null;
  ramp_id: UUID | null;
  assigned_by: string | null;
  created_at: Ts;
  /** Whether the deal is still open, so it can be practiced. */
  open: boolean;
  session_id: UUID | null;
  score: NonNullable<PracticeSession["score"]> | null;
}

/** A teammate's ramp plan, written from the project's playbook. See backend/src/agent/ramp.rs. */
export interface RampPlan {
  summary: string;
  weeks: {
    title: string;
    goal: string;
    steps: string[];
    practice: { deal_id: UUID; company: string; scenario: PracticeSession["scenario"]; skill: string; drill: string }[];
  }[];
}

export interface Ramp {
  id: UUID;
  user_id: UUID;
  name: string;
  plan: RampPlan | null;
  requested_by: string | null;
  created_at: Ts;
  status: "ready" | "writing" | "failed";
  error: string | null;
  practice: PracticeAssignment[];
}

/** GET /projects/:id/ramps: each teammate's latest for owners and admins, only your own otherwise. */
export interface RampsView {
  ramps: Ramp[];
  pass_score: number;
}

export interface OneOnOneView {
  latest: {
    id: UUID;
    prep: OneOnOnePrep | null;
    requested_by: string | null;
    created_at: Ts;
    status: "ready" | "writing" | "failed";
    error: string | null;
  } | null;
  /** The latest prep's practice call, once it's assigned. */
  assignment: PracticeAssignment | null;
}

/** A customer's next renewal and the agent's prep for it. See backend/src/agent/renewals.rs. */
export interface Renewal {
  deal_id: UUID;
  lead_id: UUID;
  lead_name: string;
  company_name: string;
  owner_name: string | null;
  value_cents: number;
  currency: string;
  won_at: Ts | null;
  /** YYYY-MM-DD. Null only for a won deal without a close date. */
  renews_on: string | null;
  /** The date is the anniversary of the win, not one the team set. */
  assumed: boolean;
  id: UUID | null;
  risk: "high" | "medium" | "low" | null;
  why: string;
  steps: string[];
  note: string;
  skipped: string;
  email_id: UUID | null;
  email_subject: string | null;
  email_body: string | null;
  email_status: EmailStatus | null;
  /** later: not prepped yet. */
  status: "later" | "writing" | "failed" | "skipped" | "done";
}

export interface RenewalView {
  enabled: boolean;
  ahead_days: number;
  items: Renewal[];
}

/** A project's sales playbook. See backend/src/agent/playbook.rs. */
export interface Playbook {
  summary: string;
  fit: string[];
  avoid: string[];
  pitch: string;
  discovery: { question: string; why: string }[];
  objections: { they_say: string; we_say: string }[];
  competitors: { name: string; win_by: string }[];
  emails: { lesson: string; example: string }[];
  winning: string[];
  pitfalls: string[];
}

export interface PlaybookSources {
  won: number;
  lost: number;
  battlecards: number;
  campaigns: number;
  calls: number;
  emails: number;
  profile: boolean;
}

export interface PlaybookView {
  latest: {
    id: UUID;
    playbook: Playbook | null;
    sources: PlaybookSources;
    created_at: Ts;
    status: "ready" | "writing" | "failed";
    error: string | null;
  } | null;
  available: PlaybookSources;
  min_sources: number;
}

/** The Monday team brief: who needs what this week, in the order to talk to them. See backend/src/agent/team_brief.rs. */
export interface TeamBrief {
  headline: string;
  people: { user_id: UUID; name: string; note: string; action: string }[];
  risks: string[];
}

/** GET /team-brief, owners and admins only. */
export interface TeamBriefView {
  latest: {
    id: UUID;
    brief: TeamBrief | null;
    /** A Monday one, emailed to owners and admins. */
    auto: boolean;
    created_at: Ts;
    status: "ready" | "writing" | "failed";
    error: string | null;
  } | null;
  /** Whether one is written and emailed every Monday. */
  weekly: boolean;
}

export type ContestMetric = "won_value" | "deals_won" | "pipeline" | "calls" | "practice";

/** A sales contest. Everyone in the organization is in it. See backend/src/contests.rs. */
export interface Contest {
  id: UUID;
  name: string;
  metric: ContestMetric;
  prize: string;
  starts_at: Ts;
  ends_at: Ts;
  created_by: string | null;
  /** Every member, the highest first. `value` is cents for won_value and pipeline, a count otherwise. */
  standings: { user_id: UUID; name: string; value: number }[];
}

/** GET /contests: running ones, the soonest to end first, then those that ended in the last 30 days. */
export interface ContestsView {
  currency: string;
  contests: Contest[];
}

/** A product on the organization's price list, priced in its currency. */
export interface Product {
  id: UUID;
  name: string;
  detail: string;
  price: number;
  period: ProposalItem["period"];
}

/** GET /products. Proposals past `discount_limit_pct` off the list need an owner or admin's approval. */
export interface PriceList {
  currency: string;
  discount_limit_pct: number | null;
  products: Product[];
}

/** An ask to approve a proposal's discount, with the agent's read. See backend/src/deal_desk.rs. */
export interface DiscountApproval {
  id: UUID;
  deal_id: UUID;
  project_id: UUID;
  lead_name: string;
  company: string;
  discount_pct: number;
  discount_limit_pct: number | null;
  reason: string;
  asked_by: string | null;
  created_at: Ts;
  read: { verdict: "approve" | "counter" | "decline"; counter_pct: number | null; summary: string; reasons: string[]; ask_for: string } | null;
  read_status: "writing" | "ready" | "failed";
  status: "asked" | "approved" | "declined";
  decided_by: string | null;
  decided_at: Ts | null;
  note: string;
}

/** A buyer's questionnaire, with how far along it is. `unknown` counts answers the agent couldn't source and nobody approved. */
export interface QuestionnaireSummary {
  id: string;
  name: string;
  created_at: string;
  answering: boolean;
  questions: number;
  answered: number;
  approved: number;
  unknown: number;
}

export interface QuestionnaireList {
  questionnaires: QuestionnaireSummary[];
  /** Approved answers the next questionnaire starts from. */
  library: number;
}

export interface QuestionnaireItem {
  id: string;
  /** Its row in the file, counting from 0 below the header. */
  position: number;
  /** Which question it is, from 1, skipping the rows that aren't questions. */
  number: number;
  question: string;
  section: string;
  answer: string;
  /** library when an approved answer to the same question filled it. Null before it's answered. */
  confidence: "library" | "sourced" | "partial" | "unknown" | null;
  sources: string[];
  approved_at: string | null;
}

export interface QuestionnaireDetail extends QuestionnaireSummary {
  items: QuestionnaireItem[];
}
