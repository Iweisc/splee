"use client";

import { use, useEffect, useState } from "react";
import Link from "next/link";
import {
  ArrowDown,
  ArrowUp,
  AtSign,
  Bot,
  CalendarCheck,
  CalendarClock,
  ClipboardList,
  Cloud,
  Contact,
  CreditCard,
  Eye,
  Globe,
  Hash,
  MailCheck,
  MailPlus,
  MapPin,
  Megaphone,
  MessageCircleQuestionMark,
  MessageSquareText,
  MessagesSquare,
  Mic,
  Newspaper,
  NotebookPen,
  PenLine,
  RefreshCw,
  Repeat2,
  Send,
  Share2,
  Smartphone,
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
import { GoogleSheetsSection } from "@/components/app/settings/google-sheets";
import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { AudienceNote } from "@/components/app/work/ad-audiences";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge, Host, Tabs } from "@/components/ui/misc";
import { Field, Input, NumberInput, Select, Switch, Textarea } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import {
  AD_PLATFORM_NAMES,
  type AdConnection,
  type AdPlatform,
  type AdsStatus,
  type Blog,
  type BlogTest,
  CRM_NAMES,
  type CrmName,
  type DiscourseForum,
  type Endpoint,
  FINDER_NAMES,
  type FinderName,
  type Finders,
  type HookEvent,
  type HookSubscription,
  type LocalUsage,
  MEETING_NAMES,
  MEETING_PROVIDERS,
  type MeetingConnection,
  type MeetingProvider,
  PAYMENT_NAMES,
  PAYMENT_PROVIDERS,
  type PaymentConnection,
  type PaymentProvider,
  type ProjectSettings,
  type SignInCrm,
  signsIn,
  type SocialChannel,
  type SlackStatus,
  type VisitorSite,
  type TwilioStatus,
  type WebhookDelivery,
  type WebhookEvent,
} from "@/lib/types";
import { dateTime, num, plural, timeAgo } from "@/lib/utils";

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
  salesforce: {
    name: "Salesforce",
    record: "leads",
    signIn: true,
    hint: "A lead whose email is already in Salesforce updates that contact or lead instead. Their reply goes in a task.",
  },
  zoho: {
    name: "Zoho CRM",
    record: "leads",
    signIn: true,
    hint: "Hot and booked leads go to the Leads module, matched by email, in whichever Zoho data centre your account is in.",
  },
  dynamics: {
    name: "Dynamics 365 Sales",
    record: "leads",
    signIn: true,
    hint: "A lead whose email is already in Dynamics 365 gets updated instead. Their reply goes in a note on the lead.",
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

/** `account` is what a CRM needs besides the token to find the account, sent alongside it. A CRM with `signIn` connects by
 *  signing in instead. */
type Crm = TokenCrm | { name: string; record: string; hint: string; signIn: true };
type TokenCrm = { name: string; record: string; hint: string; field: string; account?: { label: string; placeholder: string } };

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

/** Sends the browser to the CRM's sign-in page. It comes back to this page with how it went, which CrmSection shows. */
async function signInToCrm(projectId: string, crm: SignInCrm, body: { sandbox?: boolean; org_url?: string }) {
  const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/${crm}/oauth`, { method: "POST", body });
  window.location.assign(url);
}

/** Every CRM the project syncs to, each tested and disconnected on its own, and a form to connect another. Tokens are
 *  write-only, and connecting checks the token with the CRM first. A CRM that signs in is only offered when the server has
 *  an app for it. */
function CrmSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { mutate } = useSWRConfig();
  const connected = CRM_NAMES.filter((crm) => settings[`${crm}_connected`]);
  const open = CRM_NAMES.filter((crm) => !settings[`${crm}_connected`] && (!signsIn(crm) || me?.crm_sign_in.includes(crm)));
  const [picked, setPicked] = useState<CrmName>(open[0] ?? "hubspot");
  // Once the picked CRM connects it leaves the list, so the form moves on to the next one.
  const crm = open.includes(picked) ? picked : open[0];

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("crm");
    const failed = params.get("crm_error");
    if (!done && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done && done in CRMS) {
      toast.success(`Connected to ${CRMS[done as CrmName].name}`);
      mutate(`/projects/${projectId}/settings`);
    }
  }, [mutate, projectId]);

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
  const status = useConnection(projectId, `${crm}/status`);
  const { pending, run } = useAction();
  const { mutate } = useSWRConfig();
  const key = `/projects/${projectId}/settings`;
  const site = signsIn(crm) ? settings[`${crm}_site`] : "";
  const reconnect = signsIn(crm) && settings[`${crm}_needs_reconnect`];
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New hot leads and meetings won't be added to it.`)) call(undefined, `${name} disconnected`);
  };
  // A failed test is saved like a failed sync, so the settings are read again either way.
  const test = () =>
    run("test", async () => {
      try {
        await mutate(key, await api<ProjectSettings>(`/projects/${projectId}/integrations/${crm}/test`, { method: "POST" }), { revalidate: false });
      } catch (e) {
        await mutate(key);
        throw e;
      }
    }, `${name} is working`);
  const signInAgain = () =>
    run("reconnect", () => signInToCrm(projectId, crm as SignInCrm, { sandbox: settings.salesforce_sandbox, org_url: `https://${settings.dynamics_site}` }));
  return (
    <li className="px-3 py-2.5">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs break-all text-muted-foreground">{site ? `Signed in to ${site}` : `Adds leads as ${record}.`}</p>
        </div>
        <div className="flex gap-2">
          {reconnect ? (
            <Button size="sm" onClick={signInAgain} loading={pending === "reconnect"}>
              Reconnect
            </Button>
          ) : (
            <Button size="sm" variant="outline" onClick={test} loading={pending === "test"}>
              Test
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        </div>
      </div>
      {crm === "salesforce" && (
        <Field label="Status for booked leads" hint="From your org's Lead Status picklist." className="mt-3 sm:w-80">
          <Select
            value={settings.salesforce_booked_status}
            disabled={status.busy}
            onChange={(e) => status.call({ status: e.target.value }, e.target.value ? `Booked leads become ${e.target.value}` : "Booked leads keep their status")}
          >
            <option value="">Leave it as it is</option>
            {settings.salesforce_statuses.map((s) => (
              <option key={s.value} value={s.value}>
                {s.label}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <LastFailure error={settings[`${crm}_error`]} at={settings[`${crm}_failed_at`]} what="sync" />
    </li>
  );
}

/** Keyed by the CRM, so picking another one starts with empty fields. */
function CrmForm({ projectId, crm, label, pick, open }: { projectId: string; crm: CrmName; label: string; pick: (crm: CrmName) => void; open: CrmName[] }) {
  const c: Crm = CRMS[crm];
  const picker = (
    <Field label={label} className="sm:w-72">
      <Select value={crm} onChange={(e) => pick(e.target.value as CrmName)}>
        {open.map((c) => (
          <option key={c} value={c}>
            {CRMS[c].name}
          </option>
        ))}
      </Select>
    </Field>
  );
  if ("signIn" in c) return <SignInCrmForm projectId={projectId} crm={crm as SignInCrm} picker={picker} />;
  return <TokenCrmForm projectId={projectId} crm={crm} crmInfo={c} picker={picker} />;
}

function TokenCrmForm({ projectId, crm, crmInfo, picker }: { projectId: string; crm: CrmName; crmInfo: TokenCrm; picker: React.ReactNode }) {
  const { name, field, hint, account } = crmInfo;
  const [token, setToken] = useState("");
  const [accountId, setAccountId] = useState("");
  const { busy, call } = useConnection(projectId, crm);
  const connect = () => call({ token, account: accountId }, `Connected to ${name}`);
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4">
      {picker}
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

function SignInCrmForm({ projectId, crm, picker }: { projectId: string; crm: SignInCrm; picker: React.ReactNode }) {
  const { name, hint } = CRMS[crm];
  const [orgUrl, setOrgUrl] = useState("");
  const [sandbox, setSandbox] = useState(false);
  const { pending, run } = useAction();
  const signIn = () => run("sign-in", () => signInToCrm(projectId, crm, { sandbox, org_url: orgUrl }));
  return (
    <form onSubmit={submit(signIn)} className="flex flex-col gap-4">
      {picker}
      {crm === "dynamics" && (
        <Field label="Org URL" hint="The address Dynamics 365 opens at." className="sm:w-96">
          <Input placeholder="https://acme.crm.dynamics.com" autoComplete="off" value={orgUrl} onChange={(e) => setOrgUrl(e.target.value.trim())} />
        </Field>
      )}
      {crm === "salesforce" && (
        <div className="flex items-center gap-3">
          <Switch checked={sandbox} onChange={setSandbox} label="Sandbox org" />
          <span>
            <span className="block text-sm">Sandbox org</span>
            <span className="block text-xs text-muted-foreground">Signs in at test.salesforce.com</span>
          </span>
        </div>
      )}
      <div className="flex flex-col gap-2">
        <Button type="submit" loading={pending === "sign-in"} disabled={crm === "dynamics" && !orgUrl} className="self-start">
          Sign in to {name}
        </Button>
        <p className="text-xs text-pretty text-muted-foreground">{hint}</p>
      </div>
    </form>
  );
}

/** Where each provider keeps the key and the webhook. Event names are as the provider writes them. */
const PAYMENTS: Record<PaymentProvider, { key: string; keyHint: string; webhook: React.ReactNode; secretHint: string }> = {
  stripe: {
    key: "Restricted key",
    keyHint: "In Stripe, Developers → API keys → Create restricted key, with Read on Customers, Charges and Refunds.",
    webhook: (
      <>
        In Stripe, Developers → Webhooks → Add endpoint with this URL and the events <code>charge.succeeded</code> and <code>charge.refunded</code>.
      </>
    ),
    secretHint: "From the endpoint's page in Stripe. It starts with whsec_.",
  },
  paddle: {
    key: "API key",
    keyHint: "In Paddle, Developer tools → Authentication → New API key, with Read on transactions, customers and adjustments.",
    webhook: (
      <>
        In Paddle, Developer tools → Notifications → New destination with this URL and the events <code>transaction.completed</code> and{" "}
        <code>adjustment.updated</code>.
      </>
    ),
    secretHint: "The destination's secret key, from its menu in Paddle.",
  },
  lemonsqueezy: {
    key: "API key",
    keyHint: "In Lemon Squeezy, Settings → API → New API key.",
    webhook: (
      <>
        In Lemon Squeezy, Settings → Webhooks → add this URL with a signing secret you make up, and tick <code>order_created</code>,{" "}
        <code>order_refunded</code>, <code>subscription_payment_success</code> and <code>subscription_payment_refunded</code>.
      </>
    ),
    secretHint: "The signing secret you typed in Lemon Squeezy.",
  },
};

function lookBack(c: PaymentConnection) {
  if (c.backfill_status === "queued" || c.backfill_status === "running") return "Looking for payments from the last 90 days…";
  if (c.backfill_status === "failed") return "Couldn't look back over the last 90 days.";
  return `${plural(c.backfill_found, "payment")} in the last 90 days, ${num(c.backfill_matched)} from your leads.`;
}

/** Each provider connected to the project, and a form to connect another. Connecting looks back 90 days, which the list
 *  polls until it's done. Keys and signing secrets are write-only. */
function PaymentsSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/payments`;
  const { data, error, mutate } = useSWR<PaymentConnection[]>(key, fetcher, {
    refreshInterval: (list) => (list?.some((c) => c.backfill_status === "queued" || c.backfill_status === "running") ? 3000 : 0),
  });
  const open = PAYMENT_PROVIDERS.filter((p) => !data?.some((c) => c.provider === p));
  const [picked, setPicked] = useState<PaymentProvider>("stripe");
  // Once the picked provider connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => {
    await mutate(await api<PaymentConnection[]>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <CreditCard className="size-4" /> Payments
        </span>
      }
      description="When a lead, or anyone at their company, pays through Stripe, Paddle or Lemon Squeezy, Splee wins their deal with the amount and counts the revenue on Analytics."
    >
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((c) => (
                <ConnectedPayments key={c.provider} connection={c} call={call} />
              ))}
            </ul>
          )}
          {data && provider && <PaymentForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={data.length ? "Connect another" : "Provider"} />}
        </>
      )}
    </Section>
  );
}

type PaymentCall = (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => Promise<void>;

function ConnectedPayments({ connection: c, call }: { connection: PaymentConnection; call: PaymentCall }) {
  const origin = useOrigin();
  const name = PAYMENT_NAMES[c.provider];
  const { pending, run } = useAction();
  const [secret, setSecret] = useState("");
  const [replacing, setReplacing] = useState(false);
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New payments won't win deals or count as revenue. The ones Splee has stay.`)) run("disconnect", () => call(c.provider, "DELETE"), `${name} disconnected`);
  };
  const save = async () => {
    if (await run("secret", () => call(`${c.provider}/secret`, "PUT", { secret }), "Signing secret saved")) {
      setSecret("");
      setReplacing(false);
    }
  };
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-muted-foreground">{lookBack(c)}</p>
        </div>
        <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
          Disconnect
        </Button>
      </div>
      {c.backfill_status === "failed" && c.backfill_error && <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">{c.backfill_error}</p>}
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-3 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Webhook URL</span>
        <CopyField value={`${origin}/api/payments/${c.provider}/${c.webhook_token}`} />
        <p className="text-xs text-pretty text-muted-foreground">{PAYMENTS[c.provider].webhook}</p>
      </div>
      {c.has_secret && !replacing ? (
        <p className="mt-3 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
          <span>
            Signing secret saved.{" "}
            {c.last_event_at ? (
              <>
                Last event <span title={dateTime(c.last_event_at)}>{timeAgo(c.last_event_at)}</span>.
              </>
            ) : (
              "No events yet."
            )}
          </span>
          <button type="button" onClick={() => setReplacing(true)} className={linkClass}>
            Replace
          </button>
        </p>
      ) : (
        <form onSubmit={submit(save)} className="mt-3">
          <Field label="Signing secret" hint={PAYMENTS[c.provider].secretHint}>
            <div className="flex gap-2">
              <Input aria-label="Signing secret" type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
              <Button type="submit" loading={pending === "secret"} disabled={!secret}>
                Save
              </Button>
            </div>
          </Field>
        </form>
      )}
    </li>
  );
}

/** Keyed by the provider, so picking another one starts with an empty key. */
function PaymentForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: PaymentProvider;
  open: PaymentProvider[];
  pick: (p: PaymentProvider) => void;
  call: PaymentCall;
  label: string;
}) {
  const info = PAYMENTS[provider];
  const [apiKey, setApiKey] = useState("");
  const { pending, run } = useAction();
  const connect = () => run("connect", () => call(provider, "PUT", { api_key: apiKey }), `Connected to ${PAYMENT_NAMES[provider]}`);
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
      <Field label={label} className="sm:w-52">
        <Select value={provider} onChange={(e) => pick(e.target.value as PaymentProvider)}>
          {open.map((p) => (
            <option key={p} value={p}>
              {PAYMENT_NAMES[p]}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={info.key} hint={info.keyHint} className="flex-1">
        <div className="flex gap-2">
          {/* The label also wraps Connect, so name the input itself. */}
          <Input aria-label={info.key} type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value.trim())} />
          <Button type="submit" loading={pending === "connect"} disabled={!apiKey}>
            Connect
          </Button>
        </div>
      </Field>
    </form>
  );
}

type MeetingField = { name: string; label: string; secret?: boolean };

/** What each provider asks for and where its webhook goes. Gong has no webhook: Splee reads its calls every 15 minutes. */
const MEETING_SETUP: Record<MeetingProvider, { fields: MeetingField[]; hint: string; webhook?: React.ReactNode; secretHint?: string }> = {
  fireflies: {
    fields: [{ name: "api_key", label: "API key", secret: true }],
    hint: "In Fireflies, Integrations → Fireflies API → copy your API key.",
    webhook: <>In Fireflies, Settings → Developer settings → Webhook, paste this URL and set a webhook secret.</>,
    secretHint: "The webhook secret you set in Fireflies.",
  },
  fathom: {
    fields: [{ name: "api_key", label: "API key", secret: true }],
    hint: "In Fathom, Settings → API Access → Generate API key.",
    webhook: <>In Fathom, Settings → API Access → Add webhook with this URL, with the transcript and summary included.</>,
    secretHint: "The webhook's secret from Fathom. It starts with whsec_.",
  },
  gong: {
    fields: [
      { name: "access_key", label: "Access key" },
      { name: "access_secret", label: "Access key secret", secret: true },
    ],
    hint: "In Gong, Company settings → Ecosystem → API → Create. A technical admin can make one.",
  },
  zoom: {
    fields: [
      { name: "account_id", label: "Account ID" },
      { name: "client_id", label: "Client ID" },
      { name: "client_secret", label: "Client secret", secret: true },
      { name: "secret_token", label: "Secret token", secret: true },
    ],
    hint: "In Zoom's App Marketplace, build a Server-to-Server OAuth app with the cloud recording and past meeting scopes. The secret token is on its Features page.",
    webhook: (
      <>
        On the Zoom app&apos;s Features page, turn on event subscriptions, add this URL and the event <code>Recording → Transcript files have completed</code>.
      </>
    ),
  },
};

function meetingStatus(c: MeetingConnection) {
  const kept = c.meetings ? `${plural(c.meetings, "meeting")} with your leads so far.` : "No meetings with your leads yet.";
  if (c.provider === "gong") return c.synced_at ? `${kept} Reads new calls every 15 minutes.` : "Reading the last week of calls…";
  return kept;
}

/** Each meeting recorder connected to the project, and a form to connect another. Keys and secrets are write-only. */
function MeetingsSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/meeting-connections`;
  const { data, error, mutate } = useSWR<MeetingConnection[]>(key, fetcher, {
    refreshInterval: (list) => (list?.some((c) => c.provider === "gong" && !c.synced_at) ? 5000 : 0),
  });
  const open = MEETING_PROVIDERS.filter((p) => !data?.some((c) => c.provider === p));
  const [picked, setPicked] = useState<MeetingProvider>("fireflies");
  // Once the picked provider connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => {
    await mutate(await api<MeetingConnection[]>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Mic className="size-4" /> Meeting notes
        </span>
      }
      description="After a meeting with a lead in Fireflies, Fathom, Gong or Zoom, the agent sums it up, drafts the follow-up for you to approve and suggests the deal's next stage."
    >
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((c) => (
                <ConnectedMeetings key={c.provider} connection={c} call={call} />
              ))}
            </ul>
          )}
          {data && provider && <MeetingForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={data.length ? "Connect another" : "Recorder"} />}
        </>
      )}
    </Section>
  );
}

function ConnectedMeetings({ connection: c, call }: { connection: MeetingConnection; call: PaymentCall }) {
  const origin = useOrigin();
  const name = MEETING_NAMES[c.provider];
  const setup = MEETING_SETUP[c.provider];
  const { pending, run } = useAction();
  const [secret, setSecret] = useState("");
  const [replacing, setReplacing] = useState(false);
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New meetings won't come into Splee. The ones already here stay.`)) run("disconnect", () => call(c.provider, "DELETE"), `${name} disconnected`);
  };
  const save = async () => {
    if (await run("secret", () => call(`${c.provider}/secret`, "PUT", { secret }), "Webhook secret saved")) {
      setSecret("");
      setReplacing(false);
    }
  };
  const asksForSecret = c.provider === "fireflies" || c.provider === "fathom";
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-muted-foreground">{meetingStatus(c)}</p>
        </div>
        <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
          Disconnect
        </Button>
      </div>
      {c.error && <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">{c.error}</p>}
      {setup.webhook && (
        // Not a Field: its <label> wrapper would turn a click on the label text into a copy.
        <div className="mt-3 flex flex-col gap-1.5">
          <span className="text-sm font-medium">Webhook URL</span>
          <CopyField value={`${origin}/api/meeting-webhooks/${c.provider}/${c.webhook_token}`} />
          <p className="text-xs text-pretty text-muted-foreground">{setup.webhook}</p>
        </div>
      )}
      {setup.webhook &&
        (!asksForSecret || (c.has_secret && !replacing) ? (
          <p className="mt-3 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
            <span>
              {asksForSecret ? "Secret saved. " : ""}
              {c.last_event_at ? (
                <>
                  Last event <span title={dateTime(c.last_event_at)}>{timeAgo(c.last_event_at)}</span>.
                </>
              ) : (
                "No events yet."
              )}
            </span>
            {asksForSecret && (
              <button type="button" onClick={() => setReplacing(true)} className={linkClass}>
                Replace
              </button>
            )}
          </p>
        ) : (
          <form onSubmit={submit(save)} className="mt-3">
            <Field label="Webhook secret" hint={setup.secretHint}>
              <div className="flex gap-2">
                <Input aria-label="Webhook secret" type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
                <Button type="submit" loading={pending === "secret"} disabled={!secret}>
                  Save
                </Button>
              </div>
            </Field>
          </form>
        ))}
    </li>
  );
}

/** Keyed by the provider, so picking another one starts with empty boxes. */
function MeetingForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: MeetingProvider;
  open: MeetingProvider[];
  pick: (p: MeetingProvider) => void;
  call: PaymentCall;
  label: string;
}) {
  const setup = MEETING_SETUP[provider];
  const [values, setValues] = useState<Record<string, string>>({});
  const { pending, run } = useAction();
  const filled = setup.fields.every((f) => values[f.name]);
  const connect = () => run("connect", () => call(provider, "PUT", values), `Connected to ${MEETING_NAMES[provider]}`);
  const input = (f: MeetingField) => (
    <Input
      aria-label={f.label}
      type={f.secret ? "password" : "text"}
      autoComplete="off"
      value={values[f.name] ?? ""}
      onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value.trim() }))}
    />
  );
  const button = (
    <Button type="submit" loading={pending === "connect"} disabled={!filled}>
      Connect
    </Button>
  );
  const select = (
    <Field label={label} className="sm:w-52">
      <Select value={provider} onChange={(e) => pick(e.target.value as MeetingProvider)}>
        {open.map((p) => (
          <option key={p} value={p}>
            {MEETING_NAMES[p]}
          </option>
        ))}
      </Select>
    </Field>
  );
  if (setup.fields.length === 1) {
    const [f] = setup.fields;
    return (
      <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
        {select}
        <Field label={f.label} hint={setup.hint} className="flex-1">
          <div className="flex gap-2">
            {input(f)}
            {button}
          </div>
        </Field>
      </form>
    );
  }
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4">
      {select}
      <div className="grid gap-4 sm:grid-cols-2">
        {setup.fields.map((f) => (
          <Field key={f.name} label={f.label}>
            {input(f)}
          </Field>
        ))}
      </div>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <p className="text-xs text-pretty text-muted-foreground sm:max-w-md">{setup.hint}</p>
        {button}
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

// Keep in sync with rest_hooks::EVENTS in backend/src/rest_hooks.rs.
const HOOK_EVENTS: Record<HookEvent, string> = {
  hot_lead: "New hot reply",
  lead_replied: "Lead replied",
  meeting_booked: "Meeting booked",
  status_changed: "Lead status changed",
  "deal.won": "Deal won",
  lead_added: "New lead added",
};

/** Which tool a subscription posts to, told apart by the host of its URL. n8n runs on your own host, so it may show as that. */
function hookApp(url: string): string {
  const host = hostOf(url);
  if (host === "hooks.zapier.com" || host.endsWith(".zapier.com")) return "Zapier";
  if (/(^|\.)(make|integromat)\.com$/.test(host)) return "Make";
  if (host.endsWith(".n8n.cloud") || new URL(url, "https://x").pathname.startsWith("/webhook/")) return "n8n";
  return host;
}

/** How to connect the three tools, and the triggers they turned on, which each tool removes itself when it's turned off. */
function AutomationSection({ projectId }: { projectId: string }) {
  const origin = useOrigin();
  const key = `/projects/${projectId}/hook-subscriptions`;
  const { data, error, mutate } = useSWR<HookSubscription[]>(key, fetcher);
  const { pending, run } = useAction();
  const remove = (h: HookSubscription) => {
    if (!confirm(`Stop sending ${HOOK_EVENTS[h.event] ?? h.event} to ${hookApp(h.target_url)}? The zap, scenario or workflow stops getting it.`)) return;
    run(h.id, async () => {
      await api(`/hook-subscriptions/${h.id}`, { method: "DELETE" });
      await mutate();
    }, `Stopped sending to ${hookApp(h.target_url)}`);
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Zap className="size-4" /> Zapier, Make and n8n
        </span>
      }
      description="Connect Splee to thousands of other apps without code. Start a zap, scenario or workflow when a lead replies, books or is added, and add leads or set their status from other apps."
    >
      <ol className="flex list-decimal flex-col gap-1.5 pl-5 text-sm text-pretty">
        <li>
          Create a key on the{" "}
          <Link href="/app/api-keys" className="font-medium underline underline-offset-2">
            API keys page
          </Link>
          .
        </li>
        <li>Add the Splee app in Zapier, Make or n8n, and paste the key when it asks.</li>
        <li>If it asks for a base URL, use this one.</li>
      </ol>
      <div className="mt-3">
        <CopyField value={`${origin}/api/v1`} />
      </div>
      <h3 className="mt-5 text-sm font-medium">Listening now</h3>
      {error ? (
        <LoadError error={error} what="your subscriptions" retry={() => mutate()} className="mt-2" />
      ) : !data?.length ? (
        <p className="mt-1 text-xs text-pretty text-muted-foreground">
          Nothing yet. A trigger shows up here once you turn on its zap, scenario or workflow.
        </p>
      ) : (
        <ul className="mt-2 divide-y divide-border rounded-lg border border-border">
          {data.map((h) => (
            <li key={h.id} className="px-3 py-2.5">
              <div className="flex items-center gap-2">
                <div className="min-w-0 flex-1">
                  <p className="flex flex-wrap items-center gap-x-2 text-sm font-medium">
                    <Host host={hookApp(h.target_url)} />
                    <Badge tone="blue">{HOOK_EVENTS[h.event] ?? h.event}</Badge>
                  </p>
                  <p className="mt-0.5 text-xs text-pretty text-muted-foreground">
                    {h.project_id ? "This project" : "Every project"} · {h.key_name} key · added{" "}
                    <span title={dateTime(h.created_at)}>{timeAgo(h.created_at)}</span>
                  </p>
                </div>
                <Button size="sm" variant="ghost" onClick={() => remove(h)} loading={pending === h.id} aria-label={`Remove the ${hookApp(h.target_url)} trigger`}>
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
              <LastFailure error={h.error} at={h.failed_at} what="post" />
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

/** The chats that get the project's alerts. */
function ChatSection({
  projectId,
  weekly,
  setWeekly,
  briefs,
  setBriefs,
}: {
  projectId: string;
  weekly: boolean;
  setWeekly: (on: boolean) => void;
  briefs: boolean;
  setBriefs: (on: boolean) => void;
}) {
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
      <div className="mt-2 flex items-start justify-between gap-4 rounded-lg border border-border px-3 py-2.5">
        <span>
          <span className="block text-sm font-medium">Meeting briefs</span>
          <span className="block text-xs text-pretty text-muted-foreground">
            A day before each booked call, the agent writes who they are, what was said and what to ask. It posts here and emails the lead&apos;s
            owner, or the team when nobody owns the lead.
          </span>
        </span>
        <Switch checked={briefs} onChange={setBriefs} label="Meeting briefs" />
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

/** What each finder asks for when connecting, beyond an API key, and what's worth knowing before picking it. */
const FINDER_KEYS: Partial<Record<FinderName, { key?: string; secret?: string; hint: string }>> = {
  peopledatalabs: { hint: "From the API keys page of your dashboard. Each match uses a credit." },
  rocketreach: { hint: "From Account → API. Each lookup that finds the person uses a credit." },
  snov: { key: "API user ID", secret: "API secret", hint: "Both are under Account settings → API." },
  dropcontact: { hint: "From Settings → API. A search can take a minute or two." },
  contactout: { key: "API token", hint: "From your ContactOut API dashboard." },
  kaspr: { hint: "From Settings → API. Only leads with a LinkedIn profile are looked up." },
  lusha: { hint: "Every search uses a credit, even one that finds no one." },
};

/** The finders whose answers can include a mobile number, for "Find phone numbers too". Keep in sync with Finder::finds_phones. */
const PHONE_FINDERS: FinderName[] = ["peopledatalabs", "rocketreach", "lusha", "prospeo"];

function VerifierSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  // Hunter connected here also finds addresses, after the email finders, as it did before Email finders existed.
  const finds = settings.verifier === "hunter" ? " Hunter also finds emails, after any email finders." : "";
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

/** The email finders, asked in the order of this list until one finds the address. Keys are write-only. */
function FinderSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/finders`;
  const { data, error, mutate } = useSWR<Finders>(key, fetcher);
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>) => {
    await mutate(await api<Finders>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  const { pending, run } = useAction();
  const list = data?.finders ?? [];
  const open = (Object.keys(FINDER_NAMES) as FinderName[]).filter((p) => !list.some((f) => f.provider === p));
  const [picked, setPicked] = useState<FinderName>("hunter");
  // Once the picked finder connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const move = (i: number, by: number) => {
    const order = list.map((f) => f.provider);
    [order[i], order[i + by]] = [order[i + by], order[i]];
    run(`move-${order[i]}`, () => call("order", "PUT", { providers: order }));
  };
  const disconnect = (name: string, provider: FinderName) => {
    if (confirm(`Disconnect ${name}? The other finders and the verifier still look for emails.`)) run(`remove-${provider}`, () => call(provider, "DELETE"), `${name} disconnected`);
  };
  const phones = list.filter((f) => PHONE_FINDERS.includes(f.provider)).map((f) => FINDER_NAMES[f.provider]);
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UserSearch className="size-4" /> Email finders
        </span>
      }
      description="Find the email of a lead who came without one, and the real address behind a guessed one. Each is asked in turn, top to bottom, until one finds it."
    >
      {error ? (
        <LoadError error={error} what="your email finders" retry={() => mutate()} />
      ) : (
        data && (
          <>
            {!!list.length && (
              <ol className="mb-4 divide-y divide-border rounded-lg border border-border">
                {list.map((f, i) => {
                  const name = FINDER_NAMES[f.provider];
                  return (
                    <li key={f.provider} className="px-3 py-2">
                      <div className="flex items-center gap-2">
                        <span className="w-5 text-sm text-muted-foreground tabular-nums">{i + 1}.</span>
                        <p className="min-w-0 flex-1 text-sm font-medium">{name}</p>
                        <div className="flex">
                          <Button size="icon" variant="ghost" className="size-8" aria-label={`Move ${name} up`} disabled={i === 0} loading={pending === `move-${f.provider}`} onClick={() => move(i, -1)}>
                            <ArrowUp className="size-4" />
                          </Button>
                          <Button size="icon" variant="ghost" className="size-8" aria-label={`Move ${name} down`} disabled={i === list.length - 1} onClick={() => move(i, 1)}>
                            <ArrowDown className="size-4" />
                          </Button>
                          <Button
                            size="icon"
                            variant="ghost"
                            className="size-8"
                            aria-label={`Disconnect ${name}`}
                            loading={pending === `remove-${f.provider}`}
                            onClick={() => disconnect(name, f.provider)}
                          >
                            <Trash2 className="size-4" />
                          </Button>
                        </div>
                      </div>
                      {/* Under the whole row, so the buttons don't squeeze it into a narrow column on phones. */}
                      {f.paused_until && (
                        <p className="pb-1 pl-7 text-xs text-pretty text-amber-700">
                          Skipped until <span title={dateTime(f.paused_until)}>{timeOf(f.paused_until)}</span>: it&apos;s out of credits or past its rate limit.
                        </p>
                      )}
                      <LastFailure error={f.error} at={f.failed_at} what="lookup" />
                    </li>
                  );
                })}
              </ol>
            )}
            {provider && <FinderForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={list.length ? "Add another" : "Service"} />}
            <div className="mt-4 flex items-start justify-between gap-4 rounded-lg border border-border px-3 py-2.5">
              <span>
                <span className="block text-sm font-medium">Find phone numbers too</span>
                <span className="block text-xs text-pretty text-muted-foreground">
                  {phones.length
                    ? `${andList(phones)} ${phones.length === 1 ? "also fills" : "also fill"} in a missing mobile number, which costs more credits.`
                    : "People Data Labs, RocketReach, Lusha and Prospeo can also fill in a missing mobile number, which costs more credits."}
                </span>
              </span>
              <Switch
                checked={data.find_phones}
                onChange={(on) => run("phones", () => call("phones", "PUT", { on }), on ? "Finding phone numbers too" : "Not finding phone numbers")}
                disabled={pending === "phones"}
                label="Find phone numbers too"
              />
            </div>
          </>
        )
      )}
    </Section>
  );
}

/** "A", "A and B", or "A, B and C". */
const andList = (names: string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`);

/** Today's time, or the date and time when it's later than today. */
const timeOf = (ts: string) => {
  const at = new Date(ts);
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return at.toDateString() === new Date().toDateString() ? time : `${at.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
};

/** Keyed by the finder, so picking another one starts with empty fields. */
function FinderForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: FinderName;
  open: FinderName[];
  pick: (p: FinderName) => void;
  call: (path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>) => Promise<void>;
  label: string;
}) {
  const info = FINDER_KEYS[provider];
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");
  const { pending, run } = useAction();
  const name = FINDER_NAMES[provider];
  const keyLabel = info?.key ?? "API key";
  const connect = () => run("connect", () => call(provider, "PUT", { key, secret }), `Connected to ${name}`);
  const button = (
    <Button type="submit" loading={pending === "connect"} disabled={!key || (!!info?.secret && !secret)}>
      Connect
    </Button>
  );
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
      <Field label={label} className="sm:w-52">
        <Select value={provider} onChange={(e) => pick(e.target.value as FinderName)}>
          {open.map((p) => (
            <option key={p} value={p}>
              {FINDER_NAMES[p]}
            </option>
          ))}
        </Select>
      </Field>
      {info?.secret ? (
        <>
          <Field label={keyLabel} className="flex-1" hint={info.hint}>
            <Input type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
          </Field>
          <Field label={info.secret} className="flex-1">
            <div className="flex gap-2">
              <Input aria-label={info.secret} type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
              {button}
            </div>
          </Field>
        </>
      ) : (
        <Field label={keyLabel} className="flex-1" hint={info?.hint ?? "Each address found uses one of the service's credits."}>
          <div className="flex gap-2">
            {/* The label also wraps Connect, so name the input itself. */}
            <Input aria-label={keyLabel} type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
            {button}
          </div>
        </Field>
      )}
    </form>
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
      <LastFailure error={error} at={failedAt} what="check" />
    </Section>
  );
}

/** The organization's Google Maps Platform key for the Companies page's Local businesses tab, shared by every project.
 *  It's write-only, and saving it checks it with one free call to Google. */
function GoogleMapsSection() {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const { data, error, mutate } = useSWR<LocalUsage>("/local-businesses/key", fetcher);
  const [key, setKey] = useState("");
  const [cap, setCap] = useState<number | null>(null);
  const { pending, run } = useAction();
  const call = (method: "PUT" | "DELETE", path: string, body?: Record<string, unknown>) => async () => {
    await mutate(await api<LocalUsage>(`/local-businesses/${path}`, { method, body }), { revalidate: false });
  };
  const connect = async () => {
    if (await run("connect", call("PUT", "key", { key }), "Connected to Google Maps")) setKey("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Google Maps? Local business search stops for every project until a key is connected again.")) {
      run("disconnect", call("DELETE", "key"), "Google Maps disconnected");
    }
  };
  const saveCap = async () => {
    if (cap !== null && (await run("cap", call("PUT", "cap", { cap }), "Monthly cap saved"))) setCap(null);
  };
  return (
    <div id="google-maps" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <MapPin className="size-4" /> Google Maps
          </span>
        }
        description="Finds local businesses by what they are and where on the Companies page. The key is shared by your organization's projects, and Google bills its calls to your account."
        actions={
          data?.connected && admin ? (
            <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
              Disconnect
            </Button>
          ) : undefined
        }
      >
        {error ? (
          <LoadError error={error} what="your Google Maps key" retry={() => mutate()} />
        ) : !data ? null : data.connected ? (
          <div className="space-y-4">
            <p className="text-sm text-pretty">
              Connected. {num(data.calls)} of {num(data.cap)} calls used this month, about ${(data.cost_cents / 100).toFixed(2)} at $
              {(data.price_per_1000_cents / 100).toFixed(2)} per 1,000. Searches stop at the cap until the 1st of next month.
            </p>
            {admin && (
              <form onSubmit={submit(saveCap)} className="flex items-end gap-2">
                <Field label="Monthly cap" hint="Each place searched costs up to 3 calls." className="w-full sm:w-72">
                  <div className="flex gap-2">
                    <NumberInput aria-label="Monthly cap" min={0} max={1000000} value={cap ?? data.cap} onValueChange={setCap} />
                    <Button type="submit" loading={pending === "cap"} disabled={cap === null || cap === data.cap}>
                      Save
                    </Button>
                  </div>
                </Field>
              </form>
            )}
          </div>
        ) : admin ? (
          <form onSubmit={submit(connect)}>
            <Field
              label="API key"
              hint="In Google Cloud, turn on Places API (New), then create a key under APIs & Services, Credentials. Splee checks it with one free call."
            >
              <div className="flex gap-2">
                <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
                <Button type="submit" loading={pending === "connect"} disabled={!key}>
                  Connect
                </Button>
              </div>
            </Field>
          </form>
        ) : (
          <p className="text-sm text-muted-foreground">Ask an admin of your organization to connect a Google Maps key.</p>
        )}
      </Section>
    </div>
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

const TEMPLATE_FIELDS = {
  first_name: "First name",
  last_name: "Last name",
  full_name: "Full name",
  company_name: "Company",
  title: "Job title",
  city: "City",
  sender_name: "Your sender name",
} as const;

/** The organization's Twilio account, shared by every project. The API key's secret and the auth token are write-only, and
 *  connecting checks both with Twilio first. */
function TwilioSection({ settings }: { settings: ProjectSettings }) {
  const { data, mutate } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const empty = { account_sid: "", api_key_sid: "", api_secret: "", auth_token: "", sms_from: "", whatsapp_from: "" };
  const [form, setForm] = useState(empty);
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const [busy, setBusy] = useState(false);
  const call = async (path: string, method: "PUT" | "DELETE", body: Record<string, unknown> | undefined, done: string) => {
    setBusy(true);
    try {
      await mutate(await api<TwilioStatus>(path, { method, body }), { revalidate: false });
      toast.success(done);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  const connect = async () => {
    if (await call("/organization/twilio", "PUT", form, "Connected to Twilio")) setForm(empty);
  };
  const disconnect = () => {
    if (confirm("Disconnect Twilio for every project? Approved texts will wait until you connect it again.")) call("/organization/twilio", "DELETE", undefined, "Twilio disconnected");
  };
  const c = data?.connection;
  const steps = settings.text_steps.length;
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Smartphone className="size-4" /> Texts and calls
        </span>
      }
      description="Sends the SMS and WhatsApp steps of your sequences and your replies to texts, and places your team's calls, from your own Twilio account. Every project in your organization shares it."
      actions={
        c ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {!data ? null : c ? (
        <div className="flex flex-col gap-4">
          <p className="text-sm text-pretty">
            Texting through {c.account_name || "your Twilio account"}
            {c.sms_from && <> with SMS from {c.sms_from}</>}
            {c.whatsapp_from && <>{c.sms_from ? " and" : " with"} WhatsApp from {c.whatsapp_from}</>}. Due texts go out in your sending hours, in the
            lead&apos;s own time zone when their number shows it, up to {settings.daily_text_limit} a day.{" "}
            {steps ? `This project has ${steps === 1 ? "1 text step" : `${steps} text steps`} on the Follow-ups page.` : "Add text steps on the Follow-ups page."}
          </p>
          {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
          <div className="flex flex-col gap-1.5">
            <span className="text-sm font-medium">Incoming messages webhook</span>
            <CopyField value={data.inbound_url} />
            <p className="text-xs text-pretty text-muted-foreground">
              In Twilio, paste this as the &quot;A message comes in&quot; webhook, with HTTP POST, on your number, Messaging Service or WhatsApp
              sender. Replies then land in the inbox, and STOP takes the number off your lists.
            </p>
          </div>
          {c.whatsapp_from && <WhatsappTemplate connection={c} onSaved={(fresh) => mutate(fresh, { revalidate: false })} />}
          <CallingSettings status={data} onSaved={(fresh) => mutate(fresh, { revalidate: false })} />
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In the Twilio console, copy the Account SID and auth token from Account info, and create an API key under API keys &amp; tokens. Splee
            sends with the key and checks that replies come from Twilio with the auth token.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Account SID">
              <Input placeholder="AC…" autoComplete="off" value={form.account_sid} onChange={set("account_sid")} />
            </Field>
            <Field label="Auth token">
              <Input type="password" autoComplete="off" value={form.auth_token} onChange={set("auth_token")} />
            </Field>
            <Field label="API key SID">
              <Input placeholder="SK…" autoComplete="off" value={form.api_key_sid} onChange={set("api_key_sid")} />
            </Field>
            <Field label="API key secret">
              <Input type="password" autoComplete="off" value={form.api_secret} onChange={set("api_secret")} />
            </Field>
            <Field label="SMS sender" hint="A number like +14155550100, or a Messaging Service SID.">
              <Input placeholder="+14155550100 or MG…" autoComplete="off" value={form.sms_from} onChange={set("sms_from")} />
            </Field>
            <Field label="WhatsApp sender" hint="Optional. A number approved for WhatsApp in Twilio.">
              <Input placeholder="+14155550101" autoComplete="off" value={form.whatsapp_from} onChange={set("whatsapp_from")} />
            </Field>
          </div>
          <div className="flex justify-end">
            <Button
              type="submit"
              loading={busy}
              disabled={!form.account_sid || !form.auth_token || !form.api_key_sid || !form.api_secret || (!form.sms_from && !form.whatsapp_from)}
            >
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={c?.error ?? null} at={c?.failed_at ?? null} what="send" />
    </Section>
  );
}

/** Calls from the browser go through a TwiML App, which saving points at Splee. Calls are recorded, and transcribed when it's on. */
function CallingSettings({ status, onSaved }: { status: TwilioStatus; onSaved: (fresh: TwilioStatus) => void }) {
  const c = status.connection!;
  const [form, setForm] = useState({ twiml_app_sid: c.twiml_app_sid, voice_from: c.voice_from, transcribe: c.call_transcription });
  const [busy, setBusy] = useState(false);
  const changed = form.twiml_app_sid !== c.twiml_app_sid || form.voice_from !== c.voice_from || form.transcribe !== c.call_transcription;
  const save = async (body: typeof form) => {
    setBusy(true);
    try {
      onSaved(await api<TwilioStatus>("/organization/twilio/calling", { method: "PUT", body }));
      toast.success(body.twiml_app_sid ? "Calling is on" : "Calling is off");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const fallback = c.sms_from.startsWith("+") ? c.sms_from : "";
  return (
    <form onSubmit={submit(() => save(form))} className="flex flex-col gap-3 rounded-lg border border-border p-3">
      <div>
        <p className="text-sm font-medium">Calls</p>
        <p className="mt-0.5 text-xs text-pretty text-muted-foreground">
          Lets your team call leads from the Calls page. Create a TwiML App in Twilio under Voice, TwiML Apps, and paste its SID here. Saving
          points its Voice URL at Splee. Calls are recorded, so tell leads at the start where the law asks you to.
        </p>
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="TwiML App SID">
          <Input placeholder="AP…" autoComplete="off" value={form.twiml_app_sid} onChange={(e) => setForm({ ...form, twiml_app_sid: e.target.value.trim() })} />
        </Field>
        <Field label="Calls come from" hint={fallback ? `Leave empty to use ${fallback}.` : "A Twilio number that can make calls."}>
          <Input placeholder={fallback || "+14155550100"} autoComplete="off" value={form.voice_from} onChange={(e) => setForm({ ...form, voice_from: e.target.value.trim() })} />
        </Field>
      </div>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <span className="flex items-center gap-2.5 text-sm">
          <Switch checked={form.transcribe} onChange={(transcribe) => setForm({ ...form, transcribe })} label="Transcribe calls" />
          Transcribe calls, and have the agent summarize them
        </span>
        <span className="flex gap-2">
          {c.twiml_app_sid && (
            <Button type="button" variant="ghost" loading={busy} onClick={() => save({ twiml_app_sid: "", voice_from: form.voice_from, transcribe: false })}>
              Turn off
            </Button>
          )}
          <Button type="submit" variant="outline" loading={busy} disabled={!changed || !form.twiml_app_sid}>
            Save
          </Button>
        </span>
      </div>
    </form>
  );
}

/** WhatsApp only takes an approved template until the lead writes back, so WhatsApp steps send this one. */
function WhatsappTemplate({ connection: c, onSaved }: { connection: NonNullable<TwilioStatus["connection"]>; onSaved: (fresh: TwilioStatus) => void }) {
  const [sid, setSid] = useState(c.whatsapp_template_sid);
  const [busy, setBusy] = useState(false);
  const save = async (template_sid: string, variables: Record<string, string>) => {
    setBusy(true);
    try {
      onSaved(await api<TwilioStatus>("/organization/twilio/template", { method: "PUT", body: { template_sid, variables } }));
      toast.success(template_sid ? "Template saved" : "Template removed");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const vars = Object.entries(c.whatsapp_template_vars).sort(([a], [b]) => Number(a) - Number(b));
  return (
    <div className="flex flex-col gap-3 rounded-lg border border-border p-3">
      <form onSubmit={submit(() => save(sid, sid === c.whatsapp_template_sid ? c.whatsapp_template_vars : {}))}>
        <Field label="WhatsApp template" hint="The Content SID of a template WhatsApp approved, from Messaging → Content Template Builder.">
          <div className="flex flex-col gap-2 sm:flex-row">
            <Input placeholder="HX…" autoComplete="off" value={sid} onChange={(e) => setSid(e.target.value.trim())} />
            <Button type="submit" variant="outline" loading={busy} disabled={sid === c.whatsapp_template_sid}>
              Save
            </Button>
          </div>
        </Field>
      </form>
      {c.whatsapp_template_sid ? (
        <>
          <p className="rounded-lg bg-muted px-3 py-2 text-sm whitespace-pre-wrap">{c.whatsapp_template_body}</p>
          {vars.length > 0 && (
            <div className="grid gap-3 sm:grid-cols-2">
              {vars.map(([name, field]) => (
                <Field key={name} label={`{{${name}}} is the lead's`}>
                  <Select value={field} onChange={(e) => save(c.whatsapp_template_sid, { ...c.whatsapp_template_vars, [name]: e.target.value })} disabled={busy}>
                    {Object.entries(TEMPLATE_FIELDS).map(([value, label]) => (
                      <option key={value} value={value}>
                        {label}
                      </option>
                    ))}
                  </Select>
                </Field>
              ))}
            </div>
          )}
        </>
      ) : (
        <p className="text-xs text-pretty text-muted-foreground">WhatsApp steps wait until you pick one. Replies inside 24 hours of their last message can be any text.</p>
      )}
    </div>
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

const BLOG_KINDS = { wordpress: "WordPress", ghost: "Ghost", webflow: "Webflow" } as const;
type BlogKind = keyof typeof BLOG_KINDS;
const WEEKLY_CAPS = [1, 2, 3, 4, 5, 6, 7];

/** What a blog does with an approved article. */
function PublishSelect({ kind, publish, onChange, disabled }: { kind: BlogKind; publish: boolean; onChange: (publish: boolean) => void; disabled?: boolean }) {
  return (
    <Select value={publish ? "publish" : "draft"} disabled={disabled} onChange={(e) => onChange(e.target.value === "publish")} aria-label="When approved">
      <option value="draft">Save as a draft in {BLOG_KINDS[kind]}</option>
      <option value="publish">Publish it</option>
    </Select>
  );
}

function CapSelect({ cap, onChange, disabled }: { cap: number; onChange: (cap: number) => void; disabled?: boolean }) {
  return (
    <Select value={cap} disabled={disabled} onChange={(e) => onChange(Number(e.target.value))} aria-label="Articles a week">
      {WEEKLY_CAPS.map((n) => (
        <option key={n} value={n}>
          {n === 1 ? "1 article a week" : `${n} articles a week`}
        </option>
      ))}
    </Select>
  );
}

/** A connected blog's settings, saved as they change. A paused blog says why and offers to connect it again. */
function BlogRow({ blog: b, onChange, onReconnect }: { blog: Blog; onChange: () => Promise<unknown>; onReconnect: (b: Blog) => void }) {
  const { pending, run } = useAction();
  const save = (body: { publish?: boolean; weekly_cap?: number }) =>
    run("save", async () => {
      await api(`/blogs/${b.id}`, { method: "PATCH", body });
      await onChange();
    }, "Saved");
  const remove = () => {
    if (!confirm(`Remove ${b.name}? Approved articles for it go back to drafts.`)) return;
    run("remove", async () => {
      await api(`/blogs/${b.id}`, { method: "DELETE" });
      await onChange();
    }, `Removed ${b.name}`);
  };
  const where =
    b.kind === "wordpress" ? `Writing as ${b.username}.` : b.kind === "webflow" ? `Articles go in the ${b.collection_name} collection.` : "Writing with the Admin API key.";
  return (
    <li className="flex flex-col gap-3 px-3 py-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm font-medium">
            {b.name} <Badge>{BLOG_KINDS[b.kind]}</Badge>
          </p>
          <p className="text-xs text-pretty text-muted-foreground">
            <Host host={b.url.replace(/^https?:\/\//, "")} /> · {where}
          </p>
        </div>
        <Button size="sm" variant="ghost" onClick={remove} loading={pending === "remove"} aria-label={`Remove ${b.name}`}>
          <Trash2 className="size-3.5" />
        </Button>
      </div>
      {b.paused ? (
        <div className="flex flex-col gap-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 sm:flex-row sm:items-center">
          <p className="flex-1 text-pretty">
            <span className="font-medium">Paused. </span>
            {b.error ?? `${BLOG_KINDS[b.kind]} turned Splee down.`}
          </p>
          <Button size="sm" variant="outline" onClick={() => onReconnect(b)} className="self-start">
            Connect again
          </Button>
        </div>
      ) : (
        <div className="grid gap-2 sm:grid-cols-2">
          <PublishSelect kind={b.kind} publish={b.publish} onChange={(publish) => save({ publish })} disabled={pending === "save"} />
          <CapSelect cap={b.weekly_cap} onChange={(weekly_cap) => save({ weekly_cap })} disabled={pending === "save"} />
        </div>
      )}
      {!b.paused && <LastFailure error={b.error} at={b.failed_at} what="post" />}
    </li>
  );
}

const BLOG_STEPS: Record<BlogKind, string> = {
  wordpress: "In WordPress, open Users, then Profile, and add an application password near the bottom. Self-hosted sites and WordPress.com Business plans have it.",
  ghost: "In Ghost's admin, open Settings, then Integrations, add a custom integration and copy its Admin API key.",
  webflow: "In your Webflow site's settings, open Apps & integrations and generate an API token that can read sites and read and write the CMS.",
};

type Fields = { title_field: string; body_field: string; tags_field: string; excerpt_field: string };
type Collection = BlogTest["sites"][number]["collections"][number];

/** The fields a collection's articles most likely go in: its name field for the title, its first rich text field for the
 *  article, and plain text fields named like tags and a summary. */
function guessFields(c: Collection | undefined): Fields {
  const plain = (c?.fields ?? []).filter((f) => f.type === "PlainText");
  const named = (...words: string[]) => plain.find((f) => words.some((w) => f.slug.includes(w)))?.slug ?? "";
  return {
    title_field: plain.some((f) => f.slug === "name") ? "name" : (plain[0]?.slug ?? ""),
    body_field: c?.fields.find((f) => f.type === "RichText")?.slug ?? "",
    tags_field: named("tag"),
    excerpt_field: named("summary", "excerpt", "description"),
  };
}

/** Picks a field of the collection of one type. Optional ones can be left out. */
function FieldSelect({
  label,
  collection,
  type,
  value,
  optional,
  onChange,
}: {
  label: string;
  collection: Collection;
  type: "PlainText" | "RichText";
  value: string;
  optional?: boolean;
  onChange: (slug: string) => void;
}) {
  const fields = collection.fields.filter((f) => f.type === type && f.slug !== "slug");
  return (
    <Field label={label}>
      <Select value={value} onChange={(e) => onChange(e.target.value)}>
        {(optional || !value) && <option value="">{optional ? "Leave it out" : "Pick a field"}</option>}
        {fields.map((f) => (
          <option key={f.slug} value={f.slug}>
            {f.name}
          </option>
        ))}
      </Select>
    </Field>
  );
}

/** Each blog is connected and removed on its own, not with the page's Save. Secrets are write-only. Testing reads the blog
 *  with what was typed, and connecting reads it again before saving. Connecting the same blog again replaces its secret and
 *  unpauses it. */
function BlogSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/blogs`;
  const { data, error, mutate } = useSWR<Blog[]>(key, fetcher);
  const { pending, run } = useAction();
  const [kind, setKind] = useState<BlogKind>("wordpress");
  const empty = { url: "", username: "", secret: "" };
  const [form, setForm] = useState(empty);
  const [tested, setTested] = useState<BlogTest | null>(null);
  const [siteId, setSiteId] = useState("");
  const [collectionId, setCollectionId] = useState("");
  const [fields, setFields] = useState<Fields>(guessFields(undefined));
  const [publish, setPublish] = useState(false);
  const [cap, setCap] = useState(2);
  const site = tested?.sites.find((s) => s.id === siteId);
  const collection = site?.collections.find((c) => c.id === collectionId);
  // Typing anything new means the blog has to be read again.
  const set = (field: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setForm({ ...form, [field]: e.target.value });
    setTested(null);
  };
  const pickKind = (k: BlogKind) => {
    setKind(k);
    setTested(null);
  };
  const pickCollection = (s: BlogTest["sites"][number] | undefined, id: string) => {
    setCollectionId(id);
    setFields(guessFields(s?.collections.find((c) => c.id === id)));
  };
  const pickSite = (found: BlogTest, id: string) => {
    const s = found.sites.find((x) => x.id === id);
    setSiteId(id);
    pickCollection(s, s?.collections[0]?.id ?? "");
  };
  const body = () => ({ kind, url: form.url.trim(), username: form.username.trim(), secret: form.secret.trim() });
  const test = () =>
    run("test", async () => {
      const found = await api<BlogTest>(`${key}/test`, { body: body() });
      setTested(found);
      if (kind === "webflow") pickSite(found, found.sites[0]?.id ?? "");
    });
  const connect = async () => {
    const done = await run("connect", async () => {
      const where = kind === "webflow" ? { site_id: siteId, collection_id: collectionId, ...fields } : {};
      await api(key, { body: { ...body(), ...where, publish, weekly_cap: cap } });
      await mutate();
    }, `Connected ${kind === "webflow" ? (site?.name ?? "Webflow") : tested?.name}`);
    if (done) {
      setForm(empty);
      setTested(null);
    }
  };
  const reconnect = (b: Blog) => {
    setKind(b.kind);
    setForm({ url: b.kind === "webflow" ? "" : b.url, username: b.username, secret: "" });
    setPublish(b.publish);
    setCap(b.weekly_cap);
    setTested(null);
  };
  const typed = !!form.secret.trim() && (kind === "webflow" || !!form.url.trim()) && (kind !== "wordpress" || !!form.username.trim());
  const ready = !!tested && (kind !== "webflow" || (!!collection && !!fields.body_field && !!fields.title_field));
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <NotebookPen className="size-4" /> Your blogs
        </span>
      }
      description="Drafts articles for your own WordPress, Ghost or Webflow blog, and publishes the ones you approve on the Social page."
    >
      {error ? (
        <LoadError error={error} what="your blogs" retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((b) => (
                <BlogRow key={b.id} blog={b} onChange={() => mutate()} onReconnect={reconnect} />
              ))}
            </ul>
          )}
          <form onSubmit={submit(tested ? connect : test)} className="flex flex-col gap-4">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <p className="text-sm font-medium">{data?.length ? "Connect another blog" : "Connect a blog"}</p>
              <Tabs
                value={kind}
                onChange={pickKind}
                options={(Object.keys(BLOG_KINDS) as BlogKind[]).map((k) => ({ value: k, label: BLOG_KINDS[k] }))}
                className="self-start"
              />
            </div>
            <p className="text-sm text-pretty text-muted-foreground">{BLOG_STEPS[kind]}</p>
            <div className="grid gap-4 sm:grid-cols-2">
              {kind !== "webflow" && (
                // WordPress's three fields fill the grid with the site on a row of its own.
                <Field label={kind === "ghost" ? "Admin address" : "Site"} className={kind === "wordpress" ? "sm:col-span-2" : undefined}>
                  <Input autoComplete="off" placeholder={kind === "ghost" ? "example.ghost.io" : "example.com/blog"} value={form.url} onChange={set("url")} />
                </Field>
              )}
              {kind === "wordpress" && (
                <Field label="Username">
                  <Input autoComplete="off" value={form.username} onChange={set("username")} />
                </Field>
              )}
              <Field label={kind === "wordpress" ? "Application password" : kind === "ghost" ? "Admin API key" : "API token"} className={kind === "webflow" ? "sm:col-span-2" : undefined}>
                <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
              </Field>
            </div>
            {tested && kind !== "webflow" && (
              <p className="rounded-lg bg-muted/60 px-3 py-2 text-sm text-pretty">
                Found <span className="font-medium">{tested.name}</span>
                {tested.user ? `, signed in as ${tested.user}.` : "."}
              </p>
            )}
            {tested && kind === "webflow" && (
              <div className="grid gap-4 rounded-lg border border-border p-3 sm:grid-cols-2">
                <Field label="Site">
                  <Select value={siteId} onChange={(e) => pickSite(tested, e.target.value)}>
                    {tested.sites.map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Collection">
                  <Select value={collectionId} onChange={(e) => pickCollection(site, e.target.value)}>
                    {site?.collections.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                {collection && (
                  <>
                    <FieldSelect label="Title goes in" collection={collection} type="PlainText" value={fields.title_field} onChange={(title_field) => setFields({ ...fields, title_field })} />
                    <FieldSelect label="Article goes in" collection={collection} type="RichText" value={fields.body_field} onChange={(body_field) => setFields({ ...fields, body_field })} />
                    <FieldSelect label="Tags go in" collection={collection} type="PlainText" value={fields.tags_field} optional onChange={(tags_field) => setFields({ ...fields, tags_field })} />
                    <FieldSelect label="Excerpt goes in" collection={collection} type="PlainText" value={fields.excerpt_field} optional onChange={(excerpt_field) => setFields({ ...fields, excerpt_field })} />
                  </>
                )}
              </div>
            )}
            {tested && (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="When you approve an article">
                  <PublishSelect kind={kind} publish={publish} onChange={setPublish} />
                </Field>
                <Field label="At most">
                  <CapSelect cap={cap} onChange={setCap} />
                </Field>
              </div>
            )}
            <div className="flex flex-wrap gap-2">
              {tested ? (
                <Button type="submit" loading={pending === "connect"} disabled={!ready}>
                  Connect blog
                </Button>
              ) : (
                <Button type="submit" loading={pending === "test"} disabled={!typed}>
                  Test connection
                </Button>
              )}
            </div>
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

/** Invites the bot with what it reads and posts with: View Channels, Send Messages and Read Message History. */
const discordInvite = (botId: string) => `https://discord.com/oauth2/authorize?client_id=${botId}&scope=bot&permissions=${(1 << 10) | (1 << 11) | (1 << 16)}`;

const linkClass = "font-medium text-foreground underline underline-offset-4";

/** What the bot can do in a channel, for its row. */
function channelNote(c: SocialChannel) {
  const where = c.platform === "discord" ? c.guild_name : c.kind === "channel" ? "Channel" : "Group";
  if (!c.can_read && !c.can_send) return `${where} · The bot can't read or post here`;
  if (!c.can_send) return `${where} · Read only, the bot can't post here`;
  if (!c.can_read) return c.kind === "channel" ? `${where} · Posts only` : `${where} · Posts only, the bot can't see messages here`;
  return `${where} · Replies and posts`;
}

/** Signs in as the bot. A failure is saved like a post's, so the settings are fetched again to show it. */
function TestBotButton({ projectId, platform }: { projectId: string; platform: "discord" | "telegram" }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const key = `/projects/${projectId}/settings`;
  const test = () =>
    run("test", async () => {
      try {
        const fresh = await api<ProjectSettings>(`/projects/${projectId}/integrations/${platform}/test`, { method: "POST" });
        await mutate(key, fresh, { revalidate: false });
      } catch (e) {
        await mutate(key);
        throw e;
      }
    }, "The bot signed in");
  return (
    <Button size="sm" variant="outline" onClick={test} loading={pending === "test"}>
      Test
    </Button>
  );
}

/** The channels a bot can see, each picked on its own. The platform gives the list, read again with Refresh. */
function BotChannels({ projectId, platform, empty }: { projectId: string; platform: "discord" | "telegram"; empty: React.ReactNode }) {
  const { data, error, mutate } = useSWR<SocialChannel[]>(`/projects/${projectId}/social-channels`, fetcher);
  const { pending, run } = useAction();
  const channels = (data ?? []).filter((c) => c.platform === platform);
  const refresh = () =>
    run("refresh", async () => {
      await mutate(await api<SocialChannel[]>(`/projects/${projectId}/integrations/${platform}/channels`, { method: "POST" }), { revalidate: false });
    }, "List refreshed");
  const pick = (c: SocialChannel, picked: boolean) =>
    run(c.id, async () => {
      const fresh = await api<SocialChannel>(`/social-channels/${c.id}`, { method: "PATCH", body: { picked } });
      await mutate(data?.map((x) => (x.id === c.id ? fresh : x)), { revalidate: false });
    });
  if (error) return <LoadError error={error} what="the bot's channels" retry={() => mutate()} />;
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm font-medium">{platform === "discord" ? "Channels it reads and posts in" : "Chats it reads and posts in"}</p>
        <Button size="sm" variant="ghost" onClick={refresh} loading={pending === "refresh"} className="px-2">
          <RefreshCw className="size-3.5" /> Refresh
        </Button>
      </div>
      {!data ? null : channels.length === 0 ? (
        <p className="text-sm text-pretty text-muted-foreground">{empty}</p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {channels.map((c) => (
            <li key={c.id} className="flex items-center gap-3 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">{platform === "discord" ? `#${c.name}` : c.name}</p>
                <p className="text-xs text-pretty text-muted-foreground">{channelNote(c)}</p>
              </div>
              <Switch
                checked={c.picked}
                onChange={(on) => pick(c, on)}
                disabled={pending === c.id || (!c.picked && !c.can_read && !c.can_send)}
                label={`Use ${platform === "discord" ? `#${c.name}` : c.name}`}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** The bot token is write-only. Connecting checks it with Discord and lists the channels the bot can see. */
function DiscordSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { mutate } = useSWRConfig();
  const { busy, call } = useConnection(projectId, "discord");
  const connect = async () => {
    if (await call({ token }, "Connected to Discord")) {
      setToken("");
      mutate(`/projects/${projectId}/social-channels`);
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Discord? Approved messages will wait until you connect the bot again.")) call(undefined, "Discord disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Bot className="size-4" /> Discord
        </span>
      }
      description="Reads recent messages in the channels you pick, drafts replies and posts for the Social page, and posts the ones you approve as your bot."
      actions={
        settings.discord_connected ? (
          <>
            <TestBotButton projectId={projectId} platform="discord" />
            <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
              Disconnect
            </Button>
          </>
        ) : undefined
      }
    >
      {settings.discord_connected ? (
        <div className="flex flex-col gap-4 text-sm text-pretty">
          <p>
            Posting as {settings.discord_bot}, up to 10 a day, one every ten minutes.{" "}
            <a href={discordInvite(settings.discord_bot_id)} target="_blank" rel="noreferrer" className={linkClass}>
              Invite it to a server
            </a>
            .
          </p>
          <BotChannels projectId={projectId} platform="discord" empty="The bot isn't in a server yet. Invite it, then refresh the list." />
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <ol className="list-decimal space-y-1 pl-5 text-sm text-pretty text-muted-foreground">
            <li>
              In the{" "}
              <a href="https://discord.com/developers/applications" target="_blank" rel="noreferrer" className={linkClass}>
                Discord Developer Portal
              </a>
              , create an application for your company.
            </li>
            <li>On its Bot page, turn on Message Content Intent, then reset the token and copy it.</li>
            <li>Paste the token here. Splee then links you to invite the bot with Send Messages and Read Message History.</li>
          </ol>
          <Field label="Bot token">
            <div className="flex gap-2">
              <Input aria-label="Bot token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.discord_error} at={settings.discord_failed_at} what="post" />
    </Section>
  );
}

/** The bot token is write-only. Connecting checks it with Telegram and reads the groups its waiting updates show. */
function TelegramSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { mutate } = useSWRConfig();
  const { busy, call } = useConnection(projectId, "telegram");
  const connect = async () => {
    if (await call({ token }, "Connected to Telegram")) {
      setToken("");
      mutate(`/projects/${projectId}/social-channels`);
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Telegram? Approved messages will wait until you connect the bot again.")) call(undefined, "Telegram disconnected");
  };
  const botFather = (
    <a href="https://t.me/BotFather" target="_blank" rel="noreferrer" className={linkClass}>
      @BotFather
    </a>
  );
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Send className="size-4" /> Telegram
        </span>
      }
      description="Reads new messages in the groups you pick, drafts replies and posts for the Social page, and posts the ones you approve as your bot."
      actions={
        settings.telegram_connected ? (
          <>
            <TestBotButton projectId={projectId} platform="telegram" />
            <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
              Disconnect
            </Button>
          </>
        ) : undefined
      }
    >
      {settings.telegram_connected ? (
        <div className="flex flex-col gap-4 text-sm text-pretty">
          <p>Posting as @{settings.telegram_bot}, up to 10 a day, one every ten minutes.</p>
          {!settings.telegram_reads_all && (
            <p className="rounded-lg bg-amber-50 px-3 py-2 text-amber-800">
              Group privacy mode is on, so the bot only sees messages that mention it or answer it. In {botFather}, send /setprivacy and choose
              Disable. Then remove the bot from each group, add it back and press Test.
            </p>
          )}
          <BotChannels
            projectId={projectId}
            platform="telegram"
            empty="Telegram tells bots about a group once something happens there. Add the bot to a group, send a message in it, then refresh the list."
          />
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <ol className="list-decimal space-y-1 pl-5 text-sm text-pretty text-muted-foreground">
            <li>In Telegram, message {botFather}, send /newbot and copy the token it gives you.</li>
            <li>
              Send /setprivacy, pick the bot and choose Disable. Group privacy mode has to be off for the bot to see the messages in a group.
            </li>
            <li>Add the bot to your groups. In a channel, make it an admin that can post messages.</li>
          </ol>
          <Field label="Bot token">
            <div className="flex gap-2">
              <Input aria-label="Bot token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.telegram_error} at={settings.telegram_failed_at} what="post" />
    </Section>
  );
}

/** What the user's role lets Splee do on a Page or Instagram account, for its row. */
function metaNote(c: SocialChannel) {
  const where = c.platform === "facebook" ? "Facebook Page" : `Instagram, through ${c.guild_name}`;
  if (!c.can_read && !c.can_send) return `${where} · Your role here can't post or answer comments`;
  if (!c.can_send) return `${where} · Answers comments only, your role can't post`;
  if (!c.can_read) return `${where} · Posts only, your role can't answer comments`;
  return `${where} · Posts and answers comments`;
}

/** The Pages and Instagram accounts the signed-in user manages, each picked on its own. Signing in again reads the list again. */
function MetaAccounts({ projectId, signIn, signingIn }: { projectId: string; signIn: () => void; signingIn: boolean }) {
  const { data, error, mutate } = useSWR<SocialChannel[]>(`/projects/${projectId}/social-channels`, fetcher);
  const { pending, run } = useAction();
  // Each Instagram account right under its Page.
  const pages = (data ?? []).filter((c) => c.platform === "facebook");
  const accounts = pages.flatMap((p) => [p, ...(data ?? []).filter((c) => c.platform === "instagram" && c.guild_id === p.chat_id)]);
  const pick = (c: SocialChannel, picked: boolean) =>
    run(c.id, async () => {
      const fresh = await api<SocialChannel>(`/social-channels/${c.id}`, { method: "PATCH", body: { picked } });
      await mutate(data?.map((x) => (x.id === c.id ? fresh : x)), { revalidate: false });
    });
  if (error) return <LoadError error={error} what="your Pages" retry={() => mutate()} />;
  const label = (c: SocialChannel) => (c.platform === "instagram" ? `@${c.username}` : c.name);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm font-medium">Pages and accounts the agent may use</p>
        <Button size="sm" variant="ghost" onClick={signIn} loading={signingIn} className="px-2">
          <RefreshCw className="size-3.5" /> Refresh
        </Button>
      </div>
      {!data ? null : accounts.length === 0 ? (
        <p className="text-sm text-pretty text-muted-foreground">Facebook didn&apos;t share any Pages. Refresh, and pick your Pages when Facebook asks.</p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {accounts.map((c) => (
            <li key={c.id} className="flex items-center gap-3 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">{label(c)}</p>
                <p className="text-xs text-pretty text-muted-foreground">{metaNote(c)}</p>
              </div>
              <Switch
                checked={c.picked}
                onChange={(on) => pick(c, on)}
                disabled={pending === c.id || (!c.picked && !c.can_read && !c.can_send)}
                label={`Use ${label(c)}`}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Connects by signing in with Facebook, which only shows when the server has a Meta app. Facebook sends the browser back here
 *  with how it went, and the user picks the Pages and Instagram accounts the agent may use. */
function MetaSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { pending, run } = useAction();
  const { busy, call } = useConnection(projectId, "meta");
  const { mutate } = useSWRConfig();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("meta") === "connected";
    const failed = params.get("meta_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) {
      toast.success("Signed in with Facebook. Pick the Pages and accounts the agent may use.");
      mutate(`/projects/${projectId}/settings`);
      mutate(`/projects/${projectId}/social-channels`);
    }
  }, [mutate, projectId]);

  if (!me?.meta_sign_in && !settings.meta_connected && !settings.meta_reconnect) return null;
  const signIn = () =>
    run("meta", async () => {
      const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/meta/oauth`, { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (confirm("Disconnect Facebook and Instagram? Approved posts and replies will wait until you sign in again.")) call(undefined, "Facebook and Instagram disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Share2 className="size-4" /> Facebook and Instagram
        </span>
      }
      description="Reads the comments on your recent posts, drafts answers and posts for the Social page, and posts the ones you approve on your Pages and Instagram accounts."
      actions={
        settings.meta_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.meta_connected ? (
        <div className="flex flex-col gap-4 text-sm text-pretty">
          <p>Signed in as {settings.meta_name}. Each Page and account posts up to 5 times and answers up to 15 comments a day.</p>
          <MetaAccounts projectId={projectId} signIn={signIn} signingIn={pending === "meta"} />
        </div>
      ) : settings.meta_reconnect ? (
        <div className="flex flex-col items-start gap-4 text-sm text-pretty">
          <p className="rounded-lg bg-amber-50 px-3 py-2 text-amber-800">
            Facebook signed Splee out, so nothing goes out on your Pages or Instagram accounts. Approved posts went back to drafts. Sign in as{" "}
            {settings.meta_name} again to pick up where you left off.
          </p>
          <Button onClick={signIn} loading={pending === "meta"}>
            Sign in with Facebook again
          </Button>
        </div>
      ) : (
        <div className="flex flex-col items-start gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Facebook asks which of your Pages Splee may use. An Instagram account takes part when it&apos;s a business or creator account linked to
            one of them.
          </p>
          <Button onClick={signIn} loading={pending === "meta"}>
            Sign in with Facebook
          </Button>
        </div>
      )}
      {settings.meta_connected && (
        <>
          <LastFailure error={settings.facebook_error} at={settings.facebook_failed_at} what="post" />
          <LastFailure error={settings.instagram_error} at={settings.instagram_failed_at} what="post" />
        </>
      )}
    </Section>
  );
}

/** What each platform asks for when it signs in, under its Connect button. */
const AD_HINTS: Record<AdPlatform, string> = {
  linkedin: "Fills a Matched Audience in your Campaign Manager account.",
  google: "Fills a Customer Match list in your Google Ads account.",
  meta: "Fills a Custom Audience in your Meta ad account. Accept Meta's Custom Audience terms there first.",
};

/** Sends the browser to the platform's sign-in page. It comes back to this page with how it went, which AdAudiencesSection shows. */
async function signInToAds(projectId: string, platform: AdPlatform) {
  const { url } = await api<{ url: string }>(`/projects/${projectId}/ads/${platform}/oauth`, { method: "POST" });
  window.location.assign(url);
}

/** The ad platforms the project signed in to, each with its ad account and the hot leads' audience, and a button for each one
 *  this Splee has an app for. Each campaign's own audiences are turned on from the campaign's page. */
function AdAudiencesSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/ads`;
  const { data, error, mutate } = useSWR<AdsStatus>(key, fetcher);
  const { pending, run } = useAction();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("ads");
    const failed = params.get("ads_error");
    if (!done && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done && done in AD_PLATFORM_NAMES) {
      toast.success(`Connected to ${AD_PLATFORM_NAMES[done as AdPlatform]}`);
      mutate();
    }
  }, [mutate]);

  if (error) return <LoadError error={error} what="your ad platforms" retry={() => mutate()} />;
  if (!data || (!data.available.length && !data.connections.length)) return null;
  const open = data.available.filter((p) => !data.connections.some((c) => c.platform === p));
  const call = (what: string, path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>, done?: string) =>
    run(what, () => mutate(api<AdsStatus>(`${key}/${path}`, { method, body }), { revalidate: false }), done);
  return (
    <div id="ad-audiences" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <Megaphone className="size-4" /> Ad audiences
          </span>
        }
        description="Show ads to the people you're emailing. Turn on an audience per campaign from its page, and Splee keeps it in sync every hour. Only hashed addresses and numbers leave Splee."
      >
        {!!data.connections.length && (
          <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
            {data.connections.map((c) => (
              <ConnectedAds key={c.platform} projectId={projectId} connection={c} hot={data.audiences.find((a) => a.platform === c.platform)} pending={pending} call={call} />
            ))}
          </ul>
        )}
        {!!open.length && (
          <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap">
            {open.map((p) => (
              <div key={p} className="flex flex-col items-start gap-1.5 sm:flex-1 sm:basis-56">
                <Button variant="outline" onClick={() => run(p, () => signInToAds(projectId, p))} loading={pending === p}>
                  Connect {AD_PLATFORM_NAMES[p]}
                </Button>
                <p className="text-xs text-pretty text-muted-foreground">{AD_HINTS[p]}</p>
              </div>
            ))}
          </div>
        )}
      </Section>
    </div>
  );
}

type AdsCall = (what: string, path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>, done?: string) => Promise<boolean>;

function ConnectedAds({
  projectId,
  connection: c,
  hot,
  pending,
  call,
}: {
  projectId: string;
  connection: AdConnection;
  hot: AdsStatus["audiences"][number] | undefined;
  pending: string | null;
  call: AdsCall;
}) {
  const name = AD_PLATFORM_NAMES[c.platform];
  const { pending: signingIn, run } = useAction();
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? Splee deletes its audiences there and stops syncing.`)) call(`${c.platform}:off`, c.platform, "DELETE", undefined, `${name} disconnected`);
  };
  const pick = (account_id: string) => call(`${c.platform}:account`, `${c.platform}/account`, "PUT", { account_id }, "Ad account picked");
  const toggleHot = (on: boolean) => call(`${c.platform}:hot`, `${c.platform}/hot`, "PUT", { on }, on ? `Hot leads audience made in ${name}` : `Hot leads audience deleted in ${name}`);
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-pretty text-muted-foreground">
            {c.needs_reconnect ? "Signed out. Reconnect to keep its audiences in sync." : c.ad_account_id ? `Ad account: ${c.ad_account_name}` : "Pick the ad account to fill."}
          </p>
          {c.held_until && <p className="text-xs text-pretty text-amber-700">{name} asked Splee to slow down. Syncing again {timeAgo(c.held_until)}.</p>}
        </div>
        <div className="flex gap-2">
          {c.needs_reconnect && (
            <Button size="sm" onClick={() => run("reconnect", () => signInToAds(projectId, c.platform))} loading={signingIn === "reconnect"}>
              Reconnect
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={disconnect} loading={pending === `${c.platform}:off`}>
            Disconnect
          </Button>
        </div>
      </div>
      {c.ad_accounts.length > 1 && (
        <Field label="Ad account" hint="Audiences move to the account you pick." className="mt-3 sm:w-80">
          <Select value={c.ad_account_id} disabled={c.needs_reconnect || pending === `${c.platform}:account`} onChange={(e) => pick(e.target.value)}>
            {!c.ad_account_id && <option value="">Pick one</option>}
            {c.ad_accounts.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <div className="mt-3 flex items-center gap-3">
        <Switch
          checked={!!hot}
          onChange={toggleHot}
          disabled={pending === `${c.platform}:hot` || (!hot && (c.needs_reconnect || !c.ad_account_id))}
          label={`Hot leads audience in ${name}`}
        />
        <span className="min-w-0">
          <span className="block text-sm">Hot leads from every campaign</span>
          {hot ? <AudienceNote audience={hot} /> : <span className="block text-xs text-muted-foreground">An audience called Splee: Hot leads</span>}
        </span>
      </div>
    </li>
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
          <ChatSection
            projectId={projectId}
            weekly={s.chat_weekly_report}
            setWeekly={(chat_weekly_report) => update({ chat_weekly_report })}
            briefs={s.meeting_briefs}
            setBriefs={(meeting_briefs) => update({ meeting_briefs })}
          />

          <CrmSection projectId={projectId} settings={s} />
          <GoogleSheetsSection projectId={projectId} />
          <PaymentsSection projectId={projectId} />
          <MeetingsSection projectId={projectId} />
          <AdAudiencesSection projectId={projectId} />
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
          <FinderSection projectId={projectId} />
          <GoogleMapsSection />
          <UnipileSection projectId={projectId} settings={s} />
          <TwilioSection settings={s} />
          <XSection projectId={projectId} settings={s} />
          <RedditSection projectId={projectId} settings={s} />
          <BlueskySection projectId={projectId} settings={s} />
          <MastodonSection projectId={projectId} settings={s} />
          <ThreadsSection projectId={projectId} settings={s} />
          <YouTubeSection projectId={projectId} settings={s} />
          <StackExchangeSection projectId={projectId} settings={s} />
          <DevSection projectId={projectId} settings={s} />
          <DiscourseSection projectId={projectId} />
          <BlogSection projectId={projectId} />
          <LemmySection projectId={projectId} settings={s} />
          <NostrSection projectId={projectId} settings={s} />
          <TumblrSection projectId={projectId} settings={s} />
          <HashnodeSection projectId={projectId} settings={s} />
          <DiscordSection projectId={projectId} settings={s} />
          <TelegramSection projectId={projectId} settings={s} />
          <MetaSection projectId={projectId} settings={s} />

          <AutomationSection projectId={projectId} />
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
                  <code>close_date</code>, <code>owner</code>, <code>lost_reason</code> and <code>url</code>. &quot;Send a test&quot; posts the hot_lead shape with{" "}
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
