"use client";

import { Gauge } from "lucide-react";
import { useState } from "react";
import { useSWRConfig } from "swr";
import { NumberInput, Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { useKeepFocus } from "@/hooks/use-keep-focus";
import { api } from "@/lib/api";
import type { SegmentWithStats } from "@/lib/types";
import { MicroLabel } from "./states";

const MAX = 5000;
/** Where a new limit starts, about what one warmed-up mailbox sends a day. */
const FIRST = 20;

/** The campaign's own cap on emails a day, on top of the project's limit. Saved when it's switched, and when the number loses
 * focus. The page keys it on the saved limit, so a change from elsewhere starts the number over. */
export function DailyLimitCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const root = useKeepFocus<HTMLDivElement>("daily-limit");
  const [draft, setDraft] = useState(segment.daily_limit ?? FIRST);
  const on = segment.daily_limit !== null;

  const save = (daily_limit: number | null) =>
    run("daily_limit", () =>
      mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { daily_limit } }), { revalidate: false }),
    );
  const commit = () => {
    const n = Math.min(Math.max(Math.round(draft), 1), MAX);
    setDraft(n);
    if (n !== segment.daily_limit) save(n);
  };

  return (
    <Card ref={root} className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <Gauge className="size-3.5" /> Daily limit
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          Every campaign in the project shares its daily limit. A limit of its own keeps this one from taking most of it.
        </p>
      </div>
      <ul className="divide-y divide-border rounded-lg border border-border">
        <li className="flex items-center gap-3 px-3 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">Its own limit</p>
            <p className="text-xs text-pretty text-muted-foreground">Counts its first emails and follow-ups. Replies to leads don&apos;t count.</p>
          </div>
          <Switch checked={on} onChange={(v) => save(v ? draft : null)} disabled={pending === "daily_limit"} label="Its own daily limit" />
        </li>
        {on && (
          // The input leaves blur to NumberInput, so the save listens here, where focus leaving it bubbles up to.
          <li className="flex flex-wrap items-center gap-2 px-3 py-2.5 text-sm" onBlur={commit}>
            Up to
            <NumberInput
              value={draft}
              onValueChange={setDraft}
              onKeyDown={(e) => e.key === "Enter" && e.currentTarget.blur()}
              min={1}
              max={MAX}
              step={1}
              disabled={pending === "daily_limit"}
              aria-label="Emails a day from this campaign"
              className="h-8 w-20 tabular-nums"
            />
            emails a day
          </li>
        )}
      </ul>
    </Card>
  );
}
