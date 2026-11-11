"use client";

import { useEffect } from "react";
import { Hash } from "lucide-react";
import { toast } from "sonner";
import useSWR from "swr";
import { LoadError, Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { SlackChannel, SlackProjectChannel, SlackStatus } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";

const label = (c: { name: string; private: boolean }) => (c.private ? `${c.name} (private)` : `#${c.name}`);

/** Add to Slack for the organization, then a channel per project. Hidden when the server has no Slack app. */
export function SlackSection() {
  const { me } = useMe();
  const { data, error, mutate } = useSWR<SlackStatus>("/organization/slack", fetcher);
  const connected = !!data?.workspace;
  const channels = useSWR<SlackChannel[]>(connected ? "/organization/slack/channels" : null, fetcher);
  const { pending, run } = useAction();
  const admin = me?.role === "owner" || me?.role === "admin";

  // Slack's sign-in comes back here with how it went.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("slack");
    const failed = params.get("slack_error");
    if (!done && !failed) return;
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done) toast.success("Splee is in Slack. Pick a channel for each project.");
    mutate();
  }, [mutate]);

  if (error) return <LoadError error={error} what="Slack" retry={() => mutate()} />;
  if (!data?.available) return null;

  const connect = () =>
    run("connect", async () => {
      const { url } = await api<{ url: string }>("/organization/slack/install", { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (!confirm(`Disconnect ${data.workspace?.team_name || "Slack"}? Projects stop posting there, and the buttons on old messages stop working.`)) return;
    run("disconnect", async () => {
      await api("/organization/slack", { method: "DELETE" });
      await mutate();
    }, "Disconnected Slack");
  };

  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Hash className="size-4" /> Slack
        </span>
      }
      description="Hot replies come with their drafted answer, and each morning brings the emails waiting for approval. Your team sends, edits and approves them right in Slack."
      actions={
        admin &&
        (connected ? (
          <Button variant="outline" size="sm" onClick={disconnect} loading={pending === "disconnect"}>
            Disconnect
          </Button>
        ) : (
          <Button onClick={connect} loading={pending === "connect"}>
            Add to Slack
          </Button>
        ))
      }
    >
      {!data.workspace ? (
        !admin && <p className="text-sm text-muted-foreground">An admin of your organization can add Splee to Slack.</p>
      ) : (
        <>
          <p className="text-sm">
            Connected to <span className="font-medium">{data.workspace.team_name || data.workspace.team_id}</span>
            {data.workspace.installed_by && <> by {data.workspace.installed_by}</>}.
          </p>
          {channels.error && (
            <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700">
              {channels.error instanceof Error ? channels.error.message : "Couldn't load your channels."}
            </p>
          )}
          <ul className="mt-4 divide-y divide-border rounded-lg border border-border">
            {data.projects.map((p) => (
              <ProjectRow
                key={p.project_id}
                project={p}
                channels={channels.data}
                saving={pending === `pick:${p.project_id}`}
                pick={(channel) =>
                  run(`pick:${p.project_id}`, async () => {
                    await api(`/projects/${p.project_id}/slack-channel`, { method: "PUT", body: { channel_id: channel || null } });
                    await mutate();
                  }, channel ? `${p.project_name} posts to Slack now` : `${p.project_name} stopped posting to Slack`)
                }
              />
            ))}
          </ul>
          <p className="mt-3 text-xs text-muted-foreground">
            Splee posts to public channels without joining them. For a private channel, invite @Splee to it first.
          </p>
        </>
      )}
    </Section>
  );
}

function ProjectRow({
  project,
  channels,
  saving,
  pick,
}: {
  project: SlackProjectChannel;
  channels: SlackChannel[] | undefined;
  saving: boolean;
  pick: (channel: string) => void;
}) {
  // A saved channel Slack no longer lists, like one archived since, still shows what the project posts to.
  const saved = project.channel_id && !channels?.some((c) => c.id === project.channel_id);
  return (
    <li className="px-3 py-2.5">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        <span className="min-w-0 text-sm font-medium break-words">{project.project_name}</span>
        <Select
          aria-label={`Slack channel for ${project.project_name}`}
          className="w-full sm:w-60"
          value={project.channel_id ?? ""}
          disabled={!channels || saving}
          onChange={(e) => pick(e.target.value)}
        >
          <option value="">{channels ? "Don't post" : "Loading channels…"}</option>
          {saved && <option value={project.channel_id!}>#{project.channel_name}</option>}
          {channels?.map((c) => (
            <option key={c.id} value={c.id}>
              {label(c)}
            </option>
          ))}
        </Select>
      </div>
      {project.error && project.failed_at && (
        <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
          The last post didn&apos;t go through, <span title={dateTime(project.failed_at)}>{timeAgo(project.failed_at)}</span>: {project.error}
        </p>
      )}
    </li>
  );
}
