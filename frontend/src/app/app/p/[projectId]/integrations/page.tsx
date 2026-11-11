"use client";

import { use, useEffect, useState } from "react";
import Link from "next/link";
import {
  AtSign,
  CalendarCheck,
  CalendarClock,
  ClipboardList,
  Cloud,
  Contact,
  Eye,
  Globe,
  Hash,
  MailCheck,
  MailPlus,
  MessageCircleQuestionMark,
  MessageSquareText,
  MessagesSquare,
  Newspaper,
  PenLine,
  RefreshCw,
  Repeat2,
  Send,
  Spool,
  Trash2,
  TvMinimalPlay,
  UserPlus,
  UserSearch,
  UsersRound,
  Webhook,
  Zap,
} from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { CodeBlock } from "@/components/app/settings/code-block";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { ChipInput } from "@/components/app/settings/chip-input";
import { CopyField } from "@/components/app/settings/copy-button";
import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge, Host } from "@/components/ui/misc";
import { Field, Input, Select, Switch, Textarea } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import {
  CRM_NAMES,
  type CrmName,
  type DiscourseForum,
  type Endpoint,
  type ProjectSettings,
  type SlackStatus,
  type VisitorSite,
  type WebhookDelivery,
  type WebhookEvent,
} from "@/lib/types";
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
  { value: "follow_up_due", label: "Follow-up due", hint: "A reminder you set in the inbox came due." },
  { value: "form_submitted", label: "Form submitted", hint: "Someone filled in your website form and became a lead." },
  { value: "status_changed", label: "Status changed", hint: "Someone on your team changed a lead's status in Splee." },
];


// Where each app hands out the URL. hooks::chat_payload formats alerts for the same apps.
const CHAT_APPS = [
  ["Slack", "Apps → Incoming Webhooks → Add to Slack"],
  ["Discord", "channel settings → Integrations → Webhooks"],
  ["Teams", "the channel's Workflows → Post to a channel when a webhook request is received"],
  ["Google Chat", "Apps & integrations → Webhooks"],
  ["Telegram", "bot's sendMessage URL with ?chat_id="],
  ["ntfy", "https://ntfy.sh/ or your ntfy.* server, and a topic only you know"],
  ["Pushover", "https://api.pushover.net/1/messages.json?token=APP_TOKEN&user=USER_KEY"],
  ["Bark", "the https://api.day.app/ URL the app shows, ending in your device key"],
  ["Gotify", "your server's /message?token=APP_TOKEN"],
  ["Lark or Feishu", "group Settings → Bots → Custom Bot"],
  ["DingTalk", "group Bots → Custom, with IP security"],
  ["WeCom", "the group chat's ⋯ → Add Group Robot"],
  ["Webex", "Incoming Webhooks on the Webex App Hub"],
  ["Mattermost", "Integrations → Incoming Webhooks"],
  ["Rocket.Chat", "Integrations → New → Incoming"],
  ["Zulip", "an incoming webhook bot's Slack compatible URL"],
  ["Zoho Cliq", "Bots & Tools → Webhook Tokens → Get Webhook URL, for a channel"],
  ["Pumble", "Workspace settings → General settings → Incoming Webhooks → New Webhook"],
  ["Flock", "App Store → Webhooks → Incoming Webhook → Add, for a channel"],
] as const;

/**
 * Where the URL goes in each app, folded under a line naming the first few, so the list doesn't push the rest of the page
 * down for someone who uses one of them.
 */
function AppSteps({ steps }: { steps: readonly (readonly [app: string, step: React.ReactNode])[] }) {
  return (
    <details className="group mt-1.5 text-xs text-muted-foreground">
      <summary className="w-fit cursor-pointer select-none hover:text-foreground">
        Where it goes in {steps.slice(0, 3).map(([app]) => app).join(", ")} and {steps.length - 3} more
      </summary>
      <ul className="mt-1.5 flex flex-col gap-1 text-pretty">
        {steps.map(([app, step]) => (
          <li key={app}>
            {app}: {step}
          </li>
        ))}
      </ul>
    </details>
  );
}

const FORM_APPS = [
  ["Tally", "Integrations → Webhooks."],
  ["Typeform", "Connect → Webhooks."],
  ["Fillout", "Integrate → Webhook, in Simple mode."],
  ["Jotform", "Settings → Integrations → WebHooks."],
  ["Paperform", "After Submission → Integrations & Webhooks → Add Webhook, on New Submission."],
  ["Formbricks", "Configuration → Integrations → Manage Webhooks, on Response Finished."],
  ["Netlify", "Forms → Submission notifications → Add notification → HTTP POST request."],
  ["Webflow", "Site settings → Integrations → Webhooks, with Form submission."],
] as const;

const BOOKING_APPS = [
  ["Cal.com", "Settings → Developer → Webhooks → New, paste this URL and pick Booking created, rescheduled and cancelled."],
  ["Calendly", <>subscribe it to <code>invitee.created</code> and <code>invitee.canceled</code> with Calendly&apos;s webhook API.</>],
  ["SavvyCal", "Settings → Integrations, then + next to Webhooks."],
  ["Zcal", "Integrations, then Connect next to Webhooks, and paste this URL as the Subscriber URL."],
  ["TidyCal", "Integrations → Automations → Add webhook, with booking created, approved, rescheduled and cancelled."],
  ["OnceHub", "Account Integrations → APIs & Webhooks → Create webhook, with booking scheduled, rescheduled and canceled."],
  ["Appointlet", "Settings → Webhooks → Add Webhook."],
  ["Chili Piper", "Command Center → Integrations → Create Custom Webhook, once each for New Meeting, Meeting Update and Canceled Meeting, then Enabled."],
] as const;

/** Which app a chat URL posts to, for its row. The same hosts hooks::chat_payload formats alerts for. */
function chatApp(url: string): string {
  let host: string;
  let path: string[];
  let token: boolean;
  try {
    const parsed = new URL(url);
    host = parsed.hostname.toLowerCase();
    path = parsed.pathname.split("/").filter(Boolean);
    token = parsed.searchParams.has("token");
  } catch {
    return url;
  }
  const on = (domain: string) => host === domain || host.endsWith(`.${domain}`);
  if (on("slack.com")) return "Slack";
  if (on("discord.com") || on("discordapp.com")) return "Discord";
  if (on("logic.azure.com") || on("powerplatform.com") || on("webhook.office.com")) return "Teams";
  if (host === "chat.googleapis.com") return "Google Chat";
  if (host === "api.telegram.org") return "Telegram";
  // ntfy.sh or a self-hosted ntfy.* server, the same way hooks::ntfy does.
  if (host.startsWith("ntfy.")) return "ntfy";
  if (host === "api.pushover.net") return "Pushover";
  if (host === "open.larksuite.com") return "Lark";
  if (host === "open.feishu.cn") return "Feishu";
  if (host === "oapi.dingtalk.com") return "DingTalk";
  if (host === "qyapi.weixin.qq.com") return "WeCom";
  if (host === "webexapis.com") return "Webex";
  // cliq.zoho.com, cliq.zoho.eu and the other regions, the same way hooks::chat_payload tells them apart.
  if (host.startsWith("cliq.zoho")) return "Zoho Cliq";
  if (host === "api.pumble.com") return "Pumble";
  if (host === "api.day.app") return "Bark";
  if (host === "api.flock.com") return "Flock";
  // Self-hosted chats can be on any host, so their webhook paths tell them apart, the same way hooks::gotify and
  // hooks::mattermost do.
  if (path.at(-1) === "message" && token) return "Gotify";
  if (path.at(-2) === "hooks" && /^[a-z0-9]{26}$/.test(path.at(-1) ?? "")) return "Mattermost";
  if (path.at(-3) === "hooks") return "Rocket.Chat";
  if (path.join("/").includes("api/v1/external/slack_incoming")) return "Zulip";
  return host;
}

/** Posts a sample alert to the URL as typed, saved or not. `compact` shows just the icon on phones, for list rows. */
function TestButton({ projectId, target, url, compact }: { projectId: string; target: "slack" | "webhook"; url: string; compact?: boolean }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const send = async () => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/integrations/test`, { body: { target, url } });
      // A test that passes on a saved URL clears its last failure.
      mutate(`/projects/${projectId}/${target === "slack" ? "chat-channels" : "webhook-endpoints"}`);
      toast.success(target === "slack" ? "Test sent. Check the channel." : "Test sent. Your endpoint accepted it.");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button size="sm" variant="outline" onClick={send} loading={busy} disabled={!isUrl(url)} aria-label="Send a test">
      <Send className="size-3.5" /> <span className={compact ? "max-sm:hidden" : undefined}>Send a test</span>
    </Button>
  );
}

/** The backend clears this when an alert or a test to the saved URL goes through, or when the URL changes. */
function LastFailure({ error, at, what = "alert" }: { error: string | null; at: string | null; what?: "alert" | "sync" | "check" | "send" | "lookup" | "copy" | "post" }) {
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
  close: {
    name: "Close",
    record: "contacts",
    field: "API key",
    hint: "In Close: Settings → Developer → API Keys → New API Key.",
  },
  folk: {
    name: "folk",
    record: "people",
    field: "API key",
    hint: "In folk: Settings → API → Create a new key.",
  },
  capsule: {
    name: "Capsule",
    record: "people",
    field: "Access token",
    hint: "In Capsule: My Preferences → API Authentication Tokens, then generate a new token.",
  },
  brevo: {
    name: "Brevo",
    record: "contacts",
    field: "API key",
    hint: "In Brevo: Settings → SMTP & API → API keys → Generate a new API key.",
  },
  zendesk_sell: {
    name: "Zendesk Sell",
    record: "contacts",
    field: "Access token",
    hint: "In Zendesk Sell: Settings → Integrations → OAuth → Access Tokens, then add a user token.",
  },
  lacrm: {
    name: "Less Annoying CRM",
    record: "contacts",
    field: "API key",
    hint: "In Less Annoying CRM: Settings → API, then generate a new API key.",
  },
  salesflare: {
    name: "Salesflare",
    record: "contacts",
    field: "API key",
    hint: "In Salesflare: Settings → API keys, then the + button. Notes go on the contact's account.",
  },
  affinity: {
    name: "Affinity",
    record: "people",
    field: "API key",
    hint: "In Affinity: Settings → API, then Generate an API key.",
  },
  freshsales: {
    name: "Freshsales",
    record: "contacts",
    field: "API key",
    account: { label: "Bundle alias", placeholder: "acme.myfreshworks.com" },
    hint: "In Freshsales: your profile → Profile Settings → API Settings, which shows both.",
  },
  copper: {
    name: "Copper",
    record: "people",
    field: "API key",
    account: { label: "Your Copper email", placeholder: "you@company.com" },
    hint: "In Copper: Settings → Integrations → API Keys → Generate API Key.",
  },
  insightly: {
    name: "Insightly",
    record: "contacts",
    field: "API key",
    account: { label: "API URL", placeholder: "https://api.na1.insightly.com/v3.1/" },
    hint: "In Insightly: your profile → User Settings → API, which shows both.",
  },
  twenty: {
    name: "Twenty",
    record: "people",
    field: "API key",
    hint: "In Twenty: Settings → APIs & Webhooks → Create API key.",
  },
  twenty_self: {
    name: "Twenty (self-hosted)",
    record: "people",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://crm.acme.com" },
    hint: "In Twenty: Settings → APIs & Webhooks → Create API key.",
  },
  espocrm: {
    name: "EspoCRM",
    record: "contacts",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://crm.acme.com" },
    hint: "In EspoCRM: Administration → API Users → Create, with API Key auth and a role that can create Contacts.",
  },
  odoo: {
    name: "Odoo",
    record: "contacts",
    field: "API key",
    account: { label: "Site URL", placeholder: "https://acme.odoo.com" },
    hint: "Odoo 19+. In Odoo: your avatar → My Preferences → Security → Add API Key.",
  },
  notion: {
    name: "Notion",
    record: "database rows",
    field: "Integration secret",
    account: { label: "Database link", placeholder: "https://www.notion.so/…" },
    hint: "From an internal integration, added to the database under ••• → Connections. It needs an Email column.",
  },
  airtable: {
    name: "Airtable",
    record: "table records",
    field: "Personal access token",
    account: { label: "Table link", placeholder: "https://airtable.com/app…/tbl…" },
    hint: "With data.records:read, data.records:write, data.recordComments:write and schema.bases:read. It needs an Email field.",
  },
  baserow: {
    name: "Baserow",
    record: "table rows",
    field: "Database token",
    account: { label: "Table link", placeholder: "https://baserow.io/database/…/table/…" },
    hint: "On baserow.io or your own server, with create, read and update. The table needs an Email field and a long text field called Notes.",
  },
  nocodb: {
    name: "NocoDB",
    record: "table rows",
    field: "API token",
    account: { label: "Table link", placeholder: "https://app.nocodb.com/#/…" },
    hint: "On app.nocodb.com or your own server, from Account settings → Tokens. The table needs an Email column and a long text column called Notes.",
  },
  grist: {
    name: "Grist",
    record: "table rows",
    field: "API key",
    account: { label: "Page link", placeholder: "https://docs.getgrist.com/…/p/…" },
    hint: "On getgrist.com or your own server, from Profile settings → API. The page's table needs text columns called Email and Notes.",
  },
  teable: {
    name: "Teable",
    record: "table records",
    field: "Personal access token",
    account: { label: "Table link", placeholder: "https://app.teable.ai/base/…/table/…" },
    hint: "On app.teable.ai or your own server, with record read, create and update. The table needs a text field called Email and a long text field called Notes.",
  },
} as const satisfies Record<CrmName, Crm>;

/** `account` is what a CRM needs besides the token to find the account, sent alongside it. */
type Crm = { name: string; record: string; field: string; hint: string; account?: { label: string; placeholder: string } };

/** Connects (PUT, with the secret) or disconnects (DELETE) an integration whose secret is write-only, then shows the saved settings. */
function useConnection(projectId: string, path: string) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const call = async (body: Record<string, string | string[]> | undefined, done: string) => {
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

/** For a field with its own Add or Connect button: Enter there presses it. A disabled button already stops an empty or repeat submit. */
const submit = (run: () => void) => (e: React.FormEvent) => {
  e.preventDefault();
  run();
};

/** Every CRM the project syncs to, each disconnected on its own, and a form to connect another. Tokens are write-only, and
 *  connecting checks the token with the CRM first. */
function CrmSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const connected = CRM_NAMES.filter((crm) => settings[`${crm}_connected`]);
  const open = CRM_NAMES.filter((crm) => !settings[`${crm}_connected`]);
  const [picked, setPicked] = useState<CrmName>(open[0] ?? "hubspot");
  // Once the picked CRM connects it leaves the list, so the form moves on to the next one.
  const crm = open.includes(picked) ? picked : open[0];
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Contact className="size-4" /> CRM
        </span>
      }
      description="Adds hot and booked leads to each CRM you connect, with a note holding their reply. Splee matches by email, so people already there get updated, not duplicated."
    >
      {!!connected.length && (
        <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
          {connected.map((c) => (
            <ConnectedCrm key={c} projectId={projectId} settings={settings} crm={c} />
          ))}
        </ul>
      )}
      {crm && <CrmForm key={crm} projectId={projectId} crm={crm} label={connected.length ? "Connect another CRM" : "Connect a CRM"} pick={setPicked} open={open} />}
    </Section>
  );
}

function ConnectedCrm({ projectId, settings, crm }: { projectId: string; settings: ProjectSettings; crm: CrmName }) {
  const { name, record } = CRMS[crm];
  const { busy, call } = useConnection(projectId, crm);
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New hot leads and meetings won't be added to it.`)) call(undefined, `${name} disconnected`);
  };
  return (
    <li className="px-3 py-2.5">
      <div className="flex items-center gap-2">
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-muted-foreground">Adds leads as {record}.</p>
        </div>
        <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
          Disconnect
        </Button>
      </div>
      <LastFailure error={settings[`${crm}_error`]} at={settings[`${crm}_failed_at`]} what="sync" />
    </li>
  );
}

/** Keyed by the CRM, so picking another one starts with empty fields. */
function CrmForm({ projectId, crm, label, pick, open }: { projectId: string; crm: CrmName; label: string; pick: (crm: CrmName) => void; open: CrmName[] }) {
  const { name, field, hint, account }: Crm = CRMS[crm];
  const [token, setToken] = useState("");
  const [accountId, setAccountId] = useState("");
  const { busy, call } = useConnection(projectId, crm);
  const connect = () => call({ token, account: accountId }, `Connected to ${name}`);
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4">
      <Field label={label} className="sm:w-72">
        <Select value={crm} onChange={(e) => pick(e.target.value as CrmName)}>
          {open.map((c) => (
            <option key={c} value={c}>
              {CRMS[c].name}
            </option>
          ))}
        </Select>
      </Field>
      <div className="flex flex-col gap-4 sm:flex-row">
        {account && (
          <Field label={account.label} className="sm:w-72">
            <Input placeholder={account.placeholder} autoComplete="off" value={accountId} onChange={(e) => setAccountId(e.target.value.trim())} />
          </Field>
        )}
        <Field label={field} hint={hint} className="flex-1">
          <div className="flex gap-2">
            {/* The label also wraps Connect, so name the input itself. */}
            <Input aria-label={field} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
            <Button type="submit" loading={busy} disabled={!token || (!!account && !accountId)}>
              Connect
            </Button>
          </div>
        </Field>
      </div>
    </form>
  );
}

const hostOf = (url: string) => {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
};

/** URLs the project sends to, each added, tested and removed on its own, not with the page's Save. */
function EndpointList({
  projectId,
  kind,
  name,
  added,
  confirmRemove,
  labels,
  hint,
  placeholder,
}: {
  projectId: string;
  kind: "chat-channels" | "webhook-endpoints";
  /** What to call a URL in the list and the toasts, like "Slack" or its host. */
  name: (url: string) => string;
  added: (url: string) => string;
  confirmRemove: (url: string) => string;
  labels: { first: string; next: string; input: string };
  hint?: React.ReactNode;
  placeholder: string;
}) {
  const key = `/projects/${projectId}/${kind}`;
  const { data, error, mutate } = useSWR<Endpoint[]>(key, fetcher);
  const { pending, run } = useAction();
  const [url, setUrl] = useState("");
  const add = async () => {
    const done = await run("add", async () => {
      await api(key, { body: { url } });
      await mutate();
    }, added(url));
    if (done) setUrl("");
  };
  const remove = (e: Endpoint) => {
    if (!confirm(confirmRemove(e.url))) return;
    run(e.id, async () => {
      await api(`/${kind}/${e.id}`, { method: "DELETE" });
      await mutate();
    }, `Removed ${name(e.url)}`);
  };
  // Without this a list that didn't load looked empty, with Add offered as if nothing was set up yet.
  if (error) return <LoadError error={error} what={kind === "chat-channels" ? "your chats" : "your webhooks"} retry={() => mutate()} />;
  return (
    <>
      {!!data?.length && (
        <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
          {data.map((e) => (
            <li key={e.id} className="px-3 py-2.5">
              <div className="flex items-center gap-2">
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">
                    <Host host={name(e.url)} />
                  </p>
                  {/* Two lines without the scheme, so two URLs on the same host still read apart on a phone. */}
                  <p className="line-clamp-2 font-mono text-xs break-all text-muted-foreground" title={e.url}>
                    {e.url.replace(/^https:\/\//, "")}
                  </p>
                </div>
                <TestButton projectId={projectId} target={kind === "chat-channels" ? "slack" : "webhook"} url={e.url} compact />
                <Button size="sm" variant="ghost" onClick={() => remove(e)} loading={pending === e.id} aria-label={`Remove ${name(e.url)}`}>
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
              <LastFailure error={e.error} at={e.failed_at} />
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={submit(add)}>
        <Field label={data?.length ? labels.next : labels.first} hint={hint}>
          <div className="flex gap-2">
            {/* The label also wraps Add, so name the input itself. */}
            <Input aria-label={labels.input} type="url" value={url} onChange={(ev) => setUrl(ev.target.value.trim())} placeholder={placeholder} />
            <Button type="submit" loading={pending === "add"} disabled={!isUrl(url)}>
              Add
            </Button>
          </div>
        </Field>
      </form>
    </>
  );
}

/** The chats that get the project's alerts. */
function ChatSection({ projectId, weekly, setWeekly }: { projectId: string; weekly: boolean; setWeekly: (on: boolean) => void }) {
  const { data } = useSWR<Endpoint[]>(`/projects/${projectId}/chat-channels`, fetcher);
  const { data: slack } = useSWR<SlackStatus>("/organization/slack", fetcher);
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Hash className="size-4" /> Chat alerts
        </span>
      }
      description="Posts hot leads, booked meetings and due follow-ups to every chat you add, or to your phone."
    >
      <EndpointList
        projectId={projectId}
        kind="chat-channels"
        name={chatApp}
        added={(url) => `Alerts will post to ${chatApp(url)} too`}
        confirmRemove={(url) => `Stop posting alerts to this ${chatApp(url)} chat?`}
        labels={{ first: "Incoming webhook URL", next: "Add another chat", input: "Chat webhook URL" }}
        placeholder="https://hooks.slack.com/…"
      />
      <AppSteps steps={CHAT_APPS} />
      {slack?.available && (
        <p className="mt-3 text-sm text-muted-foreground">
          To send replies and approve emails from Slack, add the Splee app on the{" "}
          <Link href="/app/notifications" className="font-medium text-foreground underline underline-offset-2">
            Notifications page
          </Link>
          .
        </p>
      )}
      <div className="mt-4 flex items-start justify-between gap-4 rounded-lg border border-border px-3 py-2.5">
        <span>
          <span className="block text-sm font-medium">Weekly report</span>
          <span className="block text-xs text-muted-foreground">Mondays at 08:00 UTC: emails, replies, hot leads and spend.</span>
        </span>
        <Switch checked={weekly} onChange={setWeekly} disabled={!data?.length} label="Weekly report" />
      </div>
    </Section>
  );
}

const VERIFIERS = {
  neverbounce: "NeverBounce",
  millionverifier: "MillionVerifier",
  hunter: "Hunter",
  kickbox: "Kickbox",
  bouncer: "Bouncer",
  emailable: "Emailable",
  clearout: "Clearout",
  reoon: "Reoon",
  debounce: "DeBounce",
  emaillistverify: "EmailListVerify",
  bouncify: "Bouncify",
  quickemailverification: "QuickEmailVerification",
  abstract: "Abstract API",
  mailboxvalidator: "MailboxValidator",
  myemailverifier: "MyEmailVerifier",
} as const;

const FINDERS = {
  hunter: "Hunter",
  findymail: "Findymail",
  prospeo: "Prospeo",
  anymailfinder: "Anymail Finder",
  leadmagic: "LeadMagic",
  apollo: "Apollo",
  voilanorbert: "Voila Norbert",
  skrapp: "Skrapp",
  lusha: "Lusha",
} as const;

function VerifierSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  // Without its own finder, Hunter connected here also looks up guessed addresses, as it did before Email finder existed.
  const finds = settings.verifier === "hunter" && !settings.finder ? " Guessed addresses are looked up with Hunter's Email Finder first." : "";
  return (
    <ServiceKeySection
      projectId={projectId}
      path="verifier"
      services={VERIFIERS}
      current={settings.verifier}
      error={settings.verifier_error}
      failedAt={settings.verifier_failed_at}
      title={
        <>
          <MailCheck className="size-4" /> Email verification
        </>
      }
      description="Checks each address before its first email, so bad ones fail here instead of bouncing."
      connected={(name) => `Connected to ${name}. Addresses it can't confirm either way, like catch-all domains, still get their email.${finds}`}
      disconnectWarning="New addresses will be sent to without a check."
      creditHint="Each check uses one of the service's credits."
    />
  );
}

function FinderSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  return (
    <ServiceKeySection
      projectId={projectId}
      path="finder"
      services={FINDERS}
      current={settings.finder}
      error={settings.finder_error}
      failedAt={settings.finder_failed_at}
      title={
        <>
          <UserSearch className="size-4" /> Email finder
        </>
      }
      description="Looks up the real address of a lead whose email was guessed, before their first email."
      connected={(name) => `Connected to ${name}. A found address replaces the guessed one, at the same company only.`}
      disconnectWarning="Guessed addresses will be emailed as they are."
      creditHint="Each address found uses one of the service's credits."
    />
  );
}

/** A service connected with an API key, which is write-only like a CRM token. Connecting checks it with the service first. */
function ServiceKeySection<K extends string>({
  projectId,
  path,
  services,
  current,
  error,
  failedAt,
  title,
  description,
  connected,
  disconnectWarning,
  creditHint,
}: {
  projectId: string;
  path: string;
  services: Record<K, string>;
  current: K | "";
  error: string | null;
  failedAt: string | null;
  title: React.ReactNode;
  description: string;
  connected: (name: string) => string;
  disconnectWarning: string;
  creditHint: string;
}) {
  const [provider, setProvider] = useState(Object.keys(services)[0] as K);
  const [key, setKey] = useState("");
  const { busy, call } = useConnection(projectId, path);
  const connect = async () => {
    if (await call({ provider, key }, `Connected to ${services[provider]}`)) setKey("");
  };
  const name = current ? services[current] : null;
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? ${disconnectWarning}`)) call(undefined, `${name} disconnected`);
  };
  return (
    <Section
      title={<span className="flex items-center gap-2">{title}</span>}
      description={description}
      actions={
        name ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {name ? (
        <p className="text-sm">{connected(name)}</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
          <Field label="Service" className="sm:w-52">
            <Select value={provider} onChange={(e) => setProvider(e.target.value as K)}>
              {(Object.entries(services) as [K, string][]).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="API key" className="flex-1" hint={creditHint}>
            <div className="flex gap-2">
              <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!key}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={error} at={failedAt} what={path === "finder" ? "lookup" : "check"} />
    </Section>
  );
}

/** The token is write-only, like a CRM token. Connecting checks it with Unipile and finds the LinkedIn account on it. */
function UnipileSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [dsn, setDsn] = useState("");
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "unipile");
  const connect = async () => {
    if (await call({ dsn, token }, "Connected to Unipile")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Unipile? Due LinkedIn steps will wait for you to send them, and approved LinkedIn posts will wait too.")) call(undefined, "Unipile disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UserPlus className="size-4" /> LinkedIn sending
        </span>
      }
      description="Sends each due LinkedIn step, and the LinkedIn posts and comments you approve on the Social page, from your own account through Unipile. It also finds decision makers at your target companies."
      actions={
        settings.unipile_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.unipile_connected ? (
        <p className="text-sm text-pretty">
          Sending as {settings.unipile_account_name || "your LinkedIn account"}. Each due step goes out in your sending hours, up to 25 a day, and
          messages wait until they accept the connect note. A reply on LinkedIn stops their sequence and emails. Approved posts and comments go
          out one at a time every ten minutes, up to 5 a day. Finding people searches LinkedIn for decision makers, up to 100 search pages a day.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
          <Field label="DSN" className="sm:w-56" hint="On your Unipile dashboard.">
            <Input placeholder="api8.unipile.com:13851" autoComplete="off" value={dsn} onChange={(e) => setDsn(e.target.value.trim())} />
          </Field>
          <Field label="Access token" className="flex-1" hint="Connect your LinkedIn account in Unipile, then generate a token under Access Tokens.">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!dsn || !token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.unipile_error} at={settings.unipile_failed_at} what="send" />
    </Section>
  );
}

/** The cookies are write-only. Connecting has bird sign in with them and reports the handle. */
function XSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [authToken, setAuthToken] = useState("");
  const [ct0, setCt0] = useState("");
  const { busy, call } = useConnection(projectId, "x");
  const connect = async () => {
    if (await call({ auth_token: authToken, ct0 }, "Connected to X")) {
      setAuthToken("");
      setCt0("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect X? Approved posts for X will wait until you connect it again.")) call(undefined, "X disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <AtSign className="size-4" /> X
        </span>
      }
      description="Searches X as your account and posts what you approve on the Social page."
      actions={
        settings.x_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.x_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.x_handle}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In a browser that&apos;s signed in to x.com, open the dev tools and copy the auth_token and ct0 cookies.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="auth_token" className="flex-1">
              <Input type="password" autoComplete="off" value={authToken} onChange={(e) => setAuthToken(e.target.value.trim())} />
            </Field>
            <Field label="ct0" className="flex-1">
              <Input type="password" autoComplete="off" value={ct0} onChange={(e) => setCt0(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!authToken || !ct0}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.x_error} at={settings.x_failed_at} what="post" />
    </Section>
  );
}

/** The secret and password are write-only. Connecting signs in to Reddit with them and reads the username back. */
function RedditSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ client_id: "", secret: "", username: "", password: "" });
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value });
  const { busy, call } = useConnection(projectId, "reddit");
  const connect = async () => {
    if (await call(form, "Connected to Reddit")) setForm({ client_id: "", secret: "", username: "", password: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Reddit? Approved posts for Reddit will wait until you connect it again.")) call(undefined, "Reddit disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessagesSquare className="size-4" /> Reddit
        </span>
      }
      description="Searches Reddit as your account and posts what you approve on the Social page."
      actions={
        settings.reddit_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.reddit_connected ? (
        <p className="text-sm text-pretty">Posting as u/{settings.reddit_username}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Signed in as the account, open{" "}
            <a href="https://www.reddit.com/prefs/apps" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              reddit.com/prefs/apps
            </a>{" "}
            and create a script app. Any redirect URI works. The client id is the code under the app&apos;s name.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Client id">
              <Input autoComplete="off" value={form.client_id} onChange={set("client_id")} />
            </Field>
            <Field label="Secret">
              <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
            </Field>
            <Field label="Username">
              <Input autoComplete="off" placeholder="u/yourname" value={form.username} onChange={set("username")} />
            </Field>
            <Field label="Password" hint="Accounts with two-factor sign-in can't connect this way.">
              <Input type="password" autoComplete="off" value={form.password} onChange={set("password")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.client_id.trim() || !form.secret.trim() || !form.username.trim() || !form.password} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.reddit_error} at={settings.reddit_failed_at} what="post" />
    </Section>
  );
}

/** The app password is write-only. Connecting signs in to Bluesky with it and reads the handle back. */
function BlueskySection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [handle, setHandle] = useState("");
  const [password, setPassword] = useState("");
  const { busy, call } = useConnection(projectId, "bluesky");
  const connect = async () => {
    if (await call({ handle, password }, "Connected to Bluesky")) {
      setHandle("");
      setPassword("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Bluesky? Approved posts for Bluesky will wait until you connect it again.")) call(undefined, "Bluesky disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Cloud className="size-4" /> Bluesky
        </span>
      }
      description="Searches Bluesky as your account and posts what you approve on the Social page."
      actions={
        settings.bluesky_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.bluesky_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.bluesky_handle}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Signed in as the account, open{" "}
            <a href="https://bsky.app/settings/app-passwords" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              bsky.app/settings/app-passwords
            </a>{" "}
            and add an app password. Splee never needs the account&apos;s own password.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="Handle" className="flex-1">
              <Input autoComplete="off" placeholder="yourname.bsky.social" value={handle} onChange={(e) => setHandle(e.target.value.trim())} />
            </Field>
            <Field label="App password" className="flex-1">
              <Input type="password" autoComplete="off" placeholder="xxxx-xxxx-xxxx-xxxx" value={password} onChange={(e) => setPassword(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!handle || !password}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.bluesky_error} at={settings.bluesky_failed_at} what="post" />
    </Section>
  );
}

/** The token is write-only. Connecting checks it with the instance and reads the account and the instance's post limit back. */
function MastodonSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "mastodon");
  const connect = async () => {
    if (await call({ url, token }, "Connected to Mastodon")) {
      setUrl("");
      setToken("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Mastodon? Approved posts for Mastodon will wait until you connect it again.")) call(undefined, "Mastodon disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Globe className="size-4" /> Mastodon
        </span>
      }
      description="Searches Mastodon as your account and posts what you approve on the Social page."
      actions={
        settings.mastodon_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.mastodon_connected ? (
        <p className="text-sm text-pretty">
          Posting as @{settings.mastodon_handle}, up to 15 posts a day of up to {settings.mastodon_max_chars} characters. Approved posts go out one at a time
          every ten minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            On your instance, open Preferences, then Development, and create an application with the read and write:statuses scopes. Copy its access
            token.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="Instance" className="flex-1">
              <Input autoComplete="off" placeholder="mastodon.social" value={url} onChange={(e) => setUrl(e.target.value.trim())} />
            </Field>
            <Field label="Access token" className="flex-1">
              <Input type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!url || !token}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.mastodon_error} at={settings.mastodon_failed_at} what="post" />
    </Section>
  );
}

/** The token is write-only. Connecting checks it with Threads and reads the account back. */
function ThreadsSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "threads");
  const connect = async () => {
    if (await call({ token }, "Connected to Threads")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Threads? Approved posts for Threads will wait until you connect it again.")) call(undefined, "Threads disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Spool className="size-4" /> Threads
        </span>
      }
      description="Searches Threads as your account and posts what you approve on the Social page."
      actions={
        settings.threads_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.threads_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.threads_username}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://developers.facebook.com/apps" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Meta for Developers
            </a>
            , create an app with the Threads API use case and the threads_basic, threads_content_publish, threads_read_replies and
            threads_keyword_search permissions. Add your account as a Threads tester, then generate a long-lived access token.
          </p>
          <Field label="Access token" hint="It lasts 60 days. Until Meta approves keyword search for the app, searches only find your own posts.">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.threads_error} at={settings.threads_failed_at} what="post" />
    </Section>
  );
}

/** The client secret and refresh token are write-only. Connecting trades the token with Google and reads the channel back. */
function YouTubeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ client_id: "", secret: "", refresh_token: "" });
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const { busy, call } = useConnection(projectId, "youtube");
  const connect = async () => {
    if (await call(form, "Connected to YouTube")) setForm({ client_id: "", secret: "", refresh_token: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect YouTube? Approved comments for YouTube will wait until you connect it again.")) call(undefined, "YouTube disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <TvMinimalPlay className="size-4" /> YouTube
        </span>
      }
      description="Searches YouTube for videos and posts the comments you approve on the Social page as your channel."
      actions={
        settings.youtube_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.youtube_connected ? (
        <p className="text-sm text-pretty">
          Commenting as {settings.youtube_channel}, up to 15 comments a day. Approved comments go out one at a time every ten minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In Google Cloud, turn on the YouTube Data API v3, create an OAuth client and publish the app, since tokens from an app in testing
            expire in 7 days. Then in the{" "}
            <a href="https://developers.google.com/oauthplayground" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              OAuth Playground
            </a>
            , use your own client, sign in as the channel with the youtube.force-ssl scope and copy the refresh token.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Client id">
              <Input autoComplete="off" placeholder="1234.apps.googleusercontent.com" value={form.client_id} onChange={set("client_id")} />
            </Field>
            <Field label="Client secret">
              <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
            </Field>
            <Field label="Refresh token" className="sm:col-span-2">
              <Input type="password" autoComplete="off" value={form.refresh_token} onChange={set("refresh_token")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.client_id || !form.secret || !form.refresh_token} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.youtube_error} at={settings.youtube_failed_at} what="post" />
    </Section>
  );
}

/** Stack Exchange's OAuth dialog for an app with client side OAuth. It ends on a page with the token in its address. */
const stackExchangeDialog = (clientId: string) =>
  `https://stackoverflow.com/oauth/dialog?${new URLSearchParams({
    client_id: clientId,
    scope: "write_access,no_expiry",
    redirect_uri: "https://stackexchange.com/oauth/login_success",
  })}`;

/** The access token is write-only. Connecting checks that it can write and that its user has an account on the site. The client
 *  id only opens the dialog that makes the token, so it isn't sent. */
function StackExchangeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ key: "", token: "", site: "" });
  const [clientId, setClientId] = useState("");
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const { busy, call } = useConnection(projectId, "stackexchange");
  const connect = async () => {
    if (await call(form, "Connected to Stack Exchange")) setForm({ key: "", token: "", site: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Stack Exchange? Approved answers will wait until you connect it again.")) call(undefined, "Stack Exchange disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessageCircleQuestionMark className="size-4" /> Stack Exchange
        </span>
      }
      description="Searches one Stack Exchange site, like Stack Overflow, for open questions and posts the answers you approve on the Social page."
      actions={
        settings.stackexchange_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.stackexchange_connected ? (
        <p className="text-sm text-pretty">
          Answering as {settings.stackexchange_user} on {settings.stackexchange_site}, up to 5 answers a day. Approved answers go out one at a time every ten
          minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            On{" "}
            <a href="https://stackapps.com/apps/oauth/register" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Stack Apps
            </a>
            , register an app with client side OAuth turned on and give it a Stack Apps post, which apps need before they can write. Copy its key, then
            get an access token with its client id.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Key">
              <Input autoComplete="off" value={form.key} onChange={set("key")} />
            </Field>
            <Field label="Site" hint="One site per project.">
              <Input autoComplete="off" placeholder="stackoverflow.com" value={form.site} onChange={set("site")} />
            </Field>
            <Field label="Client id" hint="Copy access_token from the address it ends on.">
              <div className="flex gap-2">
                <Input aria-label="Client id" autoComplete="off" inputMode="numeric" value={clientId} onChange={(e) => setClientId(e.target.value.trim())} />
                <Button
                  type="button"
                  variant="outline"
                  disabled={!clientId}
                  onClick={() => window.open(stackExchangeDialog(clientId), "_blank", "noreferrer")}
                >
                  Get token
                </Button>
              </div>
            </Field>
            <Field label="Access token">
              <Input type="password" autoComplete="off" value={form.token} onChange={set("token")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.key || !form.token || !form.site} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.stackexchange_error} at={settings.stackexchange_failed_at} what="post" />
    </Section>
  );
}

/** The API key is write-only. Connecting checks it with DEV and reads the username back. */
function DevSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [key, setKey] = useState("");
  const { busy, call } = useConnection(projectId, "dev");
  const connect = async () => {
    if (await call({ key }, "Connected to DEV")) setKey("");
  };
  const disconnect = () => {
    if (confirm("Disconnect DEV? Approved articles will wait until you connect it again.")) call(undefined, "DEV disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Newspaper className="size-4" /> DEV
        </span>
      }
      description="Publishes the articles you approve on the Social page from your dev.to account. DEV can't take comments from apps, so it gets articles only."
      actions={
        settings.dev_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.dev_connected ? (
        <p className="text-sm text-pretty">Publishing as @{settings.dev_username}, up to 2 articles a day. Approved articles go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://dev.to/settings/extensions" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              DEV&apos;s extension settings
            </a>
            , generate an API key.
          </p>
          <Field label="API key">
            <div className="flex gap-2">
              <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!key}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.dev_error} at={settings.dev_failed_at} what="post" />
    </Section>
  );
}

/** Each forum is added and removed on its own, not with the page's Save. Keys are write-only, and adding checks the key with
 *  the forum and reads back the user it posts as. Adding the same user again replaces the key and the category. */
function DiscourseSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/discourse-forums`;
  const { data, error, mutate } = useSWR<DiscourseForum[]>(key, fetcher);
  const { pending, run } = useAction();
  const empty = { url: "", key: "", username: "", category: "" };
  const [form, setForm] = useState(empty);
  const set = (field: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [field]: e.target.value });
  const add = async () => {
    const done = await run("add", async () => {
      await api(key, { body: { ...form, url: form.url.trim(), key: form.key.trim() } });
      await mutate();
    }, `Joined ${hostOf(`https://${form.url.trim().replace(/^https?:\/\//, "")}`)}`);
    if (done) setForm(empty);
  };
  const remove = (f: DiscourseForum) => {
    if (!confirm(`Leave ${hostOf(f.url)}? Approved posts for it go back to drafts.`)) return;
    run(f.id, async () => {
      await api(`/discourse-forums/${f.id}`, { method: "DELETE" });
      await mutate();
    }, `Left ${hostOf(f.url)}`);
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessageSquareText className="size-4" /> Discourse forums
        </span>
      }
      description="Searches the forums you join for recent topics where your buyers ask for help, and posts the replies you approve on the Social page."
    >
      {error ? (
        <LoadError error={error} what="your forums" retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((f) => (
                <li key={f.id} className="px-3 py-2.5">
                  <div className="flex items-center gap-2">
                    <div className="min-w-0 flex-1">
                      <p className="text-sm font-medium">
                        <Host host={f.url.replace(/^https:\/\//, "")} />
                      </p>
                      <p className="text-xs text-pretty text-muted-foreground">
                        Posting as {f.username}, up to 5 a day. {f.category_id ? `New topics go in ${f.category_name}.` : "Replies only."}
                      </p>
                    </div>
                    <Button size="sm" variant="ghost" onClick={() => remove(f)} loading={pending === f.id} aria-label={`Leave ${hostOf(f.url)}`}>
                      <Trash2 className="size-3.5" />
                    </Button>
                  </div>
                  <LastFailure error={f.error} at={f.failed_at} what="post" />
                </li>
              ))}
            </ul>
          )}
          <form onSubmit={submit(add)} className="flex flex-col gap-4">
            <p className="text-sm text-pretty text-muted-foreground">
              On a forum you run, open Admin, then API, and make a key for your own user. On other forums, paste a User API Key and leave the
              username empty.
            </p>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={data?.length ? "Another forum" : "Forum"}>
                <Input autoComplete="off" placeholder="forum.example.com" value={form.url} onChange={set("url")} />
              </Field>
              <Field label="API key">
                <Input type="password" autoComplete="off" value={form.key} onChange={set("key")} />
              </Field>
              <Field label="Username" hint="Only for a key an admin made.">
                <Input autoComplete="off" value={form.username} onChange={set("username")} />
              </Field>
              <Field label="Category for new topics" hint="Leave it empty for replies only.">
                <Input autoComplete="off" placeholder="Name or link" value={form.category} onChange={set("category")} />
              </Field>
            </div>
            <Button type="submit" loading={pending === "add"} disabled={!form.url.trim() || !form.key.trim()} className="sm:self-start">
              Join forum
            </Button>
          </form>
        </>
      )}
    </Section>
  );
}

/** The password is write-only. Connecting signs in to the instance and reads the account back. */
function LemmySection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ url: "", username: "", password: "" });
  const { busy, call } = useConnection(projectId, "lemmy");
  const connect = async () => {
    if (await call(form, "Connected to Lemmy")) setForm({ url: "", username: "", password: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Lemmy? Approved posts for Lemmy will wait until you connect it again.")) call(undefined, "Lemmy disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UsersRound className="size-4" /> Lemmy
        </span>
      }
      description="Searches Lemmy communities as your account and posts what you approve on the Social page."
      actions={
        settings.lemmy_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.lemmy_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.lemmy_handle}, up to 10 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Splee signs in as your account on its instance. Accounts that sign in with two-factor codes can&apos;t be used.
          </p>
          <div className="grid gap-4 sm:grid-cols-3">
            <Field label="Instance">
              <Input autoComplete="off" placeholder="lemmy.world" value={form.url} onChange={(e) => setForm({ ...form, url: e.target.value.trim() })} />
            </Field>
            <Field label="Username">
              <Input autoComplete="off" value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value.trim() })} />
            </Field>
            <Field label="Password">
              <Input type="password" autoComplete="off" value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.url || !form.username || !form.password} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.lemmy_error} at={settings.lemmy_failed_at} what="post" />
    </Section>
  );
}

const hostOfRelay = (r: string) => r.replace(/^wss:\/\//, "");

/** "a", "a and b", "a, b and c". */
const listOf = (items: string[]) => (items.length < 2 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`);

/** The secret key is write-only. Connecting reads the npub from it and checks that each relay is wss:// at a public address. */
function NostrSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ key: "", relays: "" });
  const { busy, call } = useConnection(projectId, "nostr");
  const connect = async () => {
    if (await call({ key: form.key, relays: [form.relays] }, "Connected to Nostr")) setForm({ key: "", relays: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Nostr? Approved notes will wait until you connect it again.")) call(undefined, "Nostr disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Zap className="size-4" /> Nostr
        </span>
      }
      description="Searches Nostr for notes where your buyers ask for help, and posts the notes and replies you approve on the Social page with your key."
      actions={
        settings.nostr_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.nostr_connected ? (
        <div className="flex flex-col gap-1.5 text-sm text-pretty">
          <p>Posting as</p>
          <p className="rounded-lg bg-muted/60 px-3 py-2 font-mono text-xs break-all">{settings.nostr_npub}</p>
          <p>Notes go to {listOf(settings.nostr_relays.map(hostOfRelay))}, up to 15 a day, one every ten minutes.</p>
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Splee signs each note with your secret key and sends it to your relays. Use a key you made for the company, not your own.
          </p>
          <Field label="Secret key">
            <Input type="password" autoComplete="off" placeholder="nsec1…" value={form.key} onChange={(e) => setForm({ ...form, key: e.target.value.trim() })} />
          </Field>
          <Field label="Relays" hint="One per line. Leave it empty for relay.damus.io, nos.lol and relay.nostr.band.">
            <Textarea
              rows={3}
              autoComplete="off"
              placeholder="wss://relay.damus.io"
              value={form.relays}
              onChange={(e) => setForm({ ...form, relays: e.target.value })}
              className="min-h-0"
            />
          </Field>
          <Button type="submit" loading={busy} disabled={!form.key} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.nostr_error} at={settings.nostr_failed_at} what="post" />
    </Section>
  );
}

/** Connects by signing in to Tumblr, which only shows when the server has a Tumblr app. Tumblr sends the browser back here
 *  with how it went. */
function TumblrSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { pending, run } = useAction();
  const { busy, call } = useConnection(projectId, "tumblr");
  const { mutate } = useSWRConfig();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("tumblr") === "connected";
    const failed = params.get("tumblr_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) {
      toast.success("Connected to Tumblr");
      mutate(`/projects/${projectId}/settings`);
    }
  }, [mutate, projectId]);

  if (!me?.tumblr_sign_in && !settings.tumblr_connected) return null;
  const signIn = () =>
    run("tumblr", async () => {
      const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/tumblr/oauth`, { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (confirm("Disconnect Tumblr? Approved posts and reblogs will wait until you connect it again.")) call(undefined, "Tumblr disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Repeat2 className="size-4" /> Tumblr
        </span>
      }
      description="Searches Tumblr tags for posts where your buyers ask for help, and posts what you approve on the Social page from your blog."
      actions={
        settings.tumblr_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      <p className="text-sm text-pretty text-muted-foreground">
        Tumblr has no replies for apps, so Splee answers a post by reblogging it to your blog with your comment on top.
      </p>
      {settings.tumblr_connected ? (
        <div className="mt-4 flex flex-col gap-3">
          <Field label="Post as" hint={`Signed in as ${settings.tumblr_username}. Up to 10 posts and reblogs a day, one every ten minutes.`}>
            <Select value={settings.tumblr_blog} disabled={busy} onChange={(e) => call({ blog: e.target.value }, `Posting as ${e.target.value}`)} className="sm:w-72">
              {settings.tumblr_blogs.map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
            </Select>
          </Field>
        </div>
      ) : (
        <Button onClick={signIn} loading={pending === "tumblr"} className="mt-4">
          Connect Tumblr
        </Button>
      )}
      <LastFailure error={settings.tumblr_error} at={settings.tumblr_failed_at} what="post" />
    </Section>
  );
}

/** The token is write-only. Connecting checks it with Hashnode and reads back the username and publications. */
function HashnodeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "hashnode");
  const publication = useConnection(projectId, "hashnode/publication");
  const connect = async () => {
    if (await call({ token }, "Connected to Hashnode")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Hashnode? Approved articles and comments will wait until you connect it again.")) call(undefined, "Hashnode disconnected");
  };
  const publications = settings.hashnode_publications;
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <PenLine className="size-4" /> Hashnode
        </span>
      }
      description="Searches recent Hashnode articles by tag, comments on the ones you approve on the Social page, and publishes your articles."
      actions={
        settings.hashnode_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.hashnode_connected ? (
        <div className="flex flex-col gap-3 text-sm text-pretty">
          <p>Writing as @{settings.hashnode_username}, up to 2 articles and 10 comments a day, one every ten minutes.</p>
          {publications.length > 0 ? (
            <Field label="Publish articles in">
              <Select
                value={settings.hashnode_publication_id}
                disabled={publication.busy}
                onChange={(e) => publication.call({ publication_id: e.target.value }, "Publication saved")}
                className="sm:w-72"
              >
                {publications.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.title || p.url}
                  </option>
                ))}
              </Select>
            </Field>
          ) : (
            <p className="text-muted-foreground">The account has no blog yet, so only comments go out. Start one on Hashnode, then connect again.</p>
          )}
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://hashnode.com/settings/developer" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Hashnode&apos;s developer settings
            </a>
            , generate a personal access token.
          </p>
          <Field label="Access token">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.hashnode_error} at={settings.hashnode_failed_at} what="post" />
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
      <RefreshCw className="size-3.5" /> Replace
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
      description="Whoever fills in one of your forms becomes a lead and pings your chat."
    >
      <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} />
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-4 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Form webhook URL</span>
        <CopyField value={`${origin}/api/forms/${token}${segmentId ? `?campaign=${segmentId}` : ""}`} />
        <p className="text-xs text-pretty text-muted-foreground">
          Anything that can POST <code>{`{"email": "...", "name": "..."}`}</code> works too. Answers that aren&apos;t a name, email,
          company, title, website or LinkedIn go in the lead&apos;s notes.
        </p>
        <AppSteps steps={FORM_APPS} />
      </div>
    </Section>
  );
}

/** The visitors snippet, where page views may come from, and the pages that alert. Saved on its own, not with the page. */
function VisitorsSection({ projectId }: { projectId: string }) {
  const origin = useOrigin();
  const key = `/projects/${projectId}/visitors/site`;
  const { data, error, mutate } = useSWR<VisitorSite>(key, fetcher);
  const { pending, run } = useAction();
  const [draft, setDraft] = useState<{ domains: string[]; intent_paths: string[] } | null>(null);
  const [checked, setChecked] = useState(false);
  const form = draft ?? { domains: data?.domains ?? [], intent_paths: data?.intent_paths ?? [] };
  const save = () =>
    run("save", async () => {
      await mutate(await api<VisitorSite>(key, { method: "PUT", body: form }), { revalidate: false });
      setDraft(null);
    }, "Saved");
  const check = () =>
    run("check", async () => {
      await mutate();
      setChecked(true);
    });
  const last = data?.last_hit_at
    ? `The last page view came ${timeAgo(data.last_hit_at)}${data.last_hit_path ? `, on ${data.last_hit_path}` : ""}.`
    : checked
      ? "No page views yet. Open your site in a new tab, then check again."
      : "No page views yet.";
  return (
    <div id="visitors" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <Eye className="size-4" /> Website visitors
          </span>
        }
        description="Shows which companies visit your site and the pages they read, on the Companies page."
      >
        {error ? (
          <LoadError error={error} what="the snippet" retry={() => mutate()} />
        ) : !data ? null : (
          <div className="flex flex-col gap-4">
            <div className="flex flex-col gap-1.5">
              <CodeBlock code={`<script async src="${origin}/api/v/${data.site_key}.js"></script>`} label="Add to every page" wrap />
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
                <Button size="sm" variant="outline" onClick={check} loading={pending === "check"}>
                  Check install
                </Button>
                <p className="text-sm text-pretty text-muted-foreground" role="status">
                  {last}
                </p>
              </div>
            </div>
            <Field label="Your site's domains" hint={<span className="text-pretty">Page views from anywhere else are ignored. Subdomains count too.</span>} group>
              <ChipInput value={form.domains} onChange={(domains) => setDraft({ ...form, domains })} placeholder="example.com" />
            </Field>
            <Field
              label="High-intent pages"
              hint={
                <span className="text-pretty">
                  Alerts your chats when a company in a campaign, or one that read 3 pages today, opens one, once a day per company.
                </span>
              }
              group
            >
              <ChipInput value={form.intent_paths} onChange={(intent_paths) => setDraft({ ...form, intent_paths })} placeholder="/pricing, /demo" />
            </Field>
            <p className="text-xs text-pretty text-muted-foreground">
              The snippet sets no cookies and sends nothing when the browser asks not to be tracked. Splee never stores a visitor&apos;s IP
              address, only a keyed hash of it, and deletes page views after 180 days.
            </p>
            <Button onClick={save} loading={pending === "save"} disabled={!draft} className="sm:self-start">
              Save
            </Button>
          </div>
        )}
      </Section>
    </div>
  );
}

export default function IntegrationsPage({ params }: PageProps<"/app/p/[projectId]/integrations">) {
  const { projectId } = use(params);
  const origin = useOrigin();
  return (
    <ProjectSettingsPage
      title="Integrations"
      description="Get told the moment a lead replies, in your own tools."
    >
      {(s, update) => (
        <>
          <ChatSection projectId={projectId} weekly={s.chat_weekly_report} setWeekly={(chat_weekly_report) => update({ chat_weekly_report })} />

          <CrmSection projectId={projectId} settings={s} />
          <Section
            title={
              <span className="flex items-center gap-2">
                <MailPlus className="size-4" /> BCC copies
              </span>
            }
            description="Copies every email Splee sends to one address, so your CRM logs the conversation."
          >
            <Field label="BCC address" hint="HubSpot, Pipedrive, Close, Capsule and most other CRMs give you one. Leave it empty for no copies.">
              <Input type="email" autoComplete="off" value={s.bcc_email} onChange={(e) => update({ bcc_email: e.target.value.trim() })} placeholder="1234567@bcc.hubspot.com" />
            </Field>
            <LastFailure error={s.bcc_error} at={s.bcc_failed_at} what="copy" />
          </Section>
          <VerifierSection projectId={projectId} settings={s} />
          <FinderSection projectId={projectId} settings={s} />
          <UnipileSection projectId={projectId} settings={s} />
          <XSection projectId={projectId} settings={s} />
          <RedditSection projectId={projectId} settings={s} />
          <BlueskySection projectId={projectId} settings={s} />
          <MastodonSection projectId={projectId} settings={s} />
          <ThreadsSection projectId={projectId} settings={s} />
          <YouTubeSection projectId={projectId} settings={s} />
          <StackExchangeSection projectId={projectId} settings={s} />
          <DevSection projectId={projectId} settings={s} />
          <DiscourseSection projectId={projectId} />
          <LemmySection projectId={projectId} settings={s} />
          <NostrSection projectId={projectId} settings={s} />
          <TumblrSection projectId={projectId} settings={s} />
          <HashnodeSection projectId={projectId} settings={s} />

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
                  <code>from</code>, <code>to</code> and <code>message_id</code>. <code>follow_up_due</code> sends <code>&quot;reply&quot;: null</code> and a{" "}
                  <code>reminder</code> with its <code>at</code> time and the lead&apos;s <code>notes</code>. <code>meeting_rescheduled</code> sends{" "}
                  <code>&quot;reply&quot;: null</code> and the old time as <code>previous_meeting_at</code>. <code>form_submitted</code> sends{" "}
                  <code>&quot;reply&quot;: null</code> and a <code>form</code> whose <code>answers</code> each have a <code>question</code> and an{" "}
                  <code>answer</code>. <code>status_changed</code> sends <code>&quot;reply&quot;: null</code> and the old status as{" "}
                  <code>previous_status</code>. &quot;Send a test&quot; posts the hot_lead shape with{" "}
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
          <FormSection projectId={projectId} token={s.form_token} />
          <VisitorsSection projectId={projectId} />
          <Section
            title={
              <span className="flex items-center gap-2">
                <CalendarCheck className="size-4" /> Booking tools
              </span>
            }
            description="When a lead books a call, Splee marks them Meeting booked and stops emailing them. If they cancel, they go back to Hot. Your chat channel and webhook hear about both."
          >
            {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
            <div className="flex flex-col gap-1.5">
              <span className="text-sm font-medium">Booking webhook URL</span>
              <CopyField value={`${origin}/api/bookings/${s.booking_token}`} />
              <p className="text-xs text-pretty text-muted-foreground">
                Splee finds the lead by email and shows the call&apos;s time on it. Anything else, like a Zapier step, can POST{" "}
                <code>{`{"email": "...", "start_time": "..."}`}</code>.
              </p>
              <AppSteps steps={BOOKING_APPS} />
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
