"use client";

import Link from "next/link";
import { Megaphone } from "lucide-react";
import useSWR from "swr";
import { Switch } from "@/components/ui/form";
import { Card, Skeleton } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { AD_PLATFORM_NAMES, type AdAudience, type AdConnection, type AdsStatus } from "@/lib/types";
import { cn, dateTime, num, plural, timeAgo } from "@/lib/utils";
import { MicroLabel } from "./states";

/** How an audience is doing, in one line: its size as the platform reported it, what Splee sent and when it last synced. */
export function AudienceNote({ audience: a, className }: { audience: AdAudience; className?: string }) {
  if (a.error) return <p className={cn("text-xs text-pretty text-red-700", className)}>{a.error}</p>;
  if (!a.synced_at) return <p className={cn("text-xs text-muted-foreground", className)}>Fills up within a minute.</p>;
  const size = a.size === null ? "Size not reported yet" : `Reaches about ${num(a.size)}`;
  return (
    <p className={cn("text-xs text-pretty text-muted-foreground", className)}>
      {size} · {plural(a.uploaded, "lead")} sent · Synced <span title={dateTime(a.synced_at)}>{timeAgo(a.synced_at)}</span>
    </p>
  );
}

/** The campaign's audience on each ad platform the project connected, each turned on and off on its own. */
export function AdAudiencesCard({ projectId, segmentId }: { projectId: string; segmentId: string }) {
  const key = `/segments/${segmentId}/audiences`;
  const { data, mutate } = useSWR<AdsStatus>(key, fetcher);
  const { pending, run } = useAction();

  if (!data) return <Skeleton className="h-28 rounded-2xl" />;
  if (!data.available.length && !data.connections.length) return null;

  const toggle = (c: AdConnection, on: boolean) => {
    const name = AD_PLATFORM_NAMES[c.platform];
    run(c.platform, () => mutate(api<AdsStatus>(`${key}/${c.platform}`, { method: "PUT", body: { on } }), { revalidate: false }), on ? `Audience made in ${name}` : `Audience deleted in ${name}`);
  };

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <Megaphone className="size-3.5" /> Ad audiences
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          Show ads to this campaign&apos;s leads once they&apos;re emailed. Anyone who opts out or bounces drops out within the hour.
        </p>
      </div>
      {data.connections.length === 0 ? (
        <p className="text-sm text-pretty">
          <Link href={`/app/p/${projectId}/integrations#ad-audiences`} className="font-medium underline underline-offset-4">
            Connect an ad platform
          </Link>{" "}
          on the Integrations page to start.
        </p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {data.connections.map((c) => {
            const audience = data.audiences.find((a) => a.platform === c.platform);
            const name = AD_PLATFORM_NAMES[c.platform];
            const blocked = c.needs_reconnect || !c.ad_account_id;
            return (
              <li key={c.platform} className="flex items-center gap-3 px-3 py-2.5">
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">
                    {name}
                    {c.ad_account_name && <span className="font-normal text-muted-foreground"> · {c.ad_account_name}</span>}
                  </p>
                  {c.needs_reconnect ? (
                    <p className="text-xs text-pretty text-amber-700">Reconnect {name} on the Integrations page to sync.</p>
                  ) : !c.ad_account_id ? (
                    <p className="text-xs text-pretty text-muted-foreground">Pick an ad account on the Integrations page.</p>
                  ) : audience ? (
                    <AudienceNote audience={audience} />
                  ) : (
                    <p className="text-xs text-muted-foreground">Off</p>
                  )}
                </div>
                <Switch
                  checked={!!audience}
                  onChange={(on) => toggle(c, on)}
                  disabled={pending === c.platform || (blocked && !audience)}
                  label={`${name} audience for this campaign`}
                />
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}
