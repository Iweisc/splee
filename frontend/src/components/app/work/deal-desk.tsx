"use client";

import { useState } from "react";
import { BadgeCheck, Loader2, Pencil, Plus, ShieldAlert, Trash2 } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input, NumberInput, Select, Switch, Textarea } from "@/components/ui/form";
import { Badge } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { PERIODS } from "@/lib/proposals";
import type { DiscountApproval, PriceList, Product, Proposal, ProposalItem } from "@/lib/types";
import { dealMoney, timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

/** "25%" or "33.34%". */
export const pct = (n: number) => `${Number(n.toFixed(2))}%`;

/** Keep in sync with discount in backend/src/proposals.rs: the deepest discount off the list among the items. `list` gives
 * each item's list price, from the price list for unsaved edits. */
export function discount(items: ProposalItem[], list: (i: ProposalItem) => number | undefined) {
  return items.reduce((deepest, i) => {
    const l = list(i);
    return l && l > 0 ? Math.max(deepest, ((l - i.unit_price) / l) * 100) : deepest;
  }, 0);
}

const VERDICT = {
  approve: { label: "Approve", tone: "green" },
  counter: { label: "Counter", tone: "amber" },
  decline: { label: "Decline", tone: "red" },
} as const;

/** The agent's read on an ask: its call, why, and what to get in return. `brief` shows only the call, for lists. */
export function ApprovalRead({ a, brief }: { a: DiscountApproval; brief?: boolean }) {
  if (a.read_status === "writing") {
    return (
      <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" /> The agent is reading the deal…
      </p>
    );
  }
  if (!a.read) return <p className="text-xs text-muted-foreground">The agent couldn&apos;t read this one. Decide from the deal.</p>;
  const v = VERDICT[a.read.verdict];
  return (
    <div className="flex flex-col gap-1 text-sm text-pretty">
      <p className={brief ? "line-clamp-2 text-muted-foreground" : undefined}>
        <Badge tone={v.tone}>
          {v.label}
          {a.read.counter_pct ? ` at ${pct(a.read.counter_pct)}` : ""}
        </Badge>{" "}
        {a.read.summary}
      </p>
      {!brief && a.read.reasons.length > 0 && (
        <ul className="list-disc pl-5 text-muted-foreground">
          {a.read.reasons.map((r) => (
            <li key={r}>{r}</li>
          ))}
        </ul>
      )}
      {!brief && a.read.ask_for && (
        <p>
          <span className="font-medium">Ask for:</span> {a.read.ask_for}
        </p>
      )}
    </div>
  );
}

/** Approve or decline with a note. Owners and admins only. */
export function Decide({ a, onDecided }: { a: DiscountApproval; onDecided: (p: Proposal | null) => unknown }) {
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState<"approve" | "decline" | null>(null);
  const decide = async (approve: boolean) => {
    setBusy(approve ? "approve" : "decline");
    try {
      await onDecided(await api<Proposal | null>(`/discount-approvals/${a.id}`, { method: "POST", body: { approve, note } }));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-2">
      <Input
        value={note}
        onChange={(e) => setNote(e.target.value)}
        placeholder="A note for them, optional"
        maxLength={600}
        className="h-8 min-w-48 flex-1 text-sm"
      />
      <Button type="button" size="sm" variant="outline" onClick={() => decide(false)} loading={busy === "decline"} disabled={!!busy}>
        Decline
      </Button>
      <Button type="button" size="sm" onClick={() => decide(true)} loading={busy === "approve"} disabled={!!busy}>
        Approve {pct(a.discount_pct)}
      </Button>
    </div>
  );
}

/** Under a saved proposal past the discount limit: asking for approval, the ask waiting with the agent's read, and for
 * owners and admins the decision. Also the line saying who approved it, once it's covered. */
export function DeskPanel({
  proposal,
  dealId,
  them,
  onChange,
}: {
  proposal: Proposal;
  dealId: string;
  them: string;
  onChange: (p: Proposal | null) => unknown;
}) {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const [reason, setReason] = useState("");
  const [asking, setAsking] = useState(false);
  const a = proposal.approval;
  // An ask still counts only for the discount it was made at. Saving a deeper one needs a new ask.
  const waiting = a?.status === "asked" && a.discount_pct >= proposal.discount_pct ? a : null;

  if (!proposal.held) {
    if (a?.status !== "approved" || a.discount_pct < proposal.discount_pct || proposal.discount_pct === 0) return null;
    return (
      <p className="mt-3 flex items-start gap-1.5 text-xs text-pretty text-brand-800">
        <BadgeCheck className="size-3.5 shrink-0" />
        <span>
          {a.decided_by ?? "An admin"} approved {pct(a.discount_pct)} off {a.decided_at ? timeAgo(a.decided_at) : ""}
          {a.note && <span className="text-muted-foreground">: {a.note}</span>}
        </span>
      </p>
    );
  }

  const ask = async () => {
    setAsking(true);
    try {
      await onChange(await api<Proposal | null>(`/deals/${dealId}/proposal/approval`, { method: "POST", body: { reason } }));
      setReason("");
    } catch (e) {
      toastError(e);
    } finally {
      setAsking(false);
    }
  };
  return (
    <div className="mt-3 flex flex-col gap-3 rounded-lg border border-amber-200 bg-amber-50/60 p-3">
      <p className="flex items-start gap-2 text-sm text-pretty">
        <ShieldAlert className="mt-0.5 size-4 shrink-0 text-amber-700" />
        <span>
          {pct(proposal.discount_pct)} off the price list is past the {proposal.discount_limit_pct}% limit, so {them} sees that it&apos;s being
          finished and can&apos;t accept it until an owner or admin approves it.
        </span>
      </p>
      {waiting ? (
        <>
          <div>
            <MicroLabel>
              Asked by {waiting.asked_by ?? "a teammate"} {timeAgo(waiting.created_at)}
            </MicroLabel>
            {waiting.reason && <p className="mt-0.5 text-sm text-pretty">&ldquo;{waiting.reason}&rdquo;</p>}
          </div>
          <div>
            <MicroLabel>The agent&apos;s read</MicroLabel>
            <div className="mt-1">
              <ApprovalRead a={waiting} />
            </div>
          </div>
          {admin ? <Decide a={waiting} onDecided={onChange} /> : <p className="text-xs text-muted-foreground">Waiting on an owner or admin.</p>}
        </>
      ) : (
        <>
          {a?.status === "declined" && (
            <p className="text-xs text-pretty text-muted-foreground">
              {a.decided_by ?? "An admin"} declined {pct(a.discount_pct)}
              {a.note ? `: ${a.note}` : "."}
            </p>
          )}
          <div className="flex flex-wrap items-end gap-2">
            <Textarea
              value={reason}
              onChange={(e) => setReason(e.target.value)}
              placeholder="Why this discount, and what they give in return"
              maxLength={600}
              rows={2}
              className="min-w-56 flex-1 text-sm"
            />
            <Button type="button" size="sm" onClick={ask} loading={asking} disabled={!reason.trim()}>
              Ask for approval
            </Button>
          </div>
        </>
      )}
    </div>
  );
}

/** Adds a product from the price list as a proposal item, when the deal is in the list's currency. */
export function ProductPicker({ currency, onPick, disabled }: { currency: string; onPick: (item: ProposalItem) => void; disabled?: boolean }) {
  const { data } = useSWR<PriceList>("/products", fetcher);
  if (!data?.products.length || data.currency.toLowerCase() !== currency.toLowerCase()) return null;
  return (
    <Select
      value=""
      disabled={disabled}
      onChange={(e) => {
        const p = data.products.find((x) => x.id === e.target.value);
        if (p) onPick({ name: p.name, detail: p.detail, quantity: 1, unit_price: p.price, period: p.period, product_id: p.id });
      }}
      className="h-8 w-auto text-sm"
      aria-label="Add from the price list"
    >
      <option value="">Add from the price list…</option>
      {data.products.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name}, {dealMoney(p.price, data.currency.toUpperCase())}
          {PERIODS[p.period].suffix}
        </option>
      ))}
    </Select>
  );
}

/** "20% off" next to an item priced under its list price. */
export function OffList({ list, price }: { list: number | undefined; price: number }) {
  const off = list ? ((list - price) / list) * 100 : 0;
  return off >= 0.005 ? <span className="text-xs text-amber-700 tabular-nums">{pct(off)} off</span> : null;
}

/** The list price of each product, for showing unsaved edits' discounts. */
export function useListPrices() {
  const { data } = useSWR<PriceList>("/products", fetcher);
  return (i: ProposalItem) => (i.product_id ? (data?.products.find((p) => p.id === i.product_id)?.price ?? i.list_price) : undefined);
}

const BLANK = { name: "", detail: "", price: 0, period: "month" as Product["period"] };

/** On the Team page: the price list proposals pick from and the discount limit past which they need approval. Owners and
 * admins edit it, everyone else reads it. */
export function PriceListSection({ admin }: { admin: boolean }) {
  const { data, mutate } = useSWR<PriceList>("/products", fetcher);
  const [editing, setEditing] = useState<{ id: string | null; name: string; detail: string; price: number; period: Product["period"] } | null>(null);
  const [saving, setSaving] = useState(false);
  // The limit being typed, saved with its button rather than on every keystroke.
  const [limit, setLimitDraft] = useState<number | null>(null);
  if (!data) return null;
  const money = (n: number) => dealMoney(n, data.currency.toUpperCase());
  const run = async (call: () => Promise<PriceList>) => {
    setSaving(true);
    try {
      await mutate(call(), { revalidate: false });
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setSaving(false);
    }
  };
  const save = async () => {
    if (!editing) return;
    const { id, ...body } = editing;
    if (await run(() => api<PriceList>(id ? `/products/${id}` : "/products", { method: id ? "PUT" : "POST", body }))) setEditing(null);
  };
  const setLimit = (on: boolean, value = data.discount_limit_pct ?? 15) =>
    run(() => api<PriceList>("/deal-desk/limit", { method: "PUT", body: { discount_limit_pct: on ? Math.round(value) : null } }));

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-col gap-3 text-sm">
        <div className="flex items-start justify-between gap-4">
          <span>
            <span className="block font-medium">Approve big discounts</span>
            <span className="block text-pretty text-muted-foreground">
              A discount past the limit needs an owner or admin&apos;s approval before the buyer can accept.
            </span>
          </span>
          <Switch checked={data.discount_limit_pct !== null} disabled={!admin} loading={saving} onChange={(on) => setLimit(on)} label="Approve big discounts" />
        </div>
        {data.discount_limit_pct !== null && (
          <div className="flex flex-wrap items-center gap-2">
            More than
            <NumberInput
              min={0}
              max={100}
              step={1}
              value={limit ?? data.discount_limit_pct}
              disabled={!admin}
              onValueChange={setLimitDraft}
              className="w-20"
              aria-label="Discount limit in percent"
            />
            % off
            {limit !== null && limit !== data.discount_limit_pct && (
              <Button type="button" size="sm" loading={saving} onClick={async () => (await setLimit(true, limit)) && setLimitDraft(null)}>
                Save
              </Button>
            )}
          </div>
        )}
      </div>
      {data.products.length > 0 ? (
        <ul className="divide-y divide-border rounded-xl border border-border text-sm">
          {data.products.map((p) => (
            <li key={p.id} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 px-3 py-2">
              <span className="font-medium">{p.name}</span>
              {p.detail && <span className="min-w-0 truncate text-muted-foreground">{p.detail}</span>}
              <span className="ml-auto tabular-nums">
                {money(p.price)}
                <span className="text-muted-foreground">{PERIODS[p.period].suffix}</span>
              </span>
              {admin && (
                <span className="flex gap-1 self-center">
                  <button
                    type="button"
                    className="text-muted-foreground hover:text-foreground"
                    aria-label={`Edit ${p.name}`}
                    onClick={() => setEditing({ id: p.id, name: p.name, detail: p.detail, price: p.price, period: p.period })}
                  >
                    <Pencil className="size-3.5" />
                  </button>
                  <button
                    type="button"
                    className="text-muted-foreground hover:text-foreground"
                    aria-label={`Remove ${p.name}`}
                    onClick={() => run(() => api<PriceList>(`/products/${p.id}`, { method: "DELETE" }))}
                  >
                    <Trash2 className="size-3.5" />
                  </button>
                </span>
              )}
            </li>
          ))}
        </ul>
      ) : (
        <p className="text-sm text-muted-foreground">No products yet. Proposals can still list any price.</p>
      )}
      {admin &&
        (editing ? (
          <form
            className="flex flex-wrap items-end gap-2 rounded-xl bg-muted/50 p-3 text-sm"
            onSubmit={(e) => {
              e.preventDefault();
              save();
            }}
          >
            <Input
              value={editing.name}
              onChange={(e) => setEditing({ ...editing, name: e.target.value })}
              placeholder="Name, like Seats"
              maxLength={120}
              className="h-8 min-w-36 flex-1"
              required
              autoFocus
            />
            <Input
              value={editing.detail}
              onChange={(e) => setEditing({ ...editing, detail: e.target.value })}
              placeholder="One line on what it is"
              maxLength={300}
              className="h-8 min-w-48 flex-[2]"
            />
            <NumberInput
              min={0}
              step="any"
              value={editing.price}
              onValueChange={(price) => setEditing({ ...editing, price: Math.max(0, price) })}
              className="h-8 w-28"
              aria-label={`Price in ${data.currency.toUpperCase()}`}
            />
            <Select
              value={editing.period}
              onChange={(e) => setEditing({ ...editing, period: e.target.value as Product["period"] })}
              className="h-8 w-auto"
            >
              {Object.entries(PERIODS).map(([value, p]) => (
                <option key={value} value={value}>
                  {p.label}
                </option>
              ))}
            </Select>
            <Button type="submit" size="sm" loading={saving}>
              Save
            </Button>
            <Button type="button" size="sm" variant="ghost" onClick={() => setEditing(null)}>
              Cancel
            </Button>
          </form>
        ) : (
          <div>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => setEditing({ id: null, ...BLANK })}
              disabled={data.products.length >= 100}
            >
              <Plus className="size-3.5" /> Add a product
            </Button>
          </div>
        ))}
    </div>
  );
}
