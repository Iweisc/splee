"use client";

import { use, useState } from "react";
import { CalendarCheck, CalendarClock, ClipboardList, Contact, Hash, MailCheck, MailPlus, RefreshCw, Send, Trash2, UserPlus, UserSearch, Webhook } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { CodeBlock } from "@/components/app/settings/code-block";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { CopyField } from "@/components/app/settings/copy-button";
import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge, Host } from "@/components/ui/misc";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import { CRM_NAMES, type CrmName, type Endpoint, type ProjectSettings, type WebhookDelivery, type WebhookEvent } from "@/lib/types";
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
  ["ntfy", "https://ntfy.sh/ and a topic only you know"],
  ["Pushover", "https://api.pushover.net/1/messages.json?token=APP_TOKEN&user=USER_KEY"],
  ["Gotify", "your server's /message?token=APP_TOKEN"],
  ["Lark or Feishu", "group Settings → Bots → Custom Bot"],
  ["DingTalk", "group Bots → Custom, with IP security"],
  ["WeCom", "the group chat's ⋯ → Add Group Robot"],
  ["Webex", "Incoming Webhooks on the Webex App Hub"],
  ["Mattermost", "Integrations → Incoming Webhooks"],
  ["Rocket.Chat", "Integrations → New → Incoming"],
  ["Zulip", "an incoming webhook bot's Slack compatible URL"],
];

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
  if (host === "ntfy.sh") return "ntfy";
  if (host === "api.pushover.net") return "Pushover";
  if (host === "open.larksuite.com") return "Lark";
  if (host === "open.feishu.cn") return "Feishu";
  if (host === "oapi.dingtalk.com") return "DingTalk";
  if (host === "qyapi.weixin.qq.com") return "WeCom";
  if (host === "webexapis.com") return "Webex";
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
function LastFailure({ error, at, what = "alert" }: { error: string | null; at: string | null; what?: "alert" | "sync" | "check" | "send" | "lookup" | "copy" }) {
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
} as const satisfies Record<CrmName, { name: string; record: string; field: string; hint: string }>;

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

/** For a field with its own Add or Connect button: Enter there presses it. A disabled button already stops an empty or repeat submit. */
const submit = (run: () => void) => (e: React.FormEvent) => {
  e.preventDefault();
  run();
};

/** Tokens are write-only, so once connected this only offers Disconnect. Connecting checks the token with the CRM first. */
function CrmSection({ projectId, settings, crm }: { projectId: string; settings: ProjectSettings; crm: CrmName }) {
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
      description={`Adds hot and booked leads as ${record}, with a note holding their reply.`}
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
        <form onSubmit={submit(connect)}>
          <Field label={field} hint={hint}>
            <div className="flex gap-2">
              {/* The label also wraps Connect, so name the input itself. */}
              <Input aria-label={field} type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings[`${crm}_error`]} at={settings[`${crm}_failed_at`]} what="sync" />
    </Section>
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
  hint: React.ReactNode;
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
        hint={CHAT_APPS.map(([app, where]) => (
          <span key={app} className="block">
            {app}: {where}
          </span>
        ))}
        placeholder="https://hooks.slack.com/…"
      />
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
} as const;

const FINDERS = {
  hunter: "Hunter",
  findymail: "Findymail",
  prospeo: "Prospeo",
  anymailfinder: "Anymail Finder",
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
          <Field label="Service" className="sm:w-44">
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
    if (confirm("Disconnect Unipile? Due LinkedIn steps will wait for you to send them.")) call(undefined, "Unipile disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UserPlus className="size-4" /> LinkedIn sending
        </span>
      }
      description="Sends each due LinkedIn step from your own LinkedIn account through Unipile."
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
          messages wait until they accept the connect note. A reply on LinkedIn stops their sequence and emails.
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
        <ul className="flex flex-col gap-1 text-xs text-pretty text-muted-foreground">
          <li>Tally: Integrations → Webhooks.</li>
          <li>Typeform: Connect → Webhooks.</li>
          <li>Fillout: Integrate → Webhook, in Simple mode.</li>
          <li>Jotform: Settings → Integrations → WebHooks.</li>
          <li>Paperform: After Submission → Integrations &amp; Webhooks → Add Webhook, on New Submission.</li>
          <li>Webflow: Site settings → Integrations → Webhooks, with Form submission.</li>
          <li>
            Anything else can POST <code>{`{"email": "...", "name": "..."}`}</code>.
          </li>
        </ul>
        <p className="text-xs text-pretty text-muted-foreground">
          Answers that aren&apos;t a name, email, company, title, website or LinkedIn go in the lead&apos;s notes.
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
    >
      {(s, update) => (
        <>
          <ChatSection projectId={projectId} weekly={s.chat_weekly_report} setWeekly={(chat_weekly_report) => update({ chat_weekly_report })} />

          {CRM_NAMES.map((crm) => (
            <CrmSection key={crm} projectId={projectId} settings={s} crm={crm} />
          ))}
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
              <ul className="flex flex-col gap-1 text-xs text-pretty text-muted-foreground">
                <li>Cal.com: Settings → Developer → Webhooks → New, paste this URL and pick Booking created, rescheduled and cancelled.</li>
                <li>SavvyCal: Settings → Integrations, then + next to Webhooks.</li>
                <li>Zcal: Integrations, then Connect next to Webhooks, and paste this URL as the Subscriber URL.</li>
                <li>TidyCal: Integrations → Automations → Add webhook, with booking created, approved, rescheduled and cancelled.</li>
                <li>OnceHub: Account Integrations → APIs &amp; Webhooks → Create webhook, with booking scheduled, rescheduled and canceled.</li>
                <li>
                  Calendly: subscribe it to <code>invitee.created</code> and <code>invitee.canceled</code> with Calendly&apos;s webhook API.
                </li>
                <li>
                  Anything else, like a Zapier step, can POST <code>{`{"email": "...", "start_time": "..."}`}</code>.
                </li>
              </ul>
              <p className="text-xs text-pretty text-muted-foreground">Splee finds the lead by email and shows the call&apos;s time on it.</p>
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
