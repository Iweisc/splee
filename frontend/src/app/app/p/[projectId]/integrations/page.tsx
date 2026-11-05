"use client";

import { Hash, Webhook } from "lucide-react";
import { CodeBlock } from "@/components/app/settings/code-block";
import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { Field, Input } from "@/components/ui/form";

const PAYLOAD = `POST <your webhook URL>
Content-Type: application/json

{
  "event": "hot_lead",
  "project": {
    "id": "9b1f…",
    "name": "Acme"
  },
  "lead": {
    "id": "c42e…",
    "full_name": "Sarah Chen",
    "email": "sarah@northwind.studio",
    "title": "Founder",
    "company_name": "Northwind Studio",
    "company_domain": "northwind.studio"
  },
  "reply": {
    "id": "71aa…",
    "subject": "Re: Northwind Studio x Acme",
    "body": "Sounds interesting, can you send times for next week?",
    "reason": "Asks to schedule a call"
  },
  "url": "https://your-splee-host/app/p/9b1f…/inbox"
}`;

const problemFor = (label: string, value: string) => (value && !isUrl(value) ? `${label} must be a full URL starting with https://` : null);

export default function IntegrationsPage() {
  return (
    <ProjectSettingsPage
      title="Integrations"
      description="Get told the moment a lead replies with interest, in your own tools."
      problem={(s) => problemFor("Webhook URL", s.webhook_url) ?? problemFor("Slack webhook URL", s.slack_webhook_url)}
    >
      {(s, update) => (
        <>
          <Section
            title={
              <span className="flex items-center gap-2">
                <Hash className="size-4" /> Slack
              </span>
            }
            description="Posts a message to a Slack channel for every hot lead, with the lead, company and their reply."
          >
            <Field
              label="Incoming webhook URL"
              error={problemFor("Slack webhook URL", s.slack_webhook_url)}
              hint="In Slack: Apps → Incoming Webhooks → Add to Slack, pick a channel, copy the URL."
            >
              <Input
                type="url"
                value={s.slack_webhook_url}
                onChange={(e) => update({ slack_webhook_url: e.target.value.trim() })}
                placeholder="https://hooks.slack.com/services/…"
              />
            </Field>
          </Section>

          <Section
            title={
              <span className="flex items-center gap-2">
                <Webhook className="size-4" /> Webhook
              </span>
            }
            description="Sends a JSON POST for every hot lead, for your CRM, Zapier, Make or your own backend."
          >
            <Field label="Endpoint URL" error={problemFor("Webhook URL", s.webhook_url)}>
              <Input
                type="url"
                value={s.webhook_url}
                onChange={(e) => update({ webhook_url: e.target.value.trim() })}
                placeholder="https://example.com/hooks/splee"
              />
            </Field>
            <details className="group mt-5">
              <summary className="cursor-pointer text-sm font-medium select-none">Show the payload</summary>
              <div className="mt-3">
                <CodeBlock code={PAYLOAD} label="hot_lead" />
                {/* Keep PAYLOAD in sync with the json! in notify_hot, backend/src/agent/replies.rs. */}
                <p className="mt-2 text-xs text-muted-foreground">These are all the fields Splee sends. The values are examples.</p>
              </div>
            </details>
          </Section>
        </>
      )}
    </ProjectSettingsPage>
  );
}
