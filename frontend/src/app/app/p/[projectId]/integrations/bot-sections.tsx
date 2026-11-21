"use client";

import { useState } from "react";
import { Bot, RefreshCw, Send } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings, SocialChannel } from "@/lib/types";
import { LastFailure, useConnection, submit, linkClass } from "./shared";

/** Invites the bot with what it reads and posts with: View Channels, Send Messages and Read Message History. */
const discordInvite = (botId: string) => `https://discord.com/oauth2/authorize?client_id=${botId}&scope=bot&permissions=${(1 << 10) | (1 << 11) | (1 << 16)}`;

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
export function DiscordSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
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
export function TelegramSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
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
