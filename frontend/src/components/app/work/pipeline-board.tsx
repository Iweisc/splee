"use client";

import { useEffect, useRef, useState } from "react";
import { Clock, CreditCard, GripVertical } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select } from "@/components/ui/form";
import { Card, Favicon, Modal } from "@/components/ui/misc";
import { type Deal, type DealStage, PAYMENT_NAMES } from "@/lib/types";
import { cn, plural } from "@/lib/utils";
import { dealMoney, moneyList, sumByCurrency } from "./deal-dialog";
import { OwnerMenu } from "./owner-menu";

/** What a move carries besides the stage: the final value of a won deal, the reason for a lost one. */
export type MoveExtras = { value?: number; lost_reason?: string };

export type MoveRequest = (deal: Deal, stage: DealStage) => void;

/** Gives the deal's lead to someone on the team, or nobody. */
export type AssignRequest = (deal: Deal, ownerId: string | null) => void;

const DAY = 86_400_000;

function daysIn(deal: Deal) {
  const days = Math.floor((Date.now() - new Date(deal.stage_changed_at).getTime()) / DAY);
  return days < 1 ? "Today" : plural(days, "day");
}

function dot(stage: DealStage) {
  return stage.kind === "won" ? "bg-brand-500" : stage.kind === "lost" ? "bg-red-500" : "bg-foreground/25";
}

function StageHeading({ stage, deals, currency }: { stage: DealStage; deals: Deal[]; currency: string }) {
  return (
    <div className="flex items-baseline justify-between gap-3">
      <p className="flex min-w-0 items-center gap-2 text-sm font-medium">
        <span className={cn("size-2 shrink-0 rounded-full", dot(stage))} aria-hidden />
        <span className="truncate">{stage.name}</span>
        <span className="font-normal text-muted-foreground tabular-nums">{deals.length}</span>
      </p>
      <p className="shrink-0 text-xs text-muted-foreground tabular-nums">{moneyList(sumByCurrency(deals), currency)}</p>
    </div>
  );
}

/** The company, the person and the deal's numbers. `action` is the card's handle: the drag grip or a stage picker. */
function DealCardBody({ deal, onOpen, onAssign, action }: { deal: Deal; onOpen: () => void; onAssign: AssignRequest; action?: React.ReactNode }) {
  return (
    <>
      <div className="flex items-start gap-2">
        {deal.company_domain ? <Favicon domain={deal.company_domain} size={16} className="mt-0.5" /> : <span className="size-4 shrink-0" />}
        <div className="min-w-0 flex-1">
          {/* The keyboard's way into the deal. A mouse can click anywhere on the card. */}
          <button type="button" onClick={onOpen} className="block max-w-full truncate text-left text-sm font-medium hover:underline">
            {deal.company_name || deal.lead_name || "Unnamed lead"}
          </button>
          <p className="truncate text-xs text-muted-foreground">{[deal.lead_name, deal.lead_title].filter(Boolean).join(", ")}</p>
        </div>
        {action}
      </div>
      <div className="mt-2.5 flex h-5 items-center gap-2 text-xs">
        <span className="font-medium tabular-nums">
          {deal.value > 0 ? dealMoney(deal.value, deal.currency) : <span className="text-muted-foreground">No value yet</span>}
        </span>
        {deal.paid_through && (
          <span className="flex min-w-0 items-center gap-1 text-muted-foreground" title={`Won when they paid through ${PAYMENT_NAMES[deal.paid_through]}`}>
            <CreditCard className="size-3 shrink-0" /> <span className="truncate">{PAYMENT_NAMES[deal.paid_through]}</span>
          </span>
        )}
        <span className="ml-auto flex items-center gap-1 text-muted-foreground" title="Time in this stage">
          <Clock className="size-3" /> {daysIn(deal)}
        </span>
        <OwnerMenu look="avatar" ownerId={deal.owner_id} ownerName={deal.owner_name} onPick={(id) => onAssign(deal, id)} />
      </div>
    </>
  );
}

/**
 * One column per stage. A card moves by dragging, or from the keyboard: Space or Enter on its grip picks it up, the arrow
 * keys pick a column and Space or Enter drops it there. Escape puts it back.
 */
export function PipelineBoard({
  stages,
  deals,
  currency,
  onMove,
  onOpen,
  onAssign,
}: {
  stages: DealStage[];
  deals: Deal[];
  currency: string;
  onMove: MoveRequest;
  onOpen: (deal: Deal) => void;
  onAssign: AssignRequest;
}) {
  const [dragging, setDragging] = useState<string | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const [held, setHeld] = useState<{ deal: Deal; at: number } | null>(null);
  const [said, setSaid] = useState("");
  // A dropped card renders again in its new column, which drops keyboard focus. This hands it back to the card's grip.
  const refocus = useRef<string | null>(null);
  useEffect(() => {
    if (!refocus.current) return;
    document.querySelector<HTMLElement>(`[data-grip="${refocus.current}"]`)?.focus();
    refocus.current = null;
  });

  const drop = (deal: Deal, stage: DealStage) => {
    setDragging(null);
    setOver(null);
    if (stage.id !== deal.stage_id) onMove(deal, stage);
  };

  const gripKeys = (e: React.KeyboardEvent, deal: Deal) => {
    const name = deal.company_name || deal.lead_name;
    if (!held || held.deal.id !== deal.id) {
      if (e.key !== " " && e.key !== "Enter") return;
      e.preventDefault();
      const at = stages.findIndex((s) => s.id === deal.stage_id);
      setHeld({ deal, at });
      setSaid(`Picked up ${name} in ${stages[at]?.name}. Use the arrow keys to pick a stage, then Space to drop it, or Escape to cancel.`);
      return;
    }
    const step = { ArrowRight: 1, ArrowDown: 1, ArrowLeft: -1, ArrowUp: -1 }[e.key];
    if (step) {
      e.preventDefault();
      const at = Math.min(stages.length - 1, Math.max(0, held.at + step));
      setHeld({ deal, at });
      setSaid(stages[at]!.name);
    } else if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      const stage = stages[held.at]!;
      setHeld(null);
      refocus.current = deal.id;
      setSaid(stage.id === deal.stage_id ? `${name} stays in ${stage.name}` : `Moved ${name} to ${stage.name}`);
      drop(deal, stage);
    } else if (e.key === "Escape") {
      // The page has no other use for Escape here, but a modal around it would.
      e.preventDefault();
      setHeld(null);
      setSaid(`${name} stays in ${stages.find((s) => s.id === deal.stage_id)?.name}`);
    }
  };

  return (
    <div className="-mx-4 flex min-h-0 flex-1 gap-3 overflow-x-auto px-4 pb-2 sm:-mx-8 sm:px-8">
      <p id="deal-drag-help" className="sr-only">
        Press Space or Enter to pick up the deal, the arrow keys to choose a stage, and Space or Enter again to drop it. Escape cancels.
      </p>
      <p aria-live="assertive" className="sr-only">
        {said}
      </p>
      {stages.map((stage, i) => {
        const inStage = deals.filter((d) => d.stage_id === stage.id);
        const target = over === stage.id || held?.at === i;
        return (
          <section
            key={stage.id}
            aria-label={stage.name}
            onDragOver={(e) => {
              if (!dragging) return;
              e.preventDefault();
              e.dataTransfer.dropEffect = "move";
              setOver(stage.id);
            }}
            onDragLeave={(e) => !e.currentTarget.contains(e.relatedTarget as Node | null) && setOver(null)}
            onDrop={(e) => {
              e.preventDefault();
              const deal = deals.find((d) => d.id === e.dataTransfer.getData("text/plain"));
              if (deal) drop(deal, stage);
            }}
            className={cn(
              "flex w-64 shrink-0 flex-col rounded-2xl bg-black/[0.035] transition-shadow",
              target && "bg-brand-50/70 ring-2 ring-brand-600/60",
            )}
          >
            <div className="px-3 pt-3 pb-2">
              <StageHeading stage={stage} deals={inStage} currency={currency} />
              {stage.kind === "open" && <p className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">{stage.probability}% likely to close</p>}
              {stage.kind !== "open" && <p className="mt-0.5 text-[11px] text-muted-foreground">Closed in the last 90 days</p>}
            </div>
            <ol className="flex min-h-24 flex-1 flex-col gap-2 overflow-y-auto px-2 pb-2">
              {inStage.map((deal) => {
                const lifted = held?.deal.id === deal.id;
                return (
                  <li
                    key={deal.id}
                    draggable
                    onDragStart={(e) => {
                      e.dataTransfer.setData("text/plain", deal.id);
                      e.dataTransfer.effectAllowed = "move";
                      setDragging(deal.id);
                    }}
                    onDragEnd={() => {
                      setDragging(null);
                      setOver(null);
                    }}
                    onClick={(e) => !(e.target as Element).closest("button") && onOpen(deal)}
                    className={cn(
                      "cursor-grab rounded-xl bg-card p-3 shadow-card transition-shadow hover:shadow-pop active:cursor-grabbing",
                      dragging === deal.id && "opacity-40",
                      lifted && "shadow-pop ring-2 ring-brand-600",
                    )}
                  >
                    <DealCardBody
                      deal={deal}
                      onOpen={() => onOpen(deal)}
                      onAssign={onAssign}
                      action={
                        <button
                          type="button"
                          data-grip={deal.id}
                          aria-label={`Move ${deal.company_name || deal.lead_name}`}
                          aria-describedby="deal-drag-help"
                          aria-pressed={lifted}
                          onKeyDown={(e) => gripKeys(e, deal)}
                          onBlur={() => lifted && setHeld(null)}
                          className="-m-1 shrink-0 cursor-grab rounded-md p-1 text-subtle-foreground hover:bg-accent hover:text-foreground"
                        >
                          <GripVertical className="size-4" />
                        </button>
                      }
                    />
                  </li>
                );
              })}
              {inStage.length === 0 && (
                <li
                  className={cn(
                    "flex flex-1 items-center justify-center rounded-xl border border-dashed border-border-strong/60 px-3 py-6",
                    "text-center text-xs text-muted-foreground",
                  )}
                >
                  No deals
                </li>
              )}
            </ol>
          </section>
        );
      })}
    </div>
  );
}

/** For phones, where dragging doesn't work: the deals grouped by stage, each with a stage picker instead of a grip. */
export function PipelineList({
  stages,
  deals,
  currency,
  onMove,
  onOpen,
  onAssign,
}: {
  stages: DealStage[];
  deals: Deal[];
  currency: string;
  onMove: MoveRequest;
  onOpen: (deal: Deal) => void;
  onAssign: AssignRequest;
}) {
  return (
    <div className="flex flex-col gap-5">
      {stages.map((stage) => {
        const inStage = deals.filter((d) => d.stage_id === stage.id);
        return (
          <section key={stage.id} aria-label={stage.name}>
            <div className="mb-2 px-1">
              <StageHeading stage={stage} deals={inStage} currency={currency} />
            </div>
            {inStage.length === 0 ? (
              <p className="rounded-xl border border-dashed border-border-strong/60 px-3 py-4 text-center text-xs text-muted-foreground">No deals</p>
            ) : (
              <ol className="grid gap-2 sm:grid-cols-2 xl:grid-cols-3">
                {inStage.map((deal) => (
                  <li key={deal.id}>
                    <Card className="rounded-xl p-3">
                      <DealCardBody deal={deal} onOpen={() => onOpen(deal)} onAssign={onAssign} />
                      <Select
                        value={deal.stage_id}
                        aria-label={`Stage of ${deal.company_name || deal.lead_name}`}
                        onChange={(e) => onMove(deal, stages.find((s) => s.id === e.target.value)!)}
                        className="mt-3 h-9"
                      >
                        {stages.map((s) => (
                          <option key={s.id} value={s.id}>
                            {s.name}
                          </option>
                        ))}
                      </Select>
                    </Card>
                  </li>
                ))}
              </ol>
            )}
          </section>
        );
      })}
    </div>
  );
}

/** Asks for the final value of a won deal, or why it was lost, before closing it. */
export function CloseDialog({
  deal,
  stage,
  onCancel,
  onConfirm,
}: {
  deal: Deal;
  stage: DealStage;
  onCancel: () => void;
  onConfirm: (extras: MoveExtras) => Promise<void>;
}) {
  const won = stage.kind === "won";
  const [value, setValue] = useState(deal.value);
  const [reason, setReason] = useState("");
  const [saving, setSaving] = useState(false);
  const name = deal.company_name || deal.lead_name;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      await onConfirm(won ? { value } : { lost_reason: reason.trim() });
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal open onClose={onCancel} title={won ? `Mark ${name} as won` : `Mark ${name} as lost`} className="max-w-md">
      <form onSubmit={submit} className="flex flex-col gap-4">
        {won ? (
          <Field label={`Final value in ${deal.currency}`} hint="What they agreed to pay. It counts toward won this month.">
            <NumberInput min={0} step="any" value={value} onValueChange={(v) => setValue(Math.max(0, v))} autoFocus />
          </Field>
        ) : (
          <Field label="Why was it lost?" hint="Your team sees this on the deal. Leave it blank if you don't know.">
            <Input value={reason} maxLength={500} onChange={(e) => setReason(e.target.value)} placeholder="Went with a competitor" autoFocus />
          </Field>
        )}
        <div className="flex justify-end gap-2">
          <Button type="button" variant="ghost" onClick={onCancel}>
            Cancel
          </Button>
          <Button type="submit" variant={won ? "brand" : "primary"} loading={saving}>
            {won ? "Mark as won" : "Mark as lost"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
