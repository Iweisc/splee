"use client";

import { useState } from "react";
import { Check, ExternalLink, ListChecks, Loader2, PenLine, Plus, RefreshCw, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { CopyButton } from "@/components/app/settings/copy-button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ClosePlan, ClosePlanStep, Deal } from "@/lib/types";
import { cn, timeAgo } from "@/lib/utils";
import { toastError } from "./states";

const dueLabel = (date: string) => new Date(`${date}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" });
const overdue = (step: ClosePlanStep) => !step.done_at && !!step.due_date && new Date(`${step.due_date}T23:59:59`).getTime() < Date.now();

/** On the pipeline card: how far the close plan got, or the onboarding plan once it's won. */
export function PlanProgress({ deal }: { deal: Deal }) {
  if (!deal.plan || deal.status === "lost") return null;
  return (
    <p className="mt-2 flex items-center gap-1 text-xs text-muted-foreground">
      <ListChecks className="size-3 shrink-0" /> {deal.status === "won" ? "Onboarding" : "Plan"} {deal.plan.done} of {deal.plan.total} done
    </p>
  );
}

/**
 * In the deal's window: the steps both sides take to a signed deal, shared with the buyer. Once it's won, the same plan
 * and link carry on as the onboarding plan, which the handoff writes. See backend/src/close_plans.rs.
 */
export function ClosePlanSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/plan`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data: plan, mutate } = useSWR<ClosePlan | null>(key, fetcher, {
    refreshInterval: (p) => (p?.status === "writing" ? 2500 : 0),
  });
  const [busy, setBusy] = useState<string | null>(null);
  const [adding, setAdding] = useState({ title: "", side: "them" as "us" | "them", due_date: "" });
  const them = deal.company_name || deal.lead_name || "Them";

  const act = async (what: string, call: () => Promise<ClosePlan | null>) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
      mutateAll((k) => typeof k === "string" && k.startsWith("/pipeline"));
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => api<ClosePlan | null>(key, { method: "POST" }));
  const change = (step: ClosePlanStep, body: Record<string, unknown>) =>
    act(step.id, () => api<ClosePlan | null>(`/plan-steps/${step.id}`, { method: "PATCH", body }));
  const remove = (step: ClosePlanStep) => act(step.id, () => api<ClosePlan | null>(`/plan-steps/${step.id}`, { method: "DELETE" }));
  const add = async () => {
    if (!adding.title.trim()) return;
    const body = { title: adding.title, side: adding.side, due_date: adding.due_date || null };
    if (await act("add", () => api<ClosePlan | null>(`${key}/steps`, { body }))) setAdding((a) => ({ ...a, title: "" }));
  };

  const onboarding = deal.status === "won";
  if (plan === undefined || deal.status === "lost" || (onboarding && !plan?.onboarding_goal && plan?.status !== "writing")) return null;
  const writing = plan?.status === "writing";
  const goal = onboarding ? plan?.onboarding_goal : plan?.goal;
  const steps = plan?.steps.filter((s) => s.phase === (onboarding ? "onboarding" : "close")) ?? [];
  const title = onboarding ? "Onboarding plan" : "Close plan";
  if (!plan || (!goal && !steps.length)) {
    return (
      // The button wraps under the text on a phone rather than squeezing it into a narrow column.
      <section className="flex flex-wrap items-start justify-between gap-x-3 gap-y-2 rounded-xl border border-dashed border-border-strong p-4">
        <div className="min-w-0 flex-1 basis-56">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <ListChecks className="size-3.5" /> {title}
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? onboarding
                ? `The agent is writing the steps to get ${them} going, on the plan they already have…`
                : "The agent is reading the deal and writing the steps to a signed deal…"
              : plan?.status === "failed"
                ? "Couldn't write the plan. Try again."
                : `The steps to a signed deal, ${them}'s and yours, with dates. Share it with them and they tick off their own.`}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
            <PenLine className="size-3.5" /> Write a close plan
          </Button>
        )}
      </section>
    );
  }

  const done = steps.filter((s) => s.done_at).length;
  return (
    <section className="@container rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <ListChecks className="size-3.5" /> {title}
        </p>
        <span className="text-xs text-muted-foreground tabular-nums">
          {done} of {steps.length} done
        </span>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-1">
          {writing ? (
            <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Writing…
            </span>
          ) : (
            // Once it's won the handoff writes the onboarding steps, and writes them again from its own section.
            !onboarding && (
              <Button type="button" size="sm" variant="ghost" onClick={write} loading={busy === "write"} title="Keeps the steps already done">
                <RefreshCw className="size-3.5" /> Write again
              </Button>
            )
          )}
          <a
            href={`${plan.url}?preview=1`}
            target="_blank"
            rel="noreferrer"
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-sm font-medium hover:bg-accent"
          >
            <ExternalLink className="size-3.5" /> Open
          </a>
          <CopyButton value={plan.url} label="Copy link" />
        </div>
      </div>
      {goal && <p className="mt-2 text-sm text-pretty">{goal}</p>}
      <p className="mt-1 text-xs text-muted-foreground">
        {plan.last_viewed_at
          ? `${deal.lead_name || them} last looked ${timeAgo(plan.last_viewed_at)}, ${plan.views === 1 ? "once" : `${plan.views} times`}.`
          : `Not opened yet. Send ${deal.lead_name || them} the link and they can tick off their steps.`}
      </p>
      <div className="mt-3 h-1 overflow-hidden rounded-full bg-black/5">
        <div className="h-full rounded-full bg-brand-500 transition-all" style={{ width: `${steps.length ? (done / steps.length) * 100 : 0}%` }} />
      </div>

      <ol className="mt-3 flex flex-col">
        {steps.map((step) => (
          <li key={step.id} className="group flex items-start gap-2.5 border-b border-border py-2 last:border-0">
            <button
              type="button"
              onClick={() => change(step, { done: !step.done_at })}
              disabled={busy === step.id}
              aria-pressed={!!step.done_at}
              aria-label={step.done_at ? `Mark "${step.title}" not done` : `Mark "${step.title}" done`}
              className={cn(
                "mt-0.5 flex size-4 shrink-0 items-center justify-center rounded-full border transition-colors",
                step.done_at ? "border-brand-600 bg-brand-600 text-white" : "border-border-strong hover:border-foreground",
              )}
            >
              {step.done_at && <Check className="size-3" strokeWidth={3} />}
            </button>
            <div className="min-w-0 flex-1">
              <p className={cn("text-sm", step.done_at && "text-muted-foreground line-through")}>{step.title}</p>
              {step.detail && !step.done_at && <p className="text-xs text-pretty text-muted-foreground">{step.detail}</p>}
              {step.done_by === "them" && <p className="text-xs text-brand-700">Ticked off by {them}</p>}
            </div>
            {/* On a phone the date goes under whose step it is and a long company name is cut short, or the title gets a column
                too narrow for its words. */}
            <div className="flex shrink-0 flex-col items-end gap-1 @sm:flex-row @sm:items-start @sm:gap-2.5">
              <span
                title={step.side === "them" ? them : undefined}
                className={cn(
                  "max-w-28 truncate rounded-full px-1.5 text-[11px] leading-[18px] font-medium @sm:max-w-none",
                  step.side === "them" ? "bg-sky-50 text-sky-700" : "bg-black/5 text-foreground",
                )}
              >
                {step.side === "them" ? them : "You"}
              </span>
              <span className={cn("w-12 text-right text-xs tabular-nums", overdue(step) ? "text-danger" : "text-muted-foreground")}>
                {step.due_date ? dueLabel(step.due_date) : "–"}
              </span>
            </div>
            <button
              type="button"
              onClick={() => remove(step)}
              className="shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100 hover:text-foreground focus-visible:opacity-100"
              aria-label={`Remove "${step.title}"`}
            >
              <X className="size-3.5" />
            </button>
          </li>
        ))}
      </ol>

      <div className="mt-2 flex flex-wrap gap-2">
        <Input
          value={adding.title}
          onChange={(e) => setAdding((a) => ({ ...a, title: e.target.value }))}
          // The deal's window is a form, so Enter would save the deal instead.
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void add();
            }
          }}
          placeholder="Add a step"
          maxLength={120}
          className="h-8 min-w-40 flex-1 text-sm"
        />
        <Select
          value={adding.side}
          onChange={(e) => setAdding((a) => ({ ...a, side: e.target.value as "us" | "them" }))}
          className="h-8 w-auto text-sm"
          aria-label="Whose step"
        >
          <option value="them">{them}</option>
          <option value="us">You</option>
        </Select>
        <Input
          type="date"
          value={adding.due_date}
          onChange={(e) => setAdding((a) => ({ ...a, due_date: e.target.value }))}
          className="h-8 w-auto text-sm"
          aria-label="Due"
        />
        <Button type="button" size="sm" variant="outline" onClick={add} loading={busy === "add"} disabled={!adding.title.trim()}>
          <Plus className="size-3.5" /> Add
        </Button>
      </div>
    </section>
  );
}
