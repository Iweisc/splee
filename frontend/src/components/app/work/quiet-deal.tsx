"use client";

import { useState } from "react";
import Link from "next/link";
import { Check, Hourglass, Loader2, PenLine, Send } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal } from "@/lib/types";
import { plural } from "@/lib/utils";
import { toastError } from "./states";

/** Keep in sync with QUIET_DAYS in backend/src/agent/nudges.rs. */
export const QUIET_DAYS = 14;
const DAY = 86_400_000;

/** Days since an open deal last moved, once it's been QUIET_DAYS, else null. */
export function quietDays(deal: Deal): number | null {
  if (deal.status !== "open") return null;
  const days = Math.floor((Date.now() - new Date(deal.last_touch_at).getTime()) / DAY);
  return days >= QUIET_DAYS ? days : null;
}

/** The nudge written in this quiet stretch. One from before the last email or call is history. */
function currentNudge(deal: Deal) {
  return deal.nudge && deal.nudge.created_at >= deal.last_touch_at ? deal.nudge : null;
}

/** On the pipeline card: how long the deal has been quiet, and whether a nudge waits. */
export function QuietBadge({ deal }: { deal: Deal }) {
  const days = quietDays(deal);
  if (days === null) return null;
  const waiting = currentNudge(deal)?.email_status === "draft";
  return (
    <p className="mt-2 flex items-center gap-1 text-xs text-amber-700">
      <Hourglass className="size-3 shrink-0" /> Quiet for {plural(days, "day")}
      {waiting && <span className="text-muted-foreground">· nudge ready</span>}
    </p>
  );
}

/** In the deal's window: why it went quiet and the nudge the agent drafted, to send from here. See agent/nudges.rs. */
export function QuietDeal({ deal: initial }: { deal: Deal }) {
  const key = `/deals/${initial.id}`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data: deal = initial, mutate } = useSWR<Deal>(key, fetcher, {
    fallbackData: initial,
    refreshInterval: (d) => (d?.nudge?.status === "writing" ? 3000 : 0),
  });
  const [busy, setBusy] = useState<"write" | "send" | null>(null);
  const days = quietDays(deal);
  const nudge = currentNudge(deal);
  if (deal.status !== "open" || (days === null && !nudge)) return null;

  const act = async (what: "write" | "send", call: () => Promise<unknown>, done?: string) => {
    setBusy(what);
    try {
      await call();
      if (done) toast.success(done);
      mutateAll((k) => typeof k === "string" && k.startsWith("/pipeline"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => mutate(api<Deal>(`${key}/nudge`, { method: "POST" }), { revalidate: false }));
  const send = () =>
    act(
      "send",
      async () => {
        await api(`/emails/${nudge!.email_id}`, { method: "PATCH", body: { status: "approved" } });
        await mutate();
      },
      "Approved. It goes out in the thread shortly.",
    );

  const draft = nudge?.status === "done" && nudge.email_status === "draft" ? nudge : null;
  const skipped = nudge?.status === "done" && !nudge.email_id;
  const writing = nudge?.status === "writing";

  return (
    <section className="rounded-xl border border-amber-200 bg-amber-50/50 p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium text-amber-800">
            <Hourglass className="size-3.5" /> {days === null ? "Nudged while quiet" : `Quiet for ${plural(days, "day")}`}
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? "The agent is reading the conversation and writing a nudge…"
              : skipped
                ? `No nudge: ${nudge.why}.`
                : nudge?.why || "No email either way, call or stage change since. The agent can write a nudge with a reason to answer now."}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          !draft &&
          !(nudge?.email_status && nudge.email_status !== "cancelled") && (
            <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
              <PenLine className="size-3.5" /> Write a nudge
            </Button>
          )
        )}
      </div>

      {draft && (
        <div className="mt-3 rounded-lg border border-border bg-card p-3">
          <p className="text-sm font-medium">{draft.subject}</p>
          <p className="mt-1 text-sm whitespace-pre-wrap text-foreground/80">{draft.body}</p>
          <div className="mt-3 flex flex-wrap items-center justify-end gap-2">
            <Link href={`/app/p/${deal.project_id}/emails`} className="mr-auto text-xs text-muted-foreground hover:text-foreground hover:underline">
              Edit it on the Emails page
            </Link>
            <Button type="button" size="sm" variant="ghost" onClick={write} loading={busy === "write"}>
              <PenLine className="size-3.5" /> Write another
            </Button>
            <Button type="button" size="sm" onClick={send} loading={busy === "send"}>
              <Send className="size-3.5" /> Approve and send
            </Button>
          </div>
        </div>
      )}
      {nudge?.email_status && ["approved", "scheduled", "sending"].includes(nudge.email_status) && (
        <p className="mt-2 flex items-center gap-1.5 text-xs text-muted-foreground">
          <Check className="size-3.5" /> The nudge is approved and goes out in the thread shortly.
        </p>
      )}
    </section>
  );
}
