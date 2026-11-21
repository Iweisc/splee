"use client";

import { useState } from "react";
import { RefreshCw, Webhook } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { CodeBlock } from "@/components/app/settings/code-block";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings, WebhookDelivery, WebhookEvent } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";
import { EndpointList } from "./endpoint-list";
import { hostOf } from "./shared";

const PAYLOAD = `POST <your webhook URL>
Content-Type: application/json
Splee-Timestamp: 1762588800
Splee-Signature: v1=5f0c…

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
    "status": "hot",
    "company_name": "Northwind Studio",
    "company_domain": "northwind.studio",
    "linkedin_url": "https://www.linkedin.com/in/sarah-chen",
    "x_url": "https://x.com/sarahchen",
    "meeting_at": null
  },
  "reply": {
    "id": "71aa…",
    "subject": "Re: Northwind Studio x Acme",
    "body": "Sounds interesting, can you send times for next week?",
    "classification": "hot",
    "reason": "Asks to schedule a call"
  },
  "url": "https://your-splee-host/app/p/9b1f…/inbox?lead=c42e…"
}`;

// Keep in sync with hooks::sign in backend/src/hooks.rs.
const VERIFY = `import { createHmac, timingSafeEqual } from "node:crypto";

// rawBody is the request body exactly as received, before JSON parsing.
function fromSplee(rawBody, headers, secret) {
  const timestamp = headers["splee-timestamp"];
  const signed = \`\${timestamp}.\${rawBody}\`;
  const expected = "v1=" + createHmac("sha256", secret).update(signed).digest("hex");
  const given = Buffer.from(String(headers["splee-signature"] ?? ""));
  const fresh = Math.abs(Date.now() / 1000 - Number(timestamp)) < 300;
  // Byte lengths, not string lengths: timingSafeEqual throws when they differ.
  if (!fresh || given.length !== Buffer.byteLength(expected)) return false;
  return timingSafeEqual(given, Buffer.from(expected));
}`;

// Keep in sync with hooks::EVENTS in backend/src/hooks.rs.
const EVENTS: { value: WebhookEvent; label: string; hint: string }[] = [
  { value: "hot_lead", label: "Hot lead", hint: "A reply that shows interest, like asking for a call." },
  { value: "lead_replied", label: "Every reply", hint: "Any reply a person wrote, with how Splee classified it." },
  { value: "meeting_booked", label: "Meeting booked", hint: "The lead booked a call, or your team marked them Meeting booked." },
  { value: "meeting_rescheduled", label: "Meeting moved", hint: "The lead moved the call they booked to a new time." },
  { value: "meeting_cancelled", label: "Meeting cancelled", hint: "The lead cancelled the call they booked through your scheduling tool." },
  { value: "lead_unsubscribed", label: "Unsubscribed", hint: "The lead clicked unsubscribe or asked to stop." },
  { value: "lead_bounced", label: "Bounced", hint: "The address can't get email." },
  { value: "email_sent", label: "Email sent", hint: "Every email Splee sends a lead, to log it in your CRM." },
  { value: "email.opened", label: "Email opened", hint: "The first time a lead opens an email, in campaigns that track opens." },
  { value: "email.clicked", label: "Link clicked", hint: "The first time a lead clicks each link in an email. Scanners are left out." },
  { value: "follow_up_due", label: "Follow-up due", hint: "A reminder you set in the inbox came due." },
  { value: "form_submitted", label: "Form submitted", hint: "Someone filled in your website form and became a lead." },
  { value: "status_changed", label: "Status changed", hint: "Someone on your team changed a lead's status in Splee." },
  { value: "deal.created", label: "Deal opened", hint: "A deal opened on the pipeline, from a hot reply, a booking or by hand." },
  { value: "deal.stage_changed", label: "Deal moved", hint: "A deal moved to another stage of the pipeline." },
  { value: "deal.won", label: "Deal won", hint: "A deal closed as won, with its final value." },
  { value: "deal.lost", label: "Deal lost", hint: "A deal closed as lost, with the reason given." },
  { value: "lead_erased", label: "Lead deleted", hint: "Someone deleted or forgot a lead. Only its ids are sent, to delete your copy." },
];

/** The last lead events the webhook got and how each went, with a way to send one again after fixing the endpoint. */
function RecentDeliveries({ projectId }: { projectId: string }) {
  const { mutate } = useSWRConfig();
  const key = `/projects/${projectId}/integrations/webhook-deliveries`;
  const { data, error, mutate: reload } = useSWR<WebhookDelivery[]>(key, fetcher);
  const [resending, setResending] = useState<string | null>(null);
  const label = (event: string) => EVENTS.find((e) => e.value === event)?.label ?? event;

  const resend = async (d: WebhookDelivery) => {
    setResending(d.id);
    try {
      const again = await api<WebhookDelivery>(`${key}/${d.id}/resend`, { method: "POST" });
      if (again.error) toast.error(`It didn't go through: ${again.error}`);
      else toast.success("Sent again");
      await mutate((k) => k === key || k === `/projects/${projectId}/webhook-endpoints`);
    } catch (e) {
      toastError(e);
    } finally {
      setResending(null);
    }
  };

  return (
    <details className="mt-3">
      <summary className="cursor-pointer text-sm font-medium select-none">Recent deliveries</summary>
      {error ? (
        <LoadError error={error} what="your deliveries" retry={() => reload()} className="mt-3 rounded-lg border border-border py-6" />
      ) : !data?.length ? (
        <p className="mt-2 text-xs text-muted-foreground">Nothing sent yet. Each event shows up here with how your endpoint answered.</p>
      ) : (
        <ul className="mt-3 divide-y divide-border rounded-lg border border-border">
          {data.map((d) => (
            <li key={d.id} className="flex items-start gap-3 px-3 py-2.5 text-sm">
              <div className="min-w-0 flex-1">
                <p className="flex items-center gap-2">
                  <Badge tone={d.error ? "red" : "green"}>{d.error ? "Failed" : "Delivered"}</Badge>
                  <span className="truncate font-medium">{label(d.event)}</span>
                </p>
                <p className="mt-0.5 truncate text-xs text-muted-foreground">
                  {[d.payload.lead?.full_name, d.url && hostOf(d.url)].filter(Boolean).map((part) => `${part} · `)}
                  <span title={dateTime(d.created_at)}>{timeAgo(d.created_at)}</span>
                </p>
                {d.error && <p className="mt-1 font-mono text-xs break-words text-red-700">{d.error}</p>}
              </div>
              {/* Just the icon on phones, so the event name fits beside the badge. */}
              <Button variant="ghost" size="sm" onClick={() => resend(d)} loading={resending === d.id} disabled={!!resending} aria-label="Resend">
                <RefreshCw className="size-3.5" /> <span className="max-sm:hidden">Resend</span>
              </Button>
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}

/** For a leaked secret. The new one replaces the old one right away, so the receiver has to be updated too. */
function ReplaceSecretButton({ projectId, onReplaced }: { projectId: string; onReplaced: (secret: string) => void }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const replace = async () => {
    if (!confirm("Replace the signing secret? Your endpoint will reject Splee's webhooks until you give it the new one.")) return;
    setBusy(true);
    try {
      const fresh = await api<ProjectSettings>(`/projects/${projectId}/integrations/webhook-secret`, { method: "POST" });
      await mutate(`/projects/${projectId}/settings`, fresh, { revalidate: false });
      onReplaced(fresh.webhook_secret);
      toast.success("Signing secret replaced. Update it in your endpoint.");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button size="sm" variant="outline" onClick={replace} loading={busy}>
      <RefreshCw className="size-3.5" /> Replace
    </Button>
  );
}

export function WebhooksSection({ projectId, settings: s, update }: { projectId: string; settings: ProjectSettings; update: (patch: Partial<ProjectSettings>) => void }) {
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Webhook className="size-4" /> Webhooks
        </span>
      }
      description="Sends a JSON POST to every endpoint you add when something happens to a lead, for your CRM, Zapier, Make or your own backend."
    >
      <EndpointList
        projectId={projectId}
        kind="webhook-endpoints"
        name={hostOf}
        added={(url) => `Lead events will go to ${hostOf(url)} too`}
        confirmRemove={(url) => `Stop sending lead events to ${hostOf(url)}?`}
        labels={{ first: "Endpoint URL", next: "Add another endpoint", input: "Webhook endpoint URL" }}
        hint="Each endpoint gets the events below, signed with the same secret."
        placeholder="https://example.com/hook"
      />
      <fieldset className="mt-4">
        <legend className="text-sm font-medium">Send these events</legend>
        <div className="mt-2 grid gap-2 sm:grid-cols-2">
          {EVENTS.map((e) => (
            <label key={e.value} className="flex items-start gap-2.5 rounded-lg border border-border px-3 py-2.5 sm:odd:last:col-span-2">
              <input
                type="checkbox"
                checked={s.webhook_events.includes(e.value)}
                onChange={(ev) =>
                  update({
                    webhook_events: ev.target.checked ? [...s.webhook_events, e.value] : s.webhook_events.filter((v) => v !== e.value),
                  })
                }
                className="mt-0.5 accent-black"
              />
              <span>
                <span className="block text-sm font-medium">{e.label}</span>
                <span className="block text-xs text-pretty text-muted-foreground">{e.hint}</span>
              </span>
            </label>
          ))}
        </div>
      </fieldset>
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-4 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Signing secret</span>
        <CopyField value={s.webhook_secret}>
          {/* Also patches an unsaved draft, so replacing the secret doesn't look like an unsaved edit. */}
          <ReplaceSecretButton projectId={projectId} onReplaced={(webhook_secret) => update({ webhook_secret })} />
        </CopyField>
        <p className="text-xs text-muted-foreground">
          Every webhook request is signed with this, so your endpoint can reject requests that didn&apos;t come from Splee. Chat alerts aren&apos;t signed.
        </p>
      </div>
      <details className="group mt-5">
        <summary className="cursor-pointer text-sm font-medium select-none">Show the payload</summary>
        <div className="mt-3">
          <CodeBlock code={PAYLOAD} label="hot_lead" />
          {/* Keep PAYLOAD in sync with the json! in lead_event, backend/src/hooks.rs. */}
          <p className="mt-2 text-xs text-muted-foreground">
            These are all the fields Splee sends. The values are examples. <code>lead_replied</code> has the same shape, with the reply&apos;s{" "}
            <code>classification</code>: hot, question, not_interested or other. <code>meeting_booked</code>, <code>meeting_cancelled</code>, <code>lead_unsubscribed</code> and <code>lead_bounced</code> send{" "}
            <code>&quot;reply&quot;: null</code>. <code>email_sent</code> sends <code>&quot;reply&quot;: null</code> and an <code>email</code> with its{" "}
            <code>id</code>, <code>kind</code> (initial, follow_up, auto_reply or manual_reply), <code>step</code>, <code>subject</code>, <code>body</code>,{" "}
            <code>from</code>, <code>to</code> and <code>message_id</code>. <code>email.opened</code> and <code>email.clicked</code> send{" "}
            <code>&quot;reply&quot;: null</code> and an <code>email</code> with its <code>id</code>, <code>subject</code>, <code>step</code>,{" "}
            <code>at</code> and the clicked <code>url</code>, null for an open. <code>follow_up_due</code> sends <code>&quot;reply&quot;: null</code> and a{" "}
            <code>reminder</code> with its <code>at</code> time and the lead&apos;s <code>notes</code>. <code>meeting_rescheduled</code> sends{" "}
            <code>&quot;reply&quot;: null</code> and the old time as <code>previous_meeting_at</code>. <code>form_submitted</code> sends{" "}
            <code>&quot;reply&quot;: null</code> and a <code>form</code> whose <code>answers</code> each have a <code>question</code> and an{" "}
            <code>answer</code>. <code>status_changed</code> sends <code>&quot;reply&quot;: null</code> and the old status as{" "}
            <code>previous_status</code>. The deal events send <code>&quot;reply&quot;: null</code> and a <code>deal</code> with its <code>id</code>,{" "}
            <code>stage</code>, <code>previous_stage</code>, <code>status</code> (open, won or lost), <code>value</code>, <code>currency</code>,{" "}
            <code>close_date</code>, <code>owner</code>, <code>lost_reason</code> and <code>url</code>. <code>lead_erased</code> sends only the lead&apos;s{" "}
            <code>id</code> and <code>campaign_id</code>, since its name and address are gone, with <code>forgotten</code>, <code>erased_at</code> and{" "}
            <code>&quot;reply&quot;: null</code>. &quot;Send a test&quot; posts the hot_lead shape with{" "}
            <code>&quot;event&quot;: &quot;test&quot;</code>. The lead&apos;s <code>meeting_at</code> is when the call they booked starts, once
            their scheduling tool sent it.
          </p>
          <div className="mt-4">
            <CodeBlock code={VERIFY} label="Check the signature (Node)" />
          </div>
          <p className="mt-2 text-xs text-muted-foreground">
            Splee-Signature is <code>v1=</code> and the hex HMAC-SHA256 of the timestamp, a dot and the raw body, keyed with your signing secret.
            Rejecting old timestamps stops a captured request from being replayed later.
          </p>
        </div>
      </details>
      <RecentDeliveries projectId={projectId} />
    </Section>
  );
}
