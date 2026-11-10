// Mirrors backend/src/models.rs and the response shapes in SPEC.md. Change them together.

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
}

export interface Member {
  user_id: UUID;
  email: string;
  name: string;
  role: Role;
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

export interface NotificationPrefs {
  user_id: UUID;
  hot_lead_email: boolean;
  daily_digest: boolean;
  weekly_report: boolean;
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
}

export interface Targeting {
  industries?: string[];
  roles?: string[];
  company_sizes?: string[];
  countries?: string[];
  exclude_countries?: string[];
  keywords?: string[];
  exclude_keywords?: string[];
}

/** Every CRM, in the order the integrations page lists them. Keep in sync with Crm::ALL in backend/src/crm/mod.rs. */
export const CRM_NAMES = ["hubspot", "pipedrive", "attio", "close", "folk", "capsule", "brevo", "zendesk_sell", "lacrm", "salesflare", "affinity", "freshsales", "copper", "insightly", "twenty", "twenty_self", "espocrm", "odoo", "notion", "airtable", "baserow", "nocodb", "grist", "teable"] as const;
export type CrmName = (typeof CRM_NAMES)[number];

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
  /** Looks up the real address behind a guessed one. Its key is write-only, through PUT /integrations/finder. */
  finder: "" | "hunter" | "findymail" | "prospeo" | "anymailfinder" | "leadmagic" | "apollo" | "voilanorbert" | "skrapp" | "lusha";
  finder_error: string | null;
  finder_failed_at: string | null;
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
  /** Keeps working campaigns stocked with written emails within the monthly budget and lead cap. */
  autopilot: boolean;
  /** Claude spend a calendar month (UTC) may reach, counting runs started by hand. */
  autopilot_budget_micros: number;
  /** New leads emailed a month. Past it, first emails wait for the next month while autopilot is on. */
  autopilot_lead_cap: number;
  /** Autopilot's emails go out without approval. */
  autopilot_auto_send: boolean;
}

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
  | "follow_up_due"
  | "form_submitted"
  | "status_changed";

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

export interface Company {
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
}

export interface DirectoryPage extends Paginated<DirectoryCompany> {
  /** `total` stopped counting at 10,000 and there are more. */
  capped: boolean;
}

export interface DirectoryCountry {
  country_code: string;
  companies: number;
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

export interface Lead {
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
  company_name: string;
  company_domain: string | null;
  city: string;
  country_code: string;
  source: "agent" | "import" | "api" | "form";
  status: LeadStatus;
  personalization: { hook?: string; facts?: string[] };
  notes: string;
  remind_at: Ts | null;
  /** When the call they booked starts, if their scheduling tool said. */
  meeting_at: Ts | null;
  last_contacted_at: Ts | null;
  created_at: Ts;
}

export interface LeadRow extends Lead {
  segment_name: string | null;
  last_email_status: string | null;
}

export interface LeadDetail extends Lead {
  segment_name: string | null;
  company: Company | null;
  emails: EmailMessage[];
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
  kind: "initial" | "follow_up" | "auto_reply" | "manual_reply" | "inbound";
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
}

export interface EmailRow extends EmailMessage {
  lead_name: string;
  lead_email: string | null;
  company_name: string;
  segment_name: string | null;
  project_name: string;
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

export interface Mailbox {
  id: UUID;
  org_id: UUID;
  email: string;
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
  | "social";

/** A reply or post for a social platform, drafted by a social run or written by hand, posted once approved. */
export interface SocialPost {
  id: UUID;
  project_id: UUID;
  run_id: UUID | null;
  /** LinkedIn posts go out through the project's Unipile connection. */
  platform: "x" | "reddit" | "bluesky" | "mastodon" | "linkedin";
  /** The link of the post or comment it answers, null for an original post. */
  in_reply_to: string | null;
  /** The text it answers. */
  context: string;
  /** Where a Reddit post goes, or the subreddit of the thread it answers. */
  subreddit: string;
  /** Reddit posts only. */
  title: string;
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
