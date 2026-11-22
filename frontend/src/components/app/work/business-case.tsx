"use client";

import { useState } from "react";
import { Calculator, ExternalLink, Loader2, PenLine, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/misc";
import { CopyButton } from "@/components/app/settings/copy-button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { numbersOf, paybackMonths, total } from "@/lib/business-case";
import type { BusinessCase, Deal } from "@/lib/types";
import { dealMoney, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

/** In an open deal's window: what the product is worth to the buyer, which their champion can change and print. */
export function BusinessCaseSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/case`;
  const { data: bc, mutate } = useSWR<BusinessCase | null>(deal.status === "open" ? key : null, fetcher, {
    refreshInterval: (c) => (c?.status === "writing" ? 2500 : 0),
  });
  const [busy, setBusy] = useState(false);
  if (bc === undefined || deal.status !== "open") return null;
  const them = deal.company_name || deal.lead_name || "them";
  const writing = bc?.status === "writing";

  const write = async () => {
    setBusy(true);
    try {
      await mutate(api<BusinessCase | null>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const c = bc?.business_case;
  if (!bc || !c) {
    return (
      // The button wraps under the text on a phone rather than squeezing it into a narrow column.
      <section className="flex flex-wrap items-start justify-between gap-x-3 gap-y-2 rounded-xl border border-dashed border-border-strong p-4">
        <div className="min-w-0 flex-1 basis-56">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <Calculator className="size-3.5" /> Business case
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? "The agent is working out what it's worth to them from the conversation…"
              : bc?.status === "skipped"
                ? `No case yet: ${bc.skipped}`
                : bc?.status === "failed"
                  ? "Couldn't write the case. Try again."
                  : `What it's worth to ${them} a year, line by line from what they told you, for your champion to check against their own numbers and forward to whoever signs.`}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="outline" onClick={write} loading={busy}>
            <PenLine className="size-3.5" /> Write the business case
          </Button>
        )}
      </section>
    );
  }

  const currency = bc.investment?.currency ?? deal.currency;
  const money = (n: number) => dealMoney(Math.round(n), currency);
  const ours = total(c, numbersOf(c, null));
  const theirs = bc.numbers ? total(c, numbersOf(c, bc.numbers)) : null;
  const yearly = theirs ?? ours;
  const payback = paybackMonths(yearly, bc.investment?.amount ?? 0);
  const said = c.lines.filter((l) => l.basis === "said").length;
  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <Calculator className="size-3.5" /> Business case
        </p>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-1">
          {writing ? (
            <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Writing…
            </span>
          ) : (
            <Button type="button" size="sm" variant="ghost" onClick={write} loading={busy} title="Keeps the link, starts from the agent's numbers">
              <RefreshCw className="size-3.5" /> Write again
            </Button>
          )}
          <a
            href={`${bc.url}?preview=1`}
            target="_blank"
            rel="noreferrer"
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-sm font-medium hover:bg-accent"
          >
            <ExternalLink className="size-3.5" /> Open
          </a>
          <CopyButton value={bc.url} label="Copy link" />
        </div>
      </div>
      <p className="mt-2 text-sm font-medium text-pretty">{c.title}</p>
      <p className="mt-1 flex flex-wrap items-baseline gap-x-3 gap-y-1 text-sm">
        <span>
          <span className="font-semibold tabular-nums">{money(yearly)}</span> <span className="text-muted-foreground">a year</span>
        </span>
        {payback != null && (
          <span className="text-muted-foreground">pays back in {payback < 1 ? "under a month" : `${payback.toFixed(1)} months`}</span>
        )}
        {theirs == null && (
          <span className="text-muted-foreground">
            {said === 0
              ? "all from your estimates"
              : said === c.lines.length
                ? "all from their own numbers"
                : `${said} of ${c.lines.length} lines from their own numbers`}
          </span>
        )}
      </p>
      {theirs != null && bc.edited_at && (
        <p className="mt-2 flex flex-wrap items-center gap-2 text-xs">
          <Badge tone={theirs >= ours ? "green" : "amber"}>Their numbers</Badge>
          <span className="text-muted-foreground">
            {bc.edited_by || "They"} changed them {timeAgo(bc.edited_at)}: {money(theirs)} a year, against your {money(ours)}.
          </span>
        </p>
      )}
      {bc.skipped && <p className="mt-2 text-xs text-muted-foreground">Didn&apos;t write it again: {bc.skipped}</p>}
      <p className="mt-2 text-xs text-muted-foreground">
        {bc.last_viewed_at
          ? `${deal.lead_name || them} last looked ${timeAgo(bc.last_viewed_at)}, ${bc.views === 1 ? "once" : `${bc.views} times`}.`
          : `Not opened yet. Send ${deal.lead_name || them} the link.`}
      </p>
    </section>
  );
}
