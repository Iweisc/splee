"use client";

import { use, useState } from "react";
import { CalendarCheck, CalendarClock, ClipboardList, Contact, Hash, MailCheck, RefreshCw, Send, Webhook } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { CodeBlock } from "@/components/app/settings/code-block";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { CopyField } from "@/components/app/settings/copy-button";
import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/misc";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import type { ProjectSettings, WebhookDelivery, WebhookEvent } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";

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
  { value: "meeting_cancelled", label: "Meeting cancelled", hint: "The lead cancelled the call they booked in Calendly or Cal.com." },
  { value: "lead_unsubscribed", label: "Unsubscribed", hint: "The lead clicked unsubscribe or asked to stop." },
  { value: "lead_bounced", label: "Bounced", hint: "The address can't get email." },
  { value: "email_sent", label: "Email sent", hint: "Every email Splee sends a lead, to log it in your CRM." },
  { value: "follow_up_due", label: "Follow-up due", hint: "A reminder you set in the inbox came due." },
];

const problemFor = (label: string, value: string) => (value && !isUrl(value) ? `${label} must be a full URL starting with https://` : null);

// Where each app hands out the URL. hooks::chat_payload formats alerts for the same apps.
const CHAT_APPS = [
  ["Slack", "Apps → Incoming Webhooks → Add to Slack"],
  ["Discord", "channel settings → Integrations → Webhooks"],
  ["Teams", "the channel's Workflows → Post to a channel when a webhook request is received"],
  ["Google Chat", "Apps & integrations → Webhooks"],
  ["Telegram", "bot's sendMessage URL with ?chat_id="],
  ["ntfy", "https://ntfy.sh/ and a topic only you know"],
];

/** Posts a sample alert to the URL as typed, saved or not. */
function TestButton({ projectId, target, url }: { projectId: string; target: "slack" | "webhook"; url: string }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const send = async () => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/integrations/test`, { body: { target, url } });
      // A test that passes on the saved URL clears its last failure.
      mutate(`/projects/${projectId}/settings`);
      toast.success(target === "slack" ? "Test sent. Check the channel." : "Test sent. Your endpoint accepted it.");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button size="sm" variant="outline" onClick={send} loading={busy} disabled={!isUrl(url)}>
      {!busy && <Send className="size-3.5" />} Send a test
    </Button>
  );
}

/** The backend clears this when an alert or a test to the saved URL goes through, or when the URL changes. */
function LastFailure({ error, at, what = "alert" }: { error: string | null; at: string | null; what?: "alert" | "sync" | "check" }) {
  if (!error || !at) return null;
  return (
    <div className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
      <p className="font-medium">
        The last {what} didn&apos;t go through, <span title={dateTime(at)}>{timeAgo(at)}</span>.
      </p>
      <p className="mt-1 font-mono">{error}</p>
    </div>
  );
}

/** The last lead events the webhook got and how each went, with a way to send one again after fixing the endpoint. */
function RecentDeliveries({ projectId }: { projectId: string }) {
  const { mutate } = useSWRConfig();
  const key = `/projects/${projectId}/integrations/webhook-deliveries`;
  const { data } = useSWR<WebhookDelivery[]>(key, fetcher);
  const [resending, setResending] = useState<string | null>(null);
  const label = (event: string) => EVENTS.find((e) => e.value === event)?.label ?? event;

  const resend = async (d: WebhookDelivery) => {
    setResending(d.id);
    try {
      const again = await api<WebhookDelivery>(`${key}/${d.id}/resend`, { method: "POST" });
      if (again.error) toast.error(`It didn't go through: ${again.error}`);
      else toast.success("Sent again");
      await mutate((k) => k === key || k === `/projects/${projectId}/settings`);
    } catch (e) {
      toastError(e);
    } finally {
      setResending(null);
    }
  };

  return (
    <details className="mt-3">
      <summary className="cursor-pointer text-sm font-medium select-none">Recent deliveries</summary>
      {!data?.length ? (
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
                  {d.payload.lead?.full_name && `${d.payload.lead.full_name} · `}
                  <span title={dateTime(d.created_at)}>{timeAgo(d.created_at)}</span>
                </p>
                {d.error && <p className="mt-1 font-mono text-xs break-words text-red-700">{d.error}</p>}
              </div>
              {/* Just the icon on phones, so the event name fits beside the badge. */}
              <Button variant="ghost" size="sm" onClick={() => resend(d)} loading={resending === d.id} disabled={!!resending} aria-label="Resend">
                {resending !== d.id && <RefreshCw className="size-3.5" />} <span className="max-sm:hidden">Resend</span>
              </Button>
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}

const CRMS = {
  hubspot: {
    name: "HubSpot",
    record: "contacts",
    field: "Service key",
    hint: "In HubSpot: Settings → Integrations → Service Keys → Create, with the crm.objects.contacts.read and crm.objects.contacts.write scopes. An older private app's token works too.",
  },
  pipedrive: {
    name: "Pipedrive",
    record: "people",
    field: "API token",
    hint: "In Pipedrive: your profile → Personal preferences → API, then copy your personal API token.",
  },
  attio: {
    name: "Attio",
    record: "people",
    field: "Access token",
    hint: "In Attio: Workspace settings → Developers → New access token, with read-write access to records and notes and read access to object configuration.",
  },
} as const;

/** Connects (PUT, with the secret) or disconnects (DELETE) an integration whose secret is write-only, then shows the saved settings. */
function useConnection(projectId: string, path: string) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const call = async (body: Record<string, string> | undefined, done: string) => {
    setBusy(true);
    try {
      const fresh = await api<ProjectSettings>(`/projects/${projectId}/integrations/${path}`, { method: body ? "PUT" : "DELETE", body });
      await mutate(`/projects/${projectId}/settings`, fresh, { revalidate: false });
      toast.success(done);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { busy, call };
}

/** Tokens are write-only, so once connected this only offers Disconnect. Connecting checks the token with the CRM first. */
function CrmSection({ projectId, settings, crm }: { projectId: string; settings: ProjectSettings; crm: keyof typeof CRMS }) {
  const { name, record, field, hint } = CRMS[crm];
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, crm);
  const connect = async () => {
    if (await call({ token }, `Connected to ${name}`)) setToken("");
  };
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New hot leads and meetings won't be added to it.`)) call(undefined, `${name} disconnected`);
  };
  const connected = settings[`${crm}_connected`];
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Contact className="size-4" /> {name}
        </span>
      }
      description={`Adds hot leads and booked meetings to ${name} as ${record}, with a note holding their reply.`}
      actions={
        connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {connected ? (
        <p className="text-sm">Connected. Splee matches by email, so people already in {name} get updated, not duplicated.</p>
      ) : (
        <Field label={field} hint={hint}>
          <div className="flex gap-2">
            {/* The label also wraps Connect, so name the input itself. */}
            <Input aria-label={field} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
            <Button onClick={connect} loading={busy} disabled={!token}>
              Connect
            </Button>
          </div>
        </Field>
      )}
      <LastFailure error={settings[`${crm}_error`]} at={settings[`${crm}_failed_at`]} what="sync" />
    </Section>
  );
}

const VERIFIERS = { neverbounce: "NeverBounce", millionverifier: "MillionVerifier", hunter: "Hunter" } as const;

/** The key is write-only, like a CRM token. Connecting checks it with the service first. */
function VerifierSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [provider, setProvider] = useState<keyof typeof VERIFIERS>("neverbounce");
  const [key, setKey] = useState("");
  const { busy, call } = useConnection(projectId, "verifier");
  const connect = async () => {
    if (await call({ provider, key }, `Connected to ${VERIFIERS[provider]}`)) setKey("");
  };
  const name = settings.verifier ? VERIFIERS[settings.verifier] : null;
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New addresses will be sent to without a check.`)) call(undefined, `${name} disconnected`);
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MailCheck className="size-4" /> Email verification
        </span>
      }
      description="Checks each address before its first email, so ones that don't exist fail here instead of bouncing."
      actions={
        name ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {name ? (
        <p className="text-sm">
          Connected to {name}. Addresses it can&apos;t confirm either way, like catch-all domains, still get their email.
          {settings.verifier === "hunter" && " Guessed addresses are looked up with Hunter's Email Finder first."}
        </p>
      ) : (
        <div className="flex flex-col gap-4 sm:flex-row">
          <Field label="Service" className="sm:w-44">
            <Select value={provider} onChange={(e) => setProvider(e.target.value as keyof typeof VERIFIERS)}>
              {Object.entries(VERIFIERS).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="API key" className="flex-1" hint="Each check uses one of the service's credits.">
            <div className="flex gap-2">
              <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
              <Button onClick={connect} loading={busy} disabled={!key}>
                Connect
              </Button>
            </div>
          </Field>
        </div>
      )}
      <LastFailure error={settings.verifier_error} at={settings.verifier_failed_at} what="check" />
    </Section>
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
      {!busy && <RefreshCw className="size-3.5" />} Replace
    </Button>
  );
}

/** The form webhook URL, with the campaign new leads join in its query. */
function FormSection({ projectId, token }: { projectId: string; token: string }) {
  const origin = useOrigin();
  const [segmentId, setSegmentId] = useState("");
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <ClipboardList className="size-4" /> Website forms
        </span>
      }
      description="Whoever fills in your Tally, Typeform or Webflow form becomes a lead and pings your chat."
    >
      <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} />
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-4 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Form webhook URL</span>
        <CopyField value={`${origin}/api/forms/${token}${segmentId ? `?campaign=${segmentId}` : ""}`} />
        <p className="text-xs text-pretty text-muted-foreground">
          Tally: Integrations → Webhooks. Typeform: Connect → Webhooks. Webflow: Site settings → Integrations → Webhooks, Form
          submission. Anything else can POST <code>{`{"email": "...", "name": "..."}`}</code>. Answers that aren&apos;t a name, email,
          company, title, website or LinkedIn go in the lead&apos;s notes.
        </p>
      </div>
    </Section>
  );
}

export default function IntegrationsPage({ params }: PageProps<"/app/p/[projectId]/integrations">) {
  const { projectId } = use(params);
  const origin = useOrigin();
  return (
    <ProjectSettingsPage
      title="Integrations"
      description="Get told the moment a lead replies, in your own tools."
      problem={(s) => problemFor("Webhook URL", s.webhook_url) ?? problemFor("Chat webhook URL", s.slack_webhook_url)}
    >
      {(s, update) => (
        <>
          <Section
            title={
              <span className="flex items-center gap-2">
                <Hash className="size-4" /> Chat alerts
              </span>
            }
            description="Posts to Slack, Discord, Teams, Google Chat or Telegram, or to your phone with ntfy, about hot leads, booked meetings and follow-ups that come due."
            actions={<TestButton projectId={projectId} target="slack" url={s.slack_webhook_url} />}
          >
            <Field
              label="Incoming webhook URL"
              error={problemFor("Chat webhook URL", s.slack_webhook_url)}
              hint={CHAT_APPS.map(([app, where]) => (
                <span key={app} className="block">
                  {app}: {where}
                </span>
              ))}
            >
              <Input
                type="url"
                value={s.slack_webhook_url}
                onChange={(e) => update({ slack_webhook_url: e.target.value.trim() })}
                placeholder="https://hooks.slack.com/services/…"
              />
            </Field>
            <LastFailure error={s.slack_error} at={s.slack_failed_at} />
            <div className="mt-4 flex items-start justify-between gap-4 rounded-lg border border-border px-3 py-2.5">
              <span>
                <span className="block text-sm font-medium">Weekly report</span>
                <span className="block text-xs text-muted-foreground">Mondays at 08:00 UTC: emails, replies, hot leads and spend.</span>
              </span>
              <Switch
                checked={s.chat_weekly_report}
                onChange={(chat_weekly_report) => update({ chat_weekly_report })}
                disabled={!s.slack_webhook_url}
                label="Weekly report"
              />
            </div>
          </Section>

          <CrmSection projectId={projectId} settings={s} crm="hubspot" />
          <CrmSection projectId={projectId} settings={s} crm="pipedrive" />
          <CrmSection projectId={projectId} settings={s} crm="attio" />
          <VerifierSection projectId={projectId} settings={s} />

          <Section
            title={
              <span className="flex items-center gap-2">
                <Webhook className="size-4" /> Webhook
              </span>
            }
            description="Sends a JSON POST when something happens to a lead, for your CRM, Zapier, Make or your own backend."
            actions={<TestButton projectId={projectId} target="webhook" url={s.webhook_url} />}
          >
            <Field label="Endpoint URL" error={problemFor("Webhook URL", s.webhook_url)}>
              <Input
                type="url"
                value={s.webhook_url}
                onChange={(e) => update({ webhook_url: e.target.value.trim() })}
                placeholder="https://example.com/hooks/splee"
              />
            </Field>
            <fieldset className="mt-4">
              <legend className="text-sm font-medium">Send these events</legend>
              <div className="mt-2 grid gap-2 sm:grid-cols-2">
                {EVENTS.map((e) => (
                  <label key={e.value} className="flex items-start gap-2.5 rounded-lg border border-border px-3 py-2.5">
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
                      <span className="block text-xs text-muted-foreground">{e.hint}</span>
                    </span>
                  </label>
                ))}
              </div>
            </fieldset>
            <LastFailure error={s.webhook_error} at={s.webhook_failed_at} />
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
                  <code>from</code>, <code>to</code> and <code>message_id</code>. <code>follow_up_due</code> sends <code>&quot;reply&quot;: null</code> and a{" "}
                  <code>reminder</code> with its <code>at</code> time and the lead&apos;s <code>notes</code>. &quot;Send a test&quot; posts the hot_lead shape with{" "}
                  <code>&quot;event&quot;: &quot;test&quot;</code>. The lead&apos;s <code>meeting_at</code> is when the call they booked starts, once
                  Calendly or Cal.com sent it.
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
          <FormSection projectId={projectId} token={s.form_token} />
          <Section
            title={
              <span className="flex items-center gap-2">
                <CalendarCheck className="size-4" /> Calendly and Cal.com
              </span>
            }
            description="When a lead books a call, Splee marks them Meeting booked and stops emailing them. If they cancel, they go back to Hot. Your chat channel and webhook hear about both."
          >
            {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
            <div className="flex flex-col gap-1.5">
              <span className="text-sm font-medium">Booking webhook URL</span>
              <CopyField value={`${origin}/api/bookings/${s.booking_token}`} />
              <p className="text-xs text-pretty text-muted-foreground">
                In Cal.com: Settings → Developer → Webhooks → New, paste this URL and pick Booking created, rescheduled and cancelled. In
                Calendly, subscribe it to <code>invitee.created</code> and <code>invitee.canceled</code> with Calendly&apos;s webhook API. Anything else, like a Zapier step, can POST{" "}
                <code>{`{"email": "...", "start_time": "..."}`}</code>. Splee finds the lead by email and shows the call&apos;s time on it.
              </p>
            </div>
          </Section>
          <Section
            title={
              <span className="flex items-center gap-2">
                <CalendarClock className="size-4" /> Reminders in your calendar
              </span>
            }
            description="Inbox reminders become calendar events with the lead's notes and a link."
          >
            <div className="flex flex-col gap-1.5">
              <span className="text-sm font-medium">Calendar feed URL</span>
              <CopyField value={`${origin}/api/calendar/${s.calendar_token}.ics`} />
              <p className="text-xs text-pretty text-muted-foreground">
                Anyone with this URL sees your reminders. Google Calendar: Other calendars → + → From URL. Outlook: Add calendar → Subscribe
                from web. Apple Calendar: File → New Calendar Subscription.
              </p>
            </div>
          </Section>
        </>
      )}
    </ProjectSettingsPage>
  );
}
