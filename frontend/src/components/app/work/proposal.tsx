"use client";

import { useState } from "react";
import { BadgeCheck, ExternalLink, FileSignature, Loader2, PenLine, Plus, RefreshCw, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input, NumberInput, Select } from "@/components/ui/form";
import { CopyButton } from "@/components/app/settings/copy-button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { PERIODS, firstYear, monthly } from "@/lib/proposals";
import type { Deal, Proposal, ProposalItem } from "@/lib/types";
import { dealMoney, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

const BLANK: ProposalItem = { name: "", detail: "", quantity: 1, unit_price: 0, period: "month" };

/** In the deal's window: the proposal the agent wrote, its prices to set, and whether the buyer opened or accepted it. */
export function ProposalSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/proposal`;
  const { data: proposal, mutate } = useSWR<Proposal | null>(key, fetcher, {
    refreshInterval: (p) => (p?.status === "writing" ? 2500 : 0),
  });
  const [busy, setBusy] = useState<"write" | "save" | null>(null);
  // Edits to the prices and date, kept apart until saved. Null when nothing changed.
  const [draft, setDraft] = useState<{ items: ProposalItem[]; valid_until: string } | null>(null);
  const them = deal.lead_name || deal.company_name || "the buyer";

  const act = async (what: "write" | "save", call: () => Promise<Proposal | null>, done?: string) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
      if (done) toast.success(done);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => api<Proposal | null>(key, { method: "POST" }));

  if (proposal === undefined || (deal.status !== "open" && !proposal?.accepted_at)) return null;
  const writing = proposal?.status === "writing";
  if (!proposal?.title) {
    return (
      <section className="flex items-start justify-between gap-3 rounded-xl border border-dashed border-border-strong p-4">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <FileSignature className="size-3.5" /> Proposal
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? "The agent is reading the deal and writing the proposal…"
              : proposal?.status === "failed"
                ? "Couldn't write the proposal. Try again."
                : `Written from the conversation, with the prices you quoted. ${them} accepts it online with their name, and the deal is won.`}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
            <PenLine className="size-3.5" /> Write a proposal
          </Button>
        )}
      </section>
    );
  }

  const items = draft?.items ?? proposal.items;
  const total = firstYear(items);
  const perMonth = monthly(items);
  const money = (n: number) => dealMoney(n, proposal.currency);
  const links = (
    <>
      <a
        href={`${proposal.url}?preview=1`}
        target="_blank"
        rel="noreferrer"
        className="inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-sm font-medium hover:bg-accent"
      >
        <ExternalLink className="size-3.5" /> Open
      </a>
      <CopyButton value={proposal.url} label="Copy link" />
    </>
  );

  if (proposal.accepted_at) {
    return (
      <section className="flex flex-wrap items-center gap-3 rounded-xl border border-brand-200 bg-brand-50/60 p-4">
        <BadgeCheck className="size-5 shrink-0 text-brand-700" />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium text-brand-800">
            Accepted by {[proposal.accepted_name, proposal.accepted_title].filter(Boolean).join(", ")} {timeAgo(proposal.accepted_at)}
          </p>
          <p className="text-xs text-muted-foreground">
            {proposal.title}
            {items.length > 0 && ` · ${money(total)} in the first year`}
          </p>
        </div>
        <div className="flex items-center gap-1">{links}</div>
      </section>
    );
  }

  const edit = (changes: Partial<{ items: ProposalItem[]; valid_until: string }>) =>
    setDraft((d) => ({ items: d?.items ?? proposal.items, valid_until: d?.valid_until ?? proposal.valid_until ?? "", ...changes }));
  const setItem = (i: number, changes: Partial<ProposalItem>) => edit({ items: items.map((it, j) => (j === i ? { ...it, ...changes } : it)) });
  const save = async () => {
    if (!draft) return;
    const body = { items: draft.items, valid_until: draft.valid_until || null };
    if (await act("save", () => api<Proposal | null>(key, { method: "PUT", body }), "Prices saved")) setDraft(null);
  };

  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <FileSignature className="size-3.5" /> Proposal
        </p>
        <div className="ml-auto flex items-center gap-1">
          {writing ? (
            <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Writing…
            </span>
          ) : (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={write}
              loading={busy === "write"}
              title="Rewrites the text and keeps your prices"
            >
              <RefreshCw className="size-3.5" /> Write again
            </Button>
          )}
          {links}
        </div>
      </div>
      <p className="mt-2 text-sm font-medium text-pretty">{proposal.title}</p>
      <p className="mt-0.5 text-xs text-muted-foreground">
        {proposal.last_viewed_at
          ? `${them} last looked ${timeAgo(proposal.last_viewed_at)}, ${proposal.views === 1 ? "once" : `${proposal.views} times`}.`
          : `Not opened yet. Send ${them} the link to read and accept it.`}
      </p>

      <div className="mt-3 flex flex-col gap-2">
        {items.map((item, i) => (
          <div key={i} className="flex flex-wrap items-center gap-2">
            <Input
              value={item.name}
              onChange={(e) => setItem(i, { name: e.target.value })}
              placeholder="What it is"
              maxLength={120}
              className="h-8 min-w-36 flex-1 text-sm"
              aria-label="Item"
            />
            <NumberInput
              min={1}
              step={1}
              value={item.quantity}
              onValueChange={(quantity) => setItem(i, { quantity: Math.max(1, Math.round(quantity)) })}
              className="h-8 w-16 text-sm"
              aria-label="Quantity"
            />
            <span className="text-xs text-muted-foreground">×</span>
            <NumberInput
              min={0}
              step="any"
              value={item.unit_price}
              onValueChange={(unit_price) => setItem(i, { unit_price: Math.max(0, unit_price) })}
              className="h-8 w-24 text-sm"
              aria-label={`Price in ${proposal.currency}`}
            />
            <Select
              value={item.period}
              onChange={(e) => setItem(i, { period: e.target.value as ProposalItem["period"] })}
              className="h-8 w-auto text-sm"
              aria-label="Billed"
            >
              {Object.entries(PERIODS).map(([value, p]) => (
                <option key={value} value={value}>
                  {p.label}
                </option>
              ))}
            </Select>
            <button
              type="button"
              onClick={() => edit({ items: items.filter((_, j) => j !== i) })}
              className="text-muted-foreground hover:text-foreground"
              aria-label={`Remove ${item.name || "this item"}`}
            >
              <X className="size-3.5" />
            </button>
          </div>
        ))}
        {items.length === 0 && (
          <p className="text-xs text-muted-foreground">No prices were quoted in the conversation. Add them before you send it.</p>
        )}
      </div>
      <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-2">
        <Button type="button" size="sm" variant="ghost" onClick={() => edit({ items: [...items, BLANK] })} disabled={items.length >= 10}>
          <Plus className="size-3.5" /> Add a price
        </Button>
        <label className="flex items-center gap-2 text-xs text-muted-foreground">
          Valid until
          <Input
            type="date"
            value={draft?.valid_until ?? proposal.valid_until ?? ""}
            onChange={(e) => edit({ valid_until: e.target.value })}
            className="h-8 w-auto text-sm"
          />
        </label>
        <p className="ml-auto text-sm tabular-nums">
          {perMonth > 0 && <span className="text-muted-foreground">{money(perMonth)}/mo · </span>}
          <span className="font-medium">{money(total)}</span> <span className="text-muted-foreground">first year</span>
        </p>
      </div>
      {draft && (
        <div className="mt-3 flex justify-end gap-2">
          <Button type="button" size="sm" variant="ghost" onClick={() => setDraft(null)}>
            Discard
          </Button>
          <Button type="button" size="sm" onClick={save} loading={busy === "save"}>
            Save prices
          </Button>
        </div>
      )}
    </section>
  );
}
