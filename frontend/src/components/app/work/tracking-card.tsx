"use client";

import { MousePointerClick } from "lucide-react";
import { useSWRConfig } from "swr";
import { Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { api } from "@/lib/api";
import type { SegmentWithStats } from "@/lib/types";
import { MicroLabel } from "./states";

const SETTINGS = [
  {
    key: "track_clicks",
    label: "Track link clicks",
    note: "Links go through your tracking address first, so you see who clicked what. Unsubscribe links are left alone.",
  },
  {
    key: "track_opens",
    label: "Track opens",
    note: "Adds an invisible image, which can push emails toward spam. Apple Mail opens every email itself, so open counts are estimates.",
  },
] as const;

/** Whether this campaign's first emails and follow-ups count clicks and opens. Saved as soon as it's switched. */
export function TrackingCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();

  const save = (key: (typeof SETTINGS)[number]["key"], on: boolean) =>
    run(key, () => mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { [key]: on } }), { revalidate: false }));

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <MousePointerClick className="size-3.5" /> Tracking
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          Applies to emails sent from now on, follow-ups included. Replies you send from the inbox are never tracked.
        </p>
      </div>
      <ul className="divide-y divide-border rounded-lg border border-border">
        {SETTINGS.map((s) => (
          <li key={s.key} className="flex items-center gap-3 px-3 py-2.5">
            <div className="min-w-0 flex-1">
              <p className="text-sm font-medium">{s.label}</p>
              <p className="text-xs text-pretty text-muted-foreground">{s.note}</p>
            </div>
            <Switch checked={segment[s.key]} onChange={(on) => save(s.key, on)} disabled={pending === s.key} label={s.label} />
          </li>
        ))}
      </ul>
    </Card>
  );
}
