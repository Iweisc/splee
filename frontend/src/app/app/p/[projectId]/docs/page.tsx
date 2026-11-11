"use client";

import { useParams } from "next/navigation";
import { KeyRound } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CodeBlock } from "@/components/app/settings/code-block";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { MicroLabel } from "@/components/app/work/states";
import { LinkButton } from "@/components/ui/button";
import { Badge, type BadgeTone } from "@/components/ui/misc";
import { useOrigin } from "@/hooks/use-origin";

interface Endpoint {
  method: "GET" | "POST" | "PATCH" | "DELETE";
  path: string;
  summary: string;
  params?: [name: string, description: string][];
  body?: string;
  response: string;
}

const LEAD = `{
  "id": "c42e…",
  "project_id": "9b1f…",
  "segment_id": "5d0a…",
  "full_name": "Sarah Chen",
  "first_name": "Sarah",
  "last_name": "Chen",
  "title": "Founder",
  "email": "sarah@northwind.studio",
  "email_status": "found",
  "company_name": "Northwind Studio",
  "company_domain": "northwind.studio",
  "linkedin_url": "https://www.linkedin.com/in/sarah-chen",
  "x_url": "https://x.com/sarahchen",
  "city": "Portland",
  "country_code": "US",
  "source": "agent",
  "status": "contacted",
  "notes": "",
  "last_contacted_at": "2026-11-03T09:14:00Z",
  "created_at": "2026-11-02T16:40:00Z",
  "score": 78,
  "score_fit": 43,
  "score_intent": 35,
  "score_reasons": [
    { "text": "Hiring 2 SDRs", "kind": "intent" },
    { "text": "Read /pricing yesterday", "kind": "intent" },
    { "text": "Uses a competitor: Outreach", "kind": "against" }
  ],
  "scored_at": "2026-11-11T08:20:00Z"
}`;

const METHOD_TONES: Record<Endpoint["method"], BadgeTone> = { GET: "blue", POST: "green", PATCH: "amber", DELETE: "red" };

const indent = (json: string, spaces: number) => json.replaceAll("\n", `\n${" ".repeat(spaces)}`);

function endpoints(projectId: string): Endpoint[] {
  return [
    {
      method: "GET",
      path: "/projects",
      summary: "Projects in your organization.",
      response: `[
  {
    "id": "${projectId}",
    "name": "Acme",
    "domain": "acme.com",
    "status": "active",
    "created_at": "2026-11-01T12:00:00Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/campaigns`,
      summary: "Campaigns of a project with their funnel. spend_micros is in micro-dollars (1,000,000 = $1).",
      response: `[
  {
    "id": "5d0a…",
    "name": "Design studios",
    "description": "Independent design studios in the US",
    "status": "working",
    "companies": 40,
    "leads": 72,
    "sent": 150,
    "contacted": 70,
    "replies": 6,
    "bounced": 1,
    "hot": 2,
    "spend_micros": 4210000,
    "created_at": "2026-11-01T12:05:00Z"
  }
]`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/campaigns`,
      summary:
        "Create a campaign from a description of who to reach, like describing one in the app. The agent names it and sets its targeting in a run you can follow with GET /runs below.",
      body: `{ "prompt": "Dental clinics in Leeds" }`,
      response: `{ "campaign_id": "5d0a…", "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: "/companies?q=dental&city=Leeds&country=GB",
      summary:
        "Search the company database, the most reliable listings first. q matches words at the start of words in the name or category. total stops counting at 10,000, and capped says there are more.",
      params: [
        ["q", "optional words in the name or category"],
        ["country", "optional two-letter code, like GB"],
        ["city", "optional city, any case"],
        ["category", "optional exact category, like dental_clinic"],
        ["has_email, has_phone", "optional true to keep only companies that list one"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "domain": "smiledental.co.uk",
      "name": "Smile Dental",
      "website": "https://smiledental.co.uk",
      "phone": "+44 113 000 0000",
      "email": "hello@smiledental.co.uk",
      "city": "Leeds",
      "country_code": "GB",
      "category": "dental_clinic",
      "…": "…"
    }
  ],
  "total": 125,
  "capped": false
}`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/companies",
      summary:
        "Add companies from the search above to a campaign by domain, up to 500 at a time. Companies the project already has and ones on your do-not-contact list are skipped.",
      body: `{ "domains": ["smiledental.co.uk", "brightsmiles.co.uk"] }`,
      response: `{ "added": 2, "skipped": 0 }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/companies?sort=score&min_score=50`,
      summary:
        "The project's own companies, best research fit first, with their hiring roles and tools. A company's score is its best lead's, or its own while it has no leads.",
      params: [
        ["segment_id", "optional campaign id"],
        ["q", "optional text in the name or domain"],
        ["min_score", "optional, only scores of at least this, 0 to 100"],
        ["sort", "optional, score for the highest first, or newest"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "e810…",
      "domain": "northwind.studio",
      "name": "Northwind Studio",
      "employees": "11-50",
      "country_code": "US",
      "status": "people_found",
      "score": 78,
      "score_fit": 43,
      "score_intent": 35,
      "score_reasons": [{ "text": "Hiring 2 SDRs", "kind": "intent" }],
      "hiring": ["SDR"],
      "tech": ["HubSpot", "Stripe"]
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/find-people",
      summary:
        "Have the agent find decision makers and their emails at the campaign's new companies, like Find people in the app. Send company_ids to pick some, or no body for all.",
      body: `{ "company_ids": ["e810…"] }`,
      response: `{ "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/leads?status=hot&page=1`,
      summary:
        "Leads of a project, newest first, with their campaign and the status of the last email to them. score is 0-100: score_fit (0-50) is how well the company matches your targeting and score_intent (0-50) what it did lately, with the reasons strongest first.",
      params: [
        ["status", "optional lead status: new, contacted, replied, hot, meeting_booked, …"],
        ["segment_id", "optional campaign id"],
        ["q", "optional text in the name, email, company or title"],
        ["min_score", "optional, only scores of at least this, 0 to 100"],
        ["sort", "optional, score for the highest first, or newest"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    ${indent(LEAD, 4)}
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/leads`,
      summary:
        "Add up to 1,000 leads per request. Suppressed, duplicate and invalid emails are skipped, and errors lists the first 200 of them with their zero-based index in leads.",
      body: `{
  "leads": [
    {
      "email": "sarah@northwind.studio",
      "full_name": "Sarah Chen",
      "title": "Founder",
      "company_name": "Northwind Studio",
      "company_domain": "northwind.studio",
      "linkedin_url": "https://www.linkedin.com/in/…",
      "segment_id": "5d0a…"
    },
    {
      "email": "sarah@northwind.studio",
      "full_name": "Sarah Chen"
    }
  ]
}`,
      response: `{
  "imported": 1,
  "skipped": 1,
  "errors": [{ "row": 1, "error": "Duplicate: sarah@northwind.studio" }]
}`,
    },
    {
      method: "POST",
      path: `/projects/${projectId}/leads/upsert`,
      summary:
        "Add one lead, or update the project's lead with that email. Fields you leave out or send empty keep what the lead has, and segment_id moves it into that campaign. created says which happened. 400 for a missing or invalid email, one on your do-not-contact list, or a campaign of another project.",
      body: `{ "email": "sarah@northwind.studio", "title": "CEO", "segment_id": "5d0a…" }`,
      response: `{ "created": false, "id": "c42e…", "email": "sarah@northwind.studio", "title": "CEO", "…": "…" }`,
    },
    {
      method: "POST",
      path: "/segments/5d0a…/write-emails",
      summary:
        "Have the agent write a first email for every lead in a campaign that doesn't have one yet, like Write emails in the app. Add leads with segment_id above, then call this. In a campaign that's sending, the new emails are approved straight away. 409 when every lead already has an email. The body is optional: lead_ids writes for just those leads, and instructions guides this batch.",
      body: `{ "instructions": "Mention our Leeds office" }`,
      response: `{ "run_id": "9b4f…" }`,
    },
    {
      method: "GET",
      path: "/runs/9b4f…",
      summary:
        "A run's progress, for example one started by write-emails. status goes from queued to running, then succeeded, failed or cancelled. message is its latest progress line, error says why a run failed, and cost_usd is what Claude Code reported for it.",
      response: `{
  "id": "9b4f…",
  "kind": "write_emails",
  "status": "succeeded",
  "message": "Wrote 12 emails",
  "error": "",
  "cost_usd": 0.42,
  "created_at": "2026-11-04T10:00:00Z",
  "finished_at": "2026-11-04T10:03:12Z",
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: "/leads?email=sarah@northwind.studio",
      summary:
        "Find leads by email across all your projects, for tools that only know the address, such as a calendar booking. Newest first, up to 100. Then use the id with PATCH below.",
      params: [["email", "the address to look up, any case"]],
      response: `[
  ${indent(LEAD, 2)}
]`,
    },
    {
      method: "GET",
      path: "/leads/c42e…",
      summary: "One lead with its campaign, company and every email in both directions, oldest first.",
      response: `${LEAD.slice(0, -2)},
  "segment_name": "Design studios",
  "company": { "id": "e810…", "domain": "northwind.studio", "name": "Northwind Studio", "fit_score": 86, "…": "…" },
  "emails": [{ "id": "a19c…", "direction": "outbound", "kind": "initial", "subject": "Northwind Studio x Acme", "status": "sent", "…": "…" }]
}`,
    },
    {
      method: "PATCH",
      path: "/leads/c42e…",
      summary:
        "Set a lead's status, for example meeting_booked when your calendar books a call or not_interested from your CRM, or replace its notes, the ones your team sees in the inbox. Send either or both. Follow-ups only go to contacted leads, so any other status stops them. Returns the lead like GET.",
      params: [
        ["status", "new, queued, contacted, replied, hot, meeting_booked, not_interested, bounced, unsubscribed or do_not_contact"],
        ["notes", "up to 5,000 characters, replacing the lead's notes. An empty string clears them."],
      ],
      body: `{ "status": "meeting_booked", "notes": "Booked for Tuesday 2pm by Calendly." }`,
      response: `{
  "id": "c42e…",
  "status": "meeting_booked",
  "notes": "Booked for Tuesday 2pm by Calendly.",
  "…": "…"
}`,
    },
    {
      method: "GET",
      path: "/suppressions?since=2026-11-01T00:00:00Z&page=1",
      summary:
        "Your organization's do-not-contact list, oldest first, to copy unsubscribes and bounces into your CRM or other sending tools. reason is manual, customer, unsubscribe or bounce. Read every page, then next time pass the last created_at you saw as since.",
      params: [
        ["since", "optional RFC 3339 timestamp, returns entries added after it"],
        ["page", "1-based page number"],
        ["per_page", "1 to 200, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "0b5f…",
      "org_id": "9e41…",
      "value": "sarah@northwind.studio",
      "kind": "email",
      "reason": "unsubscribe",
      "created_at": "2026-11-03T15:42:00Z"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/suppressions",
      summary:
        "Add up to 1,000 emails or domains to your organization's do-not-contact list, for example from your CRM. Splee checks the list before every email, so queued emails to them are cancelled too. added counts the new ones. One value that isn't an email or a domain rejects the request.",
      body: `{ "values": ["sarah@northwind.studio", "competitor.com"] }`,
      response: `{ "added": 2 }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/replies?since=2026-11-01T00:00:00Z`,
      summary:
        "Replies your leads sent, with the agent's classification. Oldest first, up to 500 per request. Poll with since set to the last created_at you saw.",
      params: [["since", "optional RFC 3339 timestamp, returns replies created after it"]],
      response: `[
  {
    "id": "71aa…",
    "lead_id": "c42e…",
    "direction": "inbound",
    "subject": "Re: Northwind Studio x Acme",
    "body": "Sounds interesting, can you send times for next week?",
    "from_email": "sarah@northwind.studio",
    "classification": "hot",
    "classification_reason": "Asks to schedule a call",
    "created_at": "2026-11-04T10:12:00Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/inbox?project_id=${projectId}&filter=needs_reply`,
      summary:
        "Reply threads like the Inbox page, the latest first, with the last message in either direction cut to 300 characters.",
      params: [
        ["project_id", "optional, all projects without it"],
        ["filter", "all, needs_reply, hot, auto_replied or other, default all"],
        ["q", "optional text in the lead's name, email or company"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "lead_id": "c42e…",
      "project_id": "${projectId}",
      "project_name": "Acme",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "lead_status": "hot",
      "classification": "hot",
      "pending_auto_reply": false,
      "last_message": {
        "id": "71aa…",
        "direction": "inbound",
        "subject": "Re: Northwind Studio x Acme",
        "snippet": "Sounds interesting, can you send times for next week?",
        "at": "2026-11-04T10:12:00Z"
      }
    }
  ],
  "total": 1
}`,
    },
    {
      method: "GET",
      path: `/emails?project_id=${projectId}`,
      summary:
        "Draft emails waiting for approval, oldest first, across all projects or one. Pass status to see others, like approved or sent.",
      params: [
        ["project_id, segment_id", "optional project or campaign"],
        ["status", "optional, draft by default, comma-separated for several"],
        ["page, per_page", "1-based page number, and 1 to 200 per page, default 50"],
      ],
      response: `{
  "items": [
    {
      "id": "a19c…",
      "project_id": "${projectId}",
      "segment_id": "5d0a…",
      "campaign": "Design studios",
      "lead_id": "c42e…",
      "lead_name": "Sarah Chen",
      "lead_email": "sarah@northwind.studio",
      "company_name": "Northwind Studio",
      "kind": "initial",
      "step": 0,
      "subject": "Northwind Studio x Acme",
      "body": "Hi Sarah, …",
      "status": "draft",
      "scheduled_for": null,
      "created_at": "2026-11-04T10:03:00Z"
    }
  ],
  "total": 1
}`,
    },
    {
      method: "POST",
      path: "/emails/a19c…/approve",
      summary:
        "Approve an email, like Approve in the app. It goes out from your mailboxes on the project's schedule once its campaign is sending. Approving a failed or cancelled email sends it again. 400 once it was sent.",
      response: `{ "id": "a19c…", "status": "approved", "…": "…" }`,
    },
    {
      method: "POST",
      path: "/emails/a19c…/skip",
      summary: "Cancel an email so it never goes out. Approve can still send it later.",
      response: `{ "id": "a19c…", "status": "cancelled", "…": "…" }`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/sent-emails?since=2026-11-01T00:00:00Z`,
      summary:
        "Emails Splee sent your leads: first emails, follow-ups, auto-replies and replies from the inbox. Oldest first, up to 500 per request. Poll with since set to the last sent_at you saw, or get each one as it goes out with the Email sent webhook.",
      params: [["since", "optional RFC 3339 timestamp, returns emails sent after it"]],
      response: `[
  {
    "id": "5d03…",
    "lead_id": "c42e…",
    "direction": "outbound",
    "kind": "initial",
    "step": 0,
    "subject": "Northwind Studio x Acme",
    "body": "Hi Sarah, …",
    "from_email": "alex@acme.com",
    "to_email": "sarah@northwind.studio",
    "message_id": "9e1c…@acme.com",
    "sent_at": "2026-11-03T15:40:00Z"
  }
]`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/stats`,
      summary:
        "Lifetime results and cost of a project and each of its campaigns. Money is in micro-dollars (1,000,000 = $1). AI cost is what Claude Code reports for the agent's runs, and email cost uses each mailbox's cost per email. Runs that research the whole project count only in totals. contacted counts leads sent at least one email and replies counts leads who wrote back, not bounces, so replies over contacted is the reply rate. steps has one entry per email of the sequence that went out, step 0 being the first email: a lead's first reply counts for the last email they got before it.",
      response: `{
  "totals": {
    "leads": 240,
    "drafts": 12,
    "scheduled": 30,
    "sent": 410,
    "contacted": 230,
    "replies": 19,
    "hot": 4,
    "meetings": 1,
    "ai_cost_micros": 8420000,
    "email_cost_micros": 410000,
    "spend_micros": 8830000,
    "cost_per_hot_micros": 2207500
  },
  "campaigns": [
    {
      "id": "5d0a…",
      "name": "Design studios",
      "status": "scaling",
      "companies": 60,
      "leads": 120,
      "sent": 230,
      "contacted": 118,
      "replies": 12,
      "bounced": 2,
      "hot": 3,
      "spend_micros": 3120000,
      "cost_per_hot_micros": 1040000,
      "…": "…"
    }
  ],
  "steps": [
    { "step": 0, "sent": 230, "replied": 13 },
    { "step": 1, "sent": 140, "replied": 5 },
    { "step": 2, "sent": 40, "replied": 1 }
  ]
}`,
    },
    {
      method: "GET",
      path: `/projects/${projectId}/analytics`,
      summary:
        "What the Analytics page shows for a date range: totals with the same totals for the period before in previous, a series per day (per week past 90 days), and breakdowns by campaign, mailbox, step, send weekday and hour, and channel. Days and hours are in the project's time zone. Each thing counts on the day it happened, and a reply, bounce, meeting or won deal counts for the campaign, mailbox and step of the last email the lead got before it. Rates are by lead, out of contacted, except steps and the heatmap, which are out of the emails sent. claude_cost_micros leaves out runs when you filter by mailbox. won_value has one entry per currency, in cents. revenue_cents is what leads paid through Stripe, Paddle or Lemon Squeezy, less refunds, in the organization's currency, and ignored_payments counts payments in other currencies. revenue_per_meeting_cents is revenue over meetings. return_on_spend is revenue over spend, and null unless the organization's currency is USD, since spend is in dollars.",
      params: [
        ["from", "optional first day, YYYY-MM-DD. 29 days before to by default"],
        ["to", "optional last day, YYYY-MM-DD. Today by default. At most 365 days in all"],
        ["segment_id", "optional campaign id"],
        ["mailbox_id", "optional mailbox id"],
      ],
      response: `{
  "from": "2026-10-13",
  "to": "2026-11-11",
  "previous_from": "2026-09-13",
  "previous_to": "2026-10-12",
  "timezone": "Europe/London",
  "currency": "USD",
  "revenue_tracked": true,
  "ignored_payments": 0,
  "ignored_currencies": [],
  "interval": "day",
  "totals": {
    "sent": 410, "contacted": 230, "delivered": 404, "replies": 19, "reply_rate": 0.083, "hot": 4, "hot_rate": 0.017,
    "meetings": 2, "deals_won": 1, "won_value": [{ "currency": "USD", "value_cents": 1200000 }], "unsubscribes": 3,
    "bounced": 6, "bounce_rate": 0.026, "claude_cost_micros": 8420000, "email_cost_micros": 410000,
    "spend_micros": 8830000, "cost_per_hot_micros": 2207500, "cost_per_meeting_micros": 4415000,
    "revenue_cents": 1440000, "refunded_cents": 0, "payments": 3, "paying_leads": 1, "revenue_per_meeting_cents": 720000,
    "return_on_spend": 1630.8
  },
  "previous": { "…": "…" },
  "series": [{ "date": "2026-10-13", "sent": 14, "replies": 1, "hot": 0, "meetings": 0 }],
  "campaigns": [{ "segment_id": "5d0a…", "name": "Design studios", "sent": 230, "replies": 12, "reply_rate": 0.1, "…": "…" }],
  "mailboxes": [{ "mailbox_id": "8b2e…", "email": "alex@acme.com", "sent": 205, "bounce_rate": 0.02, "bouncing": false, "…": "…" }],
  "steps": [{ "step": 0, "sent": 230, "replies": 13, "reply_rate": 0.057 }],
  "heatmap": [{ "weekday": 2, "hour": 9, "sent": 40, "replies": 4 }],
  "channels": [{ "channel": "email", "sent": 410, "leads": 230, "replies": 17, "reply_rate": 0.074 }]
}`,
    },
  ];
}

/** The REST hook endpoints Zapier, Make and n8n call. Keep the events in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs. */
function hookEndpoints(projectId: string): Endpoint[] {
  return [
    {
      method: "GET",
      path: "/me",
      summary: "The organization, key and projects your API key belongs to, for a tool's connection test.",
      response: `{
  "organization": { "id": "4e7c…", "name": "Acme" },
  "api_key": { "id": "0b9d…", "name": "Zapier", "prefix": "splee_k3Rt9x" },
  "projects": [{ "id": "${projectId}", "name": "Acme" }]
}`,
    },
    {
      method: "POST",
      path: "/hooks",
      summary:
        "Subscribe a URL to one event. Leave out project_id to hear about every project. 400 for an unknown event or a URL Splee can't reach, like one on a private network.",
      body: `{ "event": "hot_lead", "target_url": "https://hooks.zapier.com/hooks/standard/…", "project_id": "${projectId}" }`,
      response: `{
  "id": "e81f…",
  "project_id": "${projectId}",
  "event": "hot_lead",
  "target_url": "https://hooks.zapier.com/hooks/standard/…",
  "disabled_at": null,
  "created_at": "2026-11-12T09:30:00Z",
  "…": "…"
}`,
    },
    {
      method: "DELETE",
      path: "/hooks/e81f…",
      summary: "Unsubscribe, when the zap, scenario or workflow is turned off.",
      response: `{ "ok": true }`,
    },
    {
      method: "GET",
      path: `/hooks/samples/hot_lead?project_id=${projectId}`,
      summary:
        "Up to 3 recent payloads of an event, the newest first, for a trigger's setup screen. With none yet, lead_added gives your newest leads and the others give one example.",
      params: [["project_id", "optional, only this project's"]],
      response: `[
  {
    "event": "hot_lead",
    "project": { "id": "${projectId}", "name": "Acme" },
    "lead": { "id": "c42e…", "full_name": "Sarah Chen", "email": "sarah@northwind.studio", "…": "…" },
    "reply": { "id": "71aa…", "body": "Sounds interesting, can you send times for next week?", "classification": "hot", "…": "…" },
    "url": "…"
  }
]`,
    },
  ];
}

const HOOK_EVENTS: [event: string, description: string][] = [
  ["hot_lead", "a reply that shows interest, with the reply"],
  ["lead_replied", "any reply a person wrote, with how Splee classified it"],
  ["meeting_booked", "the lead booked a call, or your team marked them Meeting booked"],
  ["status_changed", "someone on your team changed the lead's status, with previous_status"],
  ["deal.won", "a deal closed as won, with the deal"],
  ["lead_added", "a new lead in the project, from any source, within about a minute"],
];

function EndpointSection({ e, base }: { e: Endpoint; base: string }) {
  const curl = [
    `curl${e.method === "GET" ? "" : ` -X ${e.method}`} "${base}${e.path}"`,
    `  -H "Authorization: Bearer $SPLEE_API_KEY"`,
    ...(e.body ? [`  -H "Content-Type: application/json"`, `  -d '${e.body}'`] : []),
  ].join(" \\\n");
  return (
    <Section
      title={
        <span className="flex items-center gap-2 font-mono text-sm">
          <Badge tone={METHOD_TONES[e.method]}>{e.method}</Badge>
          {/* Wraps on a phone, filling each line like the code below it. Firefox doesn't break it at the slashes, so
              without this it widened the page. */}
          <span className="min-w-0 break-all">{e.path.split("?")[0]}</span>
        </span>
      }
      description={e.summary}
    >
      {e.params && (
        <ul className="mb-4 space-y-1 text-sm">
          {e.params.map(([name, description]) => (
            <li key={name}>
              <code className="font-mono">{name}</code> <span className="text-muted-foreground">{description}</span>
            </li>
          ))}
        </ul>
      )}
      <div className="grid gap-3">
        <CodeBlock label="Request" code={curl} />
        <CodeBlock label="Response" code={e.response} />
      </div>
    </Section>
  );
}

export default function DocsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const base = `${useOrigin()}/api/v1`;

  return (
    <>
      <PageHeader
        title="API"
        description="Run campaigns from your own scripts or CRM: find companies and people, approve emails, and read replies and results."
        actions={
          <LinkButton href="/app/api-keys" variant="outline">
            <KeyRound className="size-4" /> API keys
          </LinkButton>
        }
      />
      <PageBody>
        <SettingsColumn>
          <Section title="Authentication" description="Send an API key from the API keys page as a Bearer token. Keys belong to the organization and see all of its projects.">
            <div className="space-y-3 text-sm">
              <div>
                <MicroLabel>Base URL</MicroLabel>
                <code className="mt-1 block font-mono">{base}</code>
              </div>
              <CodeBlock label="Header" code={"Authorization: Bearer splee_…"} />
              <p className="text-muted-foreground">
                Requests and responses are JSON. Errors return a status code and <code className="font-mono">{`{ "error": "message" }`}</code>. Paginated lists
                return <code className="font-mono">{`{ "items": [...], "total": n }`}</code>.
              </p>
            </div>
          </Section>

          {endpoints(projectId).map((e) => (
            <EndpointSection key={e.method + e.path} e={e} base={base} />
          ))}

          <Section
            title="Subscriptions for Zapier, Make and n8n"
            description="Subscribe a URL to a lead event and Splee posts each one to it as it happens, with the same JSON and signature as the webhooks on the Integrations page. A post that gets 410 Gone turns the subscription off, and revoking the key stops all of its subscriptions."
          >
            <MicroLabel>Events</MicroLabel>
            <ul className="mt-2 space-y-1 text-sm">
              {HOOK_EVENTS.map(([event, description]) => (
                <li key={event}>
                  <code className="font-mono">{event}</code> <span className="text-muted-foreground">{description}</span>
                </li>
              ))}
            </ul>
          </Section>
          {hookEndpoints(projectId).map((e) => (
            <EndpointSection key={e.method + e.path} e={e} base={base} />
          ))}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
