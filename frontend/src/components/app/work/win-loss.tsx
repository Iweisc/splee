"use client";

import { useState } from "react";
import { ChevronRight, Lightbulb, Loader2, Scale, Swords } from "lucide-react";
import { Card, EmptyState } from "@/components/ui/misc";
import type { Deal, DealReview } from "@/lib/types";
import { cn, plural } from "@/lib/utils";
import { moneyList, sumByCurrency } from "./deal-dialog";
import { reasonLabel } from "./deal-review";
import { MicroLabel } from "./states";

type Reviewed = Deal & { status: "won" | "lost"; review: DealReview & { reason: string } };

// Not Map.groupBy: Safari before 17.4 lacks it.
function group<T>(items: T[], key: (t: T) => string): [string, T[]][] {
  const groups = new Map<string, T[]>();
  for (const t of items) groups.set(key(t), [...(groups.get(key(t)) ?? []), t]);
  return [...groups];
}

const isReviewed = (d: Deal): d is Reviewed => d.status !== "open" && !!d.review?.reason && !d.review.writing;

/** The pipeline's Win/loss view: the agent's reviews of the deals on the board that closed in the last 90 days, grouped. */
export function WinLoss({ deals, currency, onOpen }: { deals: Deal[]; currency: string; onOpen: (d: Deal) => void }) {
  const closed = deals.filter((d) => d.status !== "open");
  const reviewed = closed.filter(isReviewed);
  const writing = closed.filter((d) => d.review?.writing).length;
  if (!closed.length) {
    return (
      <Card>
        <EmptyState icon={<Scale className="size-5" />} title="Nothing won or lost in 90 days">
          When a deal is won or lost, the agent reviews why, and this view shows what wins deals, what loses them and who you lose to.
        </EmptyState>
      </Card>
    );
  }
  const lessons = [...reviewed].filter((d) => d.review.lesson).sort((a, b) => (b.closed_at ?? "").localeCompare(a.closed_at ?? ""));
  return (
    <div className="flex flex-col gap-4">
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        {writing > 0 && <Loader2 className="size-3.5 animate-spin" />}
        The agent reviewed {reviewed.length} of {plural(closed.length, "deal")} closed in 90 days
        {writing > 0 ? `, and is reviewing ${writing} now.` : "."}
      </p>
      <div className="grid gap-4 lg:grid-cols-2">
        <Reasons outcome="won" deals={reviewed} currency={currency} onOpen={onOpen} />
        <Reasons outcome="lost" deals={reviewed} currency={currency} onOpen={onOpen} />
        <Competitors deals={reviewed} />
        {lessons.length > 0 && (
          <Card className="p-4">
            <MicroLabel className="mb-2 flex items-center gap-1.5">
              <Lightbulb className="size-3.5" /> Lessons
            </MicroLabel>
            <ul className="flex flex-col divide-y divide-border">
              {lessons.slice(0, 6).map((d) => (
                <li key={d.id}>
                  <button
                    type="button"
                    onClick={() => onOpen(d)}
                    className="flex w-full items-start gap-2.5 py-2.5 text-left text-sm hover:text-foreground"
                  >
                    <span className={cn("mt-1.5 size-1.5 shrink-0 rounded-full", d.status === "won" ? "bg-brand-500" : "bg-red-500")} />
                    <span className="min-w-0 flex-1 text-pretty">
                      {d.review.lesson}
                      <span className="text-muted-foreground"> · {d.company_name || d.lead_name}</span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          </Card>
        )}
      </div>
    </div>
  );
}

function Reasons({ outcome, deals, currency, onOpen }: { outcome: "won" | "lost"; deals: Reviewed[]; currency: string; onOpen: (d: Deal) => void }) {
  const [open, setOpen] = useState<string | null>(null);
  const mine = deals.filter((d) => d.status === outcome);
  const groups = group(mine, (d) => d.review.reason).sort((a, b) => b[1].length - a[1].length);
  const bar = outcome === "won" ? "bg-brand-500" : "bg-red-500";
  return (
    <Card className="p-4">
      <div className="mb-2 flex items-baseline justify-between gap-3">
        <MicroLabel>{outcome === "won" ? "Why we win" : "Why we lose"}</MicroLabel>
        <span className="text-xs text-muted-foreground tabular-nums">
          {plural(mine.length, "deal")} · {moneyList(sumByCurrency(mine), currency)}
        </span>
      </div>
      {groups.length === 0 ? (
        <p className="py-3 text-sm text-muted-foreground">{outcome === "won" ? "No reviewed wins yet." : "No reviewed losses yet."}</p>
      ) : (
        <ul className="flex flex-col divide-y divide-border">
          {groups.map(([reason, list]) => (
            <li key={reason}>
              <button
                type="button"
                aria-expanded={open === reason}
                onClick={() => setOpen(open === reason ? null : reason)}
                className="flex w-full items-center gap-3 py-2.5 text-left"
              >
                <ChevronRight className={cn("size-3.5 shrink-0 text-muted-foreground transition-transform", open === reason && "rotate-90")} />
                <span className="min-w-0 flex-1 text-sm font-medium">{reasonLabel(outcome, reason)}</span>
                <span className="h-1.5 w-20 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden>
                  <span className={cn("block h-full rounded-full", bar)} style={{ width: `${(list.length / mine.length) * 100}%` }} />
                </span>
                <span className="w-14 shrink-0 text-right text-xs text-muted-foreground tabular-nums">{plural(list.length, "deal")}</span>
              </button>
              {open === reason && (
                <ul className="mb-2 ml-6 flex flex-col gap-1">
                  {list.map((d) => (
                    <li key={d.id}>
                      <button type="button" onClick={() => onOpen(d)} className="w-full rounded-lg px-2 py-1.5 text-left hover:bg-muted">
                        <p className="flex items-baseline justify-between gap-3 text-sm">
                          <span className="truncate font-medium">{d.company_name || d.lead_name}</span>
                          <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{moneyList(sumByCurrency([d]), currency)}</span>
                        </p>
                        <p className="line-clamp-2 text-xs text-pretty text-muted-foreground">{d.review.story}</p>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function Competitors({ deals }: { deals: Reviewed[] }) {
  const rows = group(
    deals.filter((d) => d.review.competitor),
    (d) => d.review.competitor.toLowerCase(),
  )
    .map(([, list]) => ({
      name: list[0].review.competitor,
      won: list.filter((d) => d.status === "won").length,
      lost: list.filter((d) => d.status === "lost").length,
    }))
    .sort((a, b) => b.won + b.lost - (a.won + a.lost));
  return (
    <Card className="p-4">
      <MicroLabel className="mb-2 flex items-center gap-1.5">
        <Swords className="size-3.5" /> Competitors
      </MicroLabel>
      {rows.length === 0 ? (
        <p className="py-3 text-sm text-muted-foreground">No competitor came up in the reviewed deals.</p>
      ) : (
        <ul className="flex flex-col divide-y divide-border">
          {rows.map((r) => (
            <li key={r.name} className="flex items-center gap-3 py-2.5 text-sm">
              <span className="min-w-0 flex-1 truncate font-medium">{r.name}</span>
              <span className="flex h-1.5 w-24 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden>
                <span className="h-full bg-brand-500" style={{ width: `${(r.won / (r.won + r.lost)) * 100}%` }} />
                <span className="h-full bg-red-500" style={{ width: `${(r.lost / (r.won + r.lost)) * 100}%` }} />
              </span>
              <span className="w-28 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
                {r.won} won, {r.lost} lost
              </span>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
