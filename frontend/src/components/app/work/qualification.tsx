"use client";

import { useState } from "react";
import { ChevronDown, ListChecks, Loader2, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, QualificationItem } from "@/lib/types";
import { cn, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

// The order and keys of ELEMENTS in agent/qualify.rs.
const LABELS: Record<string, string> = {
  metrics: "Metrics",
  economic_buyer: "Economic buyer",
  decision_criteria: "Decision criteria",
  decision_process: "Decision process",
  paper_process: "Paper process",
  pain: "Pain",
  champion: "Champion",
  competition: "Competition",
};

const LOOK: Record<QualificationItem["status"], { bar: string; text: string; label: string }> = {
  confirmed: { bar: "bg-brand-500", text: "text-brand-700", label: "Confirmed" },
  partial: { bar: "bg-amber-400", text: "text-amber-700", label: "Partly" },
  unknown: { bar: "bg-muted", text: "text-muted-foreground", label: "Unknown" },
};

/** In an open deal's window: the agent's MEDDICC scorecard, what each element rests on and what to ask next. See backend/src/agent/qualify.rs. */
export function QualificationSection({ deal: initial }: { deal: Deal }) {
  const key = `/deals/${initial.id}`;
  const { data: deal = initial, mutate } = useSWR<Deal>(key, fetcher, {
    fallbackData: initial,
    refreshInterval: (d) => (d?.qualification_writing ? 2500 : 0),
  });
  const [busy, setBusy] = useState(false);
  // When the team asked, the scorecard that was showing then. Still showing once nothing is being written means the run failed.
  const [askedOver, setAskedOver] = useState<string | null | undefined>(undefined);
  const [open, setOpen] = useState(false);
  if (deal.status !== "open") return null;
  const q = deal.qualification;
  const writing = deal.qualification_writing;
  const failed = askedOver !== undefined && !writing && (q?.created_at ?? null) === askedOver;

  const qualify = async () => {
    setBusy(true);
    try {
      const before = q?.created_at ?? null;
      await mutate(api<Deal>(`${key}/qualify`, { method: "POST" }), { revalidate: false });
      setAskedOver(before);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const gaps = q?.items.filter((i) => i.status !== "confirmed" && i.ask) ?? [];
  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex items-start gap-3">
        <ListChecks className={cn("mt-0.5 size-4 shrink-0", q ? "text-brand-700" : "text-muted-foreground")} />
        <div className="min-w-0 flex-1">
          {q ? (
            <>
              <p className="text-sm">
                <span className="font-medium">Qualification</span>
                <span className="text-muted-foreground tabular-nums">
                  {" "}
                  · {q.confirmed} of {q.items.length} confirmed
                </span>
              </p>
              <div className="mt-2 flex gap-1" aria-hidden>
                {q.items.map((i) => (
                  <span
                    key={i.key}
                    className={cn("h-1.5 flex-1 rounded-full", LOOK[i.status].bar)}
                    title={`${LABELS[i.key] ?? i.key}: ${LOOK[i.status].label}`}
                  />
                ))}
              </div>
              {gaps.length > 0 && !open && (
                <p className="mt-2 text-sm text-pretty">
                  <span className="text-muted-foreground">Ask next: </span>
                  {gaps[0].ask}
                </p>
              )}
            </>
          ) : (
            <p className="text-sm text-muted-foreground">
              {writing
                ? "The agent is qualifying the deal…"
                : "The agent hasn't qualified this deal yet. It checks MEDDICC against what the buyer said, and says what to ask to fill each gap."}
            </p>
          )}
          {failed && <p className="mt-1 text-sm text-danger">The agent couldn&apos;t qualify it. Try again.</p>}
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="ghost" onClick={qualify} loading={busy}>
            <RefreshCw className="size-3.5" /> {q ? "Check again" : "Qualify it"}
          </Button>
        )}
      </div>
      {q && (
        <>
          {open && (
            <ul className="mt-3 flex flex-col divide-y divide-border border-t border-border">
              {q.items.map((i) => (
                <li key={i.key} className="grid grid-cols-[8.5rem_1fr] gap-3 py-2 text-sm">
                  <span className="flex items-center gap-2 font-medium">
                    <span className={cn("size-1.5 shrink-0 rounded-full", i.status === "unknown" ? "bg-border" : LOOK[i.status].bar)} />
                    {LABELS[i.key] ?? i.key}
                  </span>
                  <span className="min-w-0 text-pretty">
                    {i.evidence && <span className="text-foreground/80">{i.evidence}</span>}
                    {!i.evidence && <span className={LOOK[i.status].text}>{LOOK[i.status].label}</span>}
                    {i.ask && <span className="block text-muted-foreground">Ask: {i.ask}</span>}
                  </span>
                </li>
              ))}
            </ul>
          )}
          <div className="mt-2 flex items-center justify-between gap-3 pl-7">
            <p className="text-xs text-muted-foreground">The agent&apos;s scorecard {timeAgo(q.created_at)}</p>
            <button
              type="button"
              aria-expanded={open}
              onClick={() => setOpen(!open)}
              className="flex items-center gap-1 text-xs font-medium text-muted-foreground hover:text-foreground"
            >
              {open ? "Hide the scorecard" : "Show the scorecard"}
              <ChevronDown className={cn("size-3.5 transition-transform", open && "rotate-180")} />
            </button>
          </div>
        </>
      )}
    </section>
  );
}
