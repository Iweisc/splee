"use client";

import Link from "next/link";
import { Trash2, Zap } from "lucide-react";
import useSWR from "swr";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge, Host } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import type { HookEvent, HookSubscription } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";
import { LastFailure, hostOf } from "./shared";

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
export function AutomationSection({ projectId }: { projectId: string }) {
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
