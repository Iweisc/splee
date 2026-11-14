"use client";

import { useState } from "react";
import { Loader2, RefreshCw, Swords, Trophy, XCircle } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, DealReview } from "@/lib/types";
import { cn, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

// fit, timing and price mean opposite things on a win and a loss. See WON_REASONS and LOST_REASONS in agent/reviews.rs.
export const REASONS: Record<"won" | "lost", Record<string, string>> = {
  won: { value: "Clear value", fit: "Right fit", champion: "A champion", timing: "Good timing", price: "Price", proof: "Proof", other: "Other" },
  lost: {
    price: "Price",
    competitor: "Chose a competitor",
    timing: "Not now",
    no_decision: "No decision",
    fit: "Not a fit",
    went_dark: "Went dark",
    other: "Other",
  },
};

export const reasonLabel = (outcome: "won" | "lost", reason: string) => REASONS[outcome][reason] ?? reason;

/** In a closed deal's window: why the agent thinks it was won or lost, reviewed again on demand. See backend/src/agent/reviews.rs. */
export function DealReviewSection({ deal: initial }: { deal: Deal }) {
  const key = `/deals/${initial.id}`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data: deal = initial, mutate } = useSWR<Deal>(key, fetcher, {
    fallbackData: initial,
    refreshInterval: (d) => (d?.review?.writing ? 2500 : 0),
  });
  const [busy, setBusy] = useState(false);
  if (deal.status === "open") return null;
  const outcome = deal.status;
  const r: DealReview | null = deal.review;
  const writing = !!r?.writing;
  const written = r && r.reason ? r : null;

  const review = async () => {
    setBusy(true);
    try {
      await mutate(api<Deal>(`${key}/review`, { method: "POST" }), { revalidate: false });
      mutateAll((k) => typeof k === "string" && k.startsWith("/pipeline"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const Icon = outcome === "won" ? Trophy : XCircle;
  const tone = outcome === "won" ? "text-brand-700" : "text-red-700";
  return (
    <section className="flex items-start gap-3 rounded-xl border border-border p-4">
      <Icon className={cn("mt-0.5 size-4 shrink-0", written ? tone : "text-muted-foreground")} />
      <div className="min-w-0 flex-1">
        {written && !writing ? (
          <>
            <p className="text-sm">
              <span className={cn("font-medium", tone)}>{outcome === "won" ? "Won" : "Lost"}</span>
              <span className="text-muted-foreground"> · </span>
              <span className="font-medium">{reasonLabel(outcome, written.reason!)}</span>
              {written.competitor && (
                <span className="text-muted-foreground">
                  {" "}
                  · <Swords className="inline size-3" /> {written.competitor}
                </span>
              )}
            </p>
            <p className="mt-0.5 text-sm text-pretty text-foreground/80">{written.story}</p>
            {written.turning_point && (
              <p className="mt-1 text-sm text-pretty">
                <span className="text-muted-foreground">Turning point: </span>
                {written.turning_point}
              </p>
            )}
            {written.lesson && (
              <p className="mt-1 text-sm text-pretty">
                <span className="text-muted-foreground">Next time: </span>
                {written.lesson}
              </p>
            )}
            <p className="mt-1 text-xs text-muted-foreground">The agent&apos;s review {timeAgo(written.created_at)}</p>
          </>
        ) : (
          <p className="text-sm text-muted-foreground">
            {writing
              ? `The agent is reading why this deal was ${outcome}…`
              : r
                ? "The agent couldn't review this deal. Try again."
                : `The agent reviews every won and lost deal within a few minutes: why it was ${outcome}, the turning point and what to do next time.`}
          </p>
        )}
      </div>
      {writing ? (
        <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
      ) : (
        <Button type="button" size="sm" variant="ghost" onClick={review} loading={busy}>
          <RefreshCw className="size-3.5" /> {r ? "Review again" : "Review it"}
        </Button>
      )}
    </section>
  );
}
