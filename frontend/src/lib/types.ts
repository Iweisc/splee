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
  created_at: Ts;
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
  /** Texts after the first email, through the organization's Twilio account. */
  text_steps: TextStep[];
  /** Texts sent in any 24 hours. Replies count toward it but are never held back. */
  daily_text_limit: number;
  /** Who gets a lead that has no owner when it first replies, books or is captured by the chat. */
  assign_rule: AssignRule;
  /** Round robin's members, in the order it goes through them. */
  assign_members: UUID[];
}

export type AssignRule = "off" | "round_robin" | "mailbox";

/** GET /projects/:id/autopilot. Spend and leads count the current calendar month (UTC). */
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
  | "deal.lost";

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
export type HookEvent = "hot_lead" | "lead_replied" | "meeting_booked" | "status_changed" | "deal.won" | "lead_added";

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
  source: "agent" | "import" | "api" | "form" | "linkedin" | "google_maps" | "chat";
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
  /** Sales meetings with them from Fireflies, Fathom, Gong or Zoom, oldest first. */
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
  /** meeting_follow_up is the email the agent drafts after a sales meeting. */
  kind: "initial" | "follow_up" | "auto_reply" | "manual_reply" | "inbound" | "meeting_follow_up" | "deal_nudge";
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
}

export type TextChannel = "sms" | "whatsapp";

/** A text Splee sends through the organization's Twilio account: a sequence step, or a reply typed in the inbox. */
export interface TextMessage {
  id: UUID;
  project_id: UUID;
  lead_id: UUID;
  segment_id: UUID | null;
  channel: TextChannel;
  kind: "sequence" | "reply";
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
  /** Twilio's URL. Play it through /api/calls/{id}/recording. */
  recording_url: string;
  recording_secs: number | null;
  outcome: CallOutcome | null;
  notes: string;
  transcript: string;
  transcribed_at: Ts | null;
  summary: string;
  next_step: string;
  created_at: Ts;
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
  Pick<BookingPage, "slug" | "name" | "duration_minutes" | "timezone" | "hours" | "buffer_minutes" | "notice_minutes" | "days_ahead" | "location">
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
  | "deal_review";

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
}

export interface TodayPart<T> {
  total: number;
  items: T[];
}

/** In the order the integrations page offers them. Keep in sync with Provider in backend/src/meetings/mod.rs. */
export const MEETING_PROVIDERS = ["fireflies", "fathom", "gong", "zoom"] as const;
export type MeetingProvider = (typeof MEETING_PROVIDERS)[number];
export const MEETING_NAMES: Record<MeetingProvider, string> = { fireflies: "Fireflies", fathom: "Fathom", gong: "Gong", zoom: "Zoom" };

/** GET /projects/:id/meeting-connections. Keys and secrets never come back. */
export interface MeetingConnection {
  provider: MeetingProvider;
  /** The secret in the webhook URL, /api/meeting-webhooks/{provider}/{webhook_token}. Gong has no webhook. */
  webhook_token: string;
  has_secret: boolean;
  last_event_at: Ts | null;
  /** Gong's last read, every 15 minutes, and why it failed. */
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
}

/** GET /deals/:id/plan. The buyer's page is `url`. Views don't count the team's own previews. */
export interface ClosePlan {
  id: UUID;
  token: string;
  url: string;
  goal: string | null;
  status: "writing" | "ready" | "failed";
  views: number;
  first_viewed_at: Ts | null;
  last_viewed_at: Ts | null;
  steps: ClosePlanStep[];
}

/** A priced line of a proposal. `unit_price` is in the deal's currency's main unit. */
export interface ProposalItem {
  name: string;
  detail: string;
  quantity: number;
  unit_price: number;
  period: "one_time" | "month" | "year";
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
}

/** GET /plans/:token, what the buyer sees. */
export interface PublicClosePlan {
  goal: string;
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
