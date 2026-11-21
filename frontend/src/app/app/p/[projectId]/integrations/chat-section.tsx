"use client";

import Link from "next/link";
import { Hash } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import type { Endpoint, SlackStatus } from "@/lib/types";
import { EndpointList } from "./endpoint-list";
import { AppSteps } from "./shared";

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

/** The chats that get the project's alerts. */
export function ChatSection({
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
