"use client";

import { useParams } from "next/navigation";
import { useSyncExternalStore } from "react";
import { KeyRound } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CodeBlock } from "@/components/app/settings/code-block";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { MicroLabel } from "@/components/app/work/states";
import { LinkButton } from "@/components/ui/button";
import { Badge, type BadgeTone } from "@/components/ui/misc";

interface Endpoint {
  method: "GET" | "POST" | "PATCH";
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
  "last_contacted_at": "2026-11-03T09:14:00Z",
  "created_at": "2026-11-02T16:40:00Z"
}`;

const METHOD_TONES: Record<Endpoint["method"], BadgeTone> = { GET: "blue", POST: "green", PATCH: "amber" };

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
      path: `/projects/${projectId}/leads?status=hot&page=1`,
      summary: "Leads of a project, newest first.",
      params: [
        ["status", "optional lead status: new, contacted, replied, hot, meeting_booked, …"],
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
        "Set a lead's status, for example meeting_booked when your calendar books a call or not_interested from your CRM. Follow-ups only go to contacted leads, so any other status stops them. Returns the lead like GET.",
      params: [["status", "new, queued, contacted, replied, hot, meeting_booked, not_interested, bounced, unsubscribed or do_not_contact"]],
      body: `{ "status": "meeting_booked" }`,
      response: `{
  "id": "c42e…",
  "status": "meeting_booked",
  "…": "…"
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
      path: `/projects/${projectId}/stats`,
      summary:
        "Lifetime results and cost of a project and each of its campaigns. Money is in micro-dollars (1,000,000 = $1). AI cost is what Claude Code reports for the agent's runs, and email cost uses each mailbox's cost per email. Runs that research the whole project count only in totals.",
      response: `{
  "totals": {
    "leads": 240,
    "drafts": 12,
    "scheduled": 30,
    "sent": 410,
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
      "replies": 12,
      "hot": 3,
      "spend_micros": 3120000,
      "cost_per_hot_micros": 1040000,
      "…": "…"
    }
  ]
}`,
    },
  ];
}

// The API lives on the same host as the app, so the examples use whatever origin this page is served from.
const subscribe = () => () => {};
const useOrigin = () => useSyncExternalStore(subscribe, () => window.location.origin, () => "");

export default function DocsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const base = `${useOrigin()}/api/v1`;

  return (
    <>
      <PageHeader
        title="API"
        description="Read leads, replies and results, push your own leads into Splee, and mark leads from your CRM or scripts."
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

          {endpoints(projectId).map((e) => {
            const curl = [
              `curl${e.method === "GET" ? "" : ` -X ${e.method}`} "${base}${e.path}"`,
              `  -H "Authorization: Bearer $SPLEE_API_KEY"`,
              ...(e.body ? [`  -H "Content-Type: application/json"`, `  -d '${e.body}'`] : []),
            ].join(" \\\n");
            return (
              <Section
                key={e.method + e.path}
                title={
                  <span className="flex items-center gap-2 font-mono text-sm">
                    <Badge tone={METHOD_TONES[e.method]}>{e.method}</Badge>
                    {e.path.split("?")[0]}
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
          })}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
