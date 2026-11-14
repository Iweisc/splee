"use client";

import { useState } from "react";
import Link from "next/link";
import { HandCoins, MessageSquare, Plus, Trash2 } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Textarea } from "@/components/ui/form";
import { Badge, type BadgeTone, Modal } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Deal, DealAmounts, DealStage, Member, Pipeline, StageKind } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ClosePlanSection } from "./close-plan";
import { ProposalSection } from "./proposal";
import { QuietDeal } from "./quiet-deal";
import { toastError } from "./states";

/** The codes the currency pickers offer. A deal priced in another one through the API keeps it and shows it too. */
export const CURRENCIES = [
  ...["USD", "EUR", "GBP", "CAD", "AUD", "NZD", "CHF", "SEK", "NOK", "DKK"],
  ...["PLN", "CZK", "JPY", "SGD", "HKD", "INR", "BRL", "MXN", "ZAR", "AED"],
];

/** 12000 in USD as "$12,000", and 99.5 as "$99.50". */
/** Sums in more than one currency can't be added up, so they read "$12,000 + €3,000". `fallback` names the currency of a zero. */
export function moneyList(amounts: { currency: string; value: number }[], fallback: string): string {
  const shown = amounts.filter((a) => a.value !== 0);
  return shown.length ? shown.map((a) => dealMoney(a.value, a.currency)).join(" + ") : dealMoney(0, fallback);
}

export function sumByCurrency(deals: Deal[]): { currency: string; value: number }[] {
  const sums = new Map<string, number>();
  for (const d of deals) sums.set(d.currency, (sums.get(d.currency) ?? 0) + d.value);
  return [...sums].map(([currency, value]) => ({ currency, value }));
}

export function amountsOf(rows: DealAmounts[], key: "open" | "weighted" | "won_this_month") {
  return rows.map((r) => ({ currency: r.currency, value: r[key] }));
}

const KIND_TONE: Record<StageKind, BadgeTone> = { open: "neutral", won: "green", lost: "red" };

export function DealStageBadge({ deal }: { deal: Pick<Deal, "stage" | "status"> }) {
  return <Badge tone={KIND_TONE[deal.status]}>{deal.stage}</Badge>;
}

/** The deal under a lead's name: its stage and value, linking to it on the pipeline, or a button that starts one. */
export function LeadDeal({ leadId, deal, onChange, className }: { leadId: string; deal: Deal | null; onChange: () => unknown; className?: string }) {
  const [creating, setCreating] = useState(false);
  if (deal && deal.status === "open") {
    return (
      <Link href={`/app/pipeline?deal=${deal.id}`} className={`inline-flex items-center gap-1.5 hover:underline ${className ?? ""}`}>
        <HandCoins className="size-3.5 shrink-0 text-muted-foreground" />
        <span>
          Deal in {deal.stage}
          {deal.value > 0 && <span className="tabular-nums"> · {dealMoney(deal.value, deal.currency)}</span>}
        </span>
      </Link>
    );
  }
  return (
    <>
      <span className={`inline-flex flex-wrap items-center gap-x-1.5 gap-y-1 ${className ?? ""}`}>
        {deal && (
          <Link href={`/app/pipeline?deal=${deal.id}`} className="inline-flex items-center gap-1.5 hover:underline">
            <HandCoins className="size-3.5 shrink-0 text-muted-foreground" />
            {deal.status === "won" ? "Deal won" : "Deal lost"}
          </Link>
        )}
        <button type="button" onClick={() => setCreating(true)} className="inline-flex items-center gap-1 font-medium hover:underline">
          {!deal && <HandCoins className="size-3.5 shrink-0 text-muted-foreground" />}
          {deal ? "Start a new deal" : "Create deal"}
        </button>
      </span>
      {creating && (
        <DealDialog
          leadId={leadId}
          onClose={() => setCreating(false)}
          onSaved={() => {
            setCreating(false);
            onChange();
          }}
        />
      )}
    </>
  );
}

/**
 * Creates a deal for `leadId`, or edits `deal`. Moving between stages happens on the board, so the stage is only picked
 * when creating. Every pipeline view refreshes after a save.
 */
export function DealDialog({ deal, leadId, onClose, onSaved }: { deal?: Deal; leadId?: string; onClose: () => void; onSaved: (deal: Deal) => void }) {
  const { mutate } = useSWRConfig();
  const { data: pipeline } = useSWR<Pipeline>("/pipeline", fetcher);
  const { data: members } = useSWR<Member[]>("/organization/users", fetcher);
  const currency = deal?.currency ?? pipeline?.currency ?? "USD";
  const [values, setValues] = useState({
    value: deal?.value ?? 0,
    currency: deal ? deal.currency : "",
    close_date: deal?.close_date ?? "",
    owner_id: deal?.owner_id ?? "",
    notes: deal?.notes ?? "",
    lost_reason: deal?.lost_reason ?? "",
    stage_id: "",
  });
  const [saving, setSaving] = useState(false);
  const [deleting, setDeleting] = useState<"ask" | "busy" | null>(null);
  const set = (changes: Partial<typeof values>) => setValues((v) => ({ ...v, ...changes }));
  const picked = values.currency || currency;
  const currencies = CURRENCIES.includes(picked) ? CURRENCIES : [picked, ...CURRENCIES];
  const open: DealStage[] = pipeline?.stages.filter((s) => s.kind === "open") ?? [];
  // Where hot replies start, like the server's default.
  const stageId = values.stage_id || (open.find((s) => s.auto === "hot") ?? open[0])?.id || "";

  const refresh = () => mutate((k) => typeof k === "string" && (k.startsWith("/pipeline") || k.startsWith("/leads/") || k.startsWith("/inbox")));

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    const body = {
      value: values.value,
      currency: picked,
      close_date: values.close_date || null,
      owner_id: values.owner_id || null,
      notes: values.notes,
      ...(deal ? { lost_reason: values.lost_reason } : { lead_id: leadId, stage_id: stageId || undefined }),
    };
    try {
      const saved = deal ? await api<Deal>(`/deals/${deal.id}`, { method: "PATCH", body }) : await api<Deal>("/deals", { body });
      await refresh();
      toast.success(deal ? "Deal saved" : "Deal created");
      onSaved(saved);
    } catch (err) {
      toastError(err);
      setSaving(false);
    }
  };

  const remove = async () => {
    if (!deal) return;
    setDeleting("busy");
    try {
      await api(`/deals/${deal.id}`, { method: "DELETE" });
      await refresh();
      toast.success("Deal deleted");
      onClose();
    } catch (err) {
      toastError(err);
      setDeleting("ask");
    }
  };

  return (
    <Modal open onClose={onClose} title={deal ? (deal.company_name || deal.lead_name) : "Create deal"} className={deal ? "max-w-xl" : undefined}>
      {deal && deleting ? (
        <div className="flex flex-col gap-5">
          <p className="text-sm text-muted-foreground">
            The deal with {deal.lead_name || deal.company_name} comes off the pipeline and out of the forecast. The lead stays. You can&apos;t undo this.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setDeleting(null)} autoFocus>
              Cancel
            </Button>
            <Button variant="danger" onClick={remove} loading={deleting === "busy"}>
              <Trash2 className="size-4" /> Delete deal
            </Button>
          </div>
        </div>
      ) : (
        <form onSubmit={submit} className="flex flex-col gap-4">
          {deal && (
            <div className="-mt-2 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-sm text-muted-foreground">
              <DealStageBadge deal={deal} />
              <span>{[deal.lead_name, deal.lead_title].filter(Boolean).join(", ")}</span>
              <Link href={`/app/p/${deal.project_id}/inbox?lead=${deal.lead_id}`} className="inline-flex items-center gap-1 text-foreground hover:underline">
                <MessageSquare className="size-3.5" /> Open the conversation
              </Link>
            </div>
          )}
          {deal && <QuietDeal deal={deal} />}
          {deal && <ClosePlanSection deal={deal} />}
          {deal && <ProposalSection deal={deal} />}
          <div className="grid gap-4 sm:grid-cols-[minmax(0,1fr)_8rem]">
            <Field label={deal?.status === "won" ? "Final value" : "Value"}>
              <NumberInput min={0} step="any" value={values.value} onValueChange={(value) => set({ value: Math.max(0, value) })} autoFocus />
            </Field>
            <Field label="Currency">
              <Select value={picked} onChange={(e) => set({ currency: e.target.value })}>
                {currencies.map((c) => (
                  <option key={c}>{c}</option>
                ))}
              </Select>
            </Field>
          </div>
          {!deal && (
            <Field label="Stage">
              <Select value={stageId} onChange={(e) => set({ stage_id: e.target.value })}>
                {open.map((s) => (
                  <option key={s.id} value={s.id}>
                    {s.name}
                  </option>
                ))}
              </Select>
            </Field>
          )}
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Expected close">
              <Input type="date" value={values.close_date} onChange={(e) => set({ close_date: e.target.value })} />
            </Field>
            <Field label="Owner">
              <Select value={values.owner_id} onChange={(e) => set({ owner_id: e.target.value })}>
                <option value="">Unassigned</option>
                {members?.map((m) => (
                  <option key={m.user_id} value={m.user_id}>
                    {m.name || m.email}
                  </option>
                ))}
              </Select>
            </Field>
          </div>
          {deal?.status === "lost" && (
            <Field label="Why it was lost">
              <Input value={values.lost_reason} maxLength={500} onChange={(e) => set({ lost_reason: e.target.value })} />
            </Field>
          )}
          <Field label="Notes">
            <Textarea
              value={values.notes}
              onChange={(e) => set({ notes: e.target.value })}
              placeholder="What they need, who signs, what's next"
              className="min-h-20"
            />
          </Field>
          <div className="flex flex-wrap items-center justify-end gap-2">
            {deal && (
              <Button type="button" variant="ghost" className="mr-auto text-danger" onClick={() => setDeleting("ask")}>
                <Trash2 className="size-4" /> Delete
              </Button>
            )}
            <Button type="button" variant="ghost" onClick={onClose}>
              Cancel
            </Button>
            {deal ? (
              <Button type="submit" loading={saving}>
                Save
              </Button>
            ) : (
              <Button type="submit" loading={saving}>
                <Plus className="size-4" /> Create deal
              </Button>
            )}
          </div>
        </form>
      )}
    </Modal>
  );
}
