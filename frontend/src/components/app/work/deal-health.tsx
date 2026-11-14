"use client";

import { useEffect, useRef, useState } from "react";
import { Activity, Loader2, RefreshCw } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, DealHealth, DealStage } from "@/lib/types";
import { cn, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

const LOOK: Record<DealHealth["health"], { label: string; dot: string; text: string }> = {
  on_track: { label: "On track", dot: "bg-brand-500", text: "text-brand-700" },
  at_risk: { label: "At risk", dot: "bg-amber-500", text: "text-amber-700" },
  stalled: { label: "Stalled", dot: "bg-red-500", text: "text-red-700" },
};

/** Open deals weighted by the agent's likelihood where it rated them, and by their stage's probability elsewhere. */
export function agentWeighted(deals: Deal[], stages: DealStage[]): { amounts: { currency: string; value: number }[]; rated: number } {
  const odds = new Map(stages.map((s) => [s.id, s.probability]));
  const sums = new Map<string, number>();
  let rated = 0;
  for (const d of deals) {
    if (d.status !== "open") continue;
    if (d.health) rated++;
    const p = d.health?.likelihood ?? odds.get(d.stage_id) ?? 0;
    sums.set(d.currency, (sums.get(d.currency) ?? 0) + (d.value * p) / 100);
  }
  return { amounts: [...sums].map(([currency, value]) => ({ currency, value })), rated };
}

/** On the pipeline card: the agent's rating, with its reason on hover. */
export function HealthBadge({ deal }: { deal: Deal }) {
  if (!deal.health || deal.status !== "open") return null;
  const look = LOOK[deal.health.health];
  return (
    <p className={cn("mt-2 flex items-center gap-1.5 text-xs", look.text)} title={deal.health.why}>
      <span className={cn("size-1.5 shrink-0 rounded-full", look.dot)} />
      {look.label}
      <span className="text-muted-foreground tabular-nums">· {deal.health.likelihood}% likely</span>
    </p>
  );
}

/** In the deal's window: the agent's read on the deal and the next step, rated again on demand. See backend/src/agent/health.rs. */
export function DealHealthSection({ deal: initial }: { deal: Deal }) {
  const key = `/deals/${initial.id}`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data: deal = initial, mutate } = useSWR<Deal>(key, fetcher, {
    fallbackData: initial,
    refreshInterval: (d) => (d?.health_writing ? 2500 : 0),
  });
  const [busy, setBusy] = useState(false);
  // When the team asked, the rating that was showing then. Still showing once nothing is being written means the run failed.
  const [askedOver, setAskedOver] = useState<string | null | undefined>(undefined);
  const h = deal.health;
  const writing = deal.health_writing;
  const failed = askedOver !== undefined && !writing && (h?.created_at ?? null) === askedOver;
  // A new rating changes the pipeline card and the Weighted figure, so the board refreshes when one lands.
  const shown = useRef(h?.created_at);
  useEffect(() => {
    if (shown.current === h?.created_at) return;
    shown.current = h?.created_at;
    mutateAll((k) => typeof k === "string" && k.startsWith("/pipeline"));
  }, [h?.created_at, mutateAll]);
  if (deal.status !== "open") return null;

  const rate = async () => {
    setBusy(true);
    try {
      setAskedOver(h?.created_at ?? null);
      await mutate(api<Deal>(`${key}/health`, { method: "POST" }), { revalidate: false });
    } catch (e) {
      setAskedOver(undefined);
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const look = h ? LOOK[h.health] : null;
  return (
    <section className="flex items-start gap-3 rounded-xl border border-border p-4">
      <Activity className={cn("mt-0.5 size-4 shrink-0", look?.text ?? "text-muted-foreground")} />
      <div className="min-w-0 flex-1">
        {h && look ? (
          <>
            <p className="text-sm">
              <span className={cn("font-medium", look.text)}>{look.label}</span>
              <span className="text-muted-foreground tabular-nums"> · {h.likelihood}% likely to close</span>
            </p>
            <p className="mt-0.5 text-sm text-pretty text-foreground/80">{h.why}</p>
            {h.next_step && (
              <p className="mt-1 text-sm text-pretty">
                <span className="text-muted-foreground">Next: </span>
                {h.next_step}
              </p>
            )}
            <p className="mt-1 text-xs text-muted-foreground">The agent&apos;s read {timeAgo(h.created_at)}</p>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            {writing
              ? "The agent is reading the deal…"
              : "The agent hasn't rated this deal yet. It reads it and says how likely it is to close, why, and what to do next."}
          </p>
        )}
        {failed && <p className="mt-1 text-sm text-danger">The agent couldn&apos;t rate it. Try again.</p>}
      </div>
      {writing ? (
        <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
      ) : (
        <Button type="button" size="sm" variant="ghost" onClick={rate} loading={busy}>
          <RefreshCw className="size-3.5" /> {h ? "Rate again" : "Rate it"}
        </Button>
      )}
    </section>
  );
}
