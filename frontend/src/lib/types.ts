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

export interface ProjectSettings {
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
  webhook_url: string;
  slack_webhook_url: string;
  webhook_secret: string;
  webhook_error: string | null;
  webhook_failed_at: string | null;
  slack_error: string | null;
  slack_failed_at: string | null;
  /** Posts the last 7 days' numbers to the chat channel every Monday. */
  chat_weekly_report: boolean;
  paused: boolean;
  webhook_events: WebhookEvent[];
  /** The secret in the booking webhook URL, /api/bookings/{booking_token}. */
  booking_token: string;
  /** The secret in the reminders calendar feed URL, /api/calendar/{calendar_token}.ics. */
  calendar_token: string;
  /** The secret in the form webhook URL, /api/forms/{form_token}. */
  form_token: string;
  /** CRM tokens are write-only, through PUT /integrations/{crm}. */
  hubspot_connected: boolean;
  hubspot_error: string | null;
  hubspot_failed_at: string | null;
  pipedrive_connected: boolean;
  pipedrive_error: string | null;
  pipedrive_failed_at: string | null;
  attio_connected: boolean;
  attio_error: string | null;
  attio_failed_at: string | null;
  close_connected: boolean;
  close_error: string | null;
  close_failed_at: string | null;
  /** Its key is write-only, through PUT /integrations/verifier. */
  verifier: "" | "neverbounce" | "millionverifier" | "hunter";
  verifier_error: string | null;
  verifier_failed_at: string | null;
}

export type WebhookEvent =
  | "hot_lead"
  | "lead_replied"
  | "meeting_booked"
  | "meeting_cancelled"
  | "lead_unsubscribed"
  | "lead_bounced"
  | "email_sent"
  | "follow_up_due";

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
  /** When the call they booked starts, if Calendly or Cal.com said. */
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
  dns: { mx?: DnsCheck; spf?: DnsCheck; dmarc?: DnsCheck; checked_at?: Ts };
  created_at: Ts;
  recheck_at: Ts | null;
  ramp_up: boolean;
  /** "2026-11-09", the UTC day it first sent. */
  first_sent_on: string | null;
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
  | "segment_from_prompt";

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
