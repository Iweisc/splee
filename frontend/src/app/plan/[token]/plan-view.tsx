"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { ArrowRight, CalendarDays, Check, CircleAlert, Mail } from "lucide-react";
import { buttonClass } from "@/components/ui/button";
import { Favicon, Spinner } from "@/components/ui/misc";
import { LogoMark } from "@/components/ui/logo";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { ClosePlanStep, PublicClosePlan } from "@/lib/types";
import { cn } from "@/lib/utils";

const dueLabel = (date: string) => new Date(`${date}T00:00:00`).toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" });

/** The buyer's side of a close plan: both teams' steps to a signed deal, with their own to tick off. */
export function PlanView({ token, preview }: { token: string; preview: boolean }) {
  const path = `/plans/${encodeURIComponent(token)}`;
  const { data: plan, error, mutate } = useSWR<PublicClosePlan>(path, fetcher, { revalidateOnFocus: false });
  const [busy, setBusy] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  // Reported from the browser, once per load, so link checkers that never run scripts don't count as the buyer.
  const reported = useRef(false);
  useEffect(() => {
    if (!plan || reported.current || preview) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [plan, path, preview]);

  if (error instanceof ApiError && error.status === 404) {
    return (
      <Frame>
        <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This plan isn&apos;t here anymore</h1>
          <p className="text-sm text-muted-foreground">Reply to the email it came in and we&apos;ll send the latest.</p>
        </div>
      </Frame>
    );
  }
  if (!plan) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }

  const tick = async (step: ClosePlanStep) => {
    setBusy(step.id);
    setFailed(false);
    try {
      await mutate(api<PublicClosePlan>(`${path}/steps/${step.id}`, { body: { done: !step.done_at } }), { revalidate: false });
    } catch {
      setFailed(true);
    } finally {
      setBusy(null);
    }
  };

  const sender = plan.sender_name.trim();
  const them = plan.company.trim() || plan.first_name || "You";
  const done = plan.steps.filter((s) => s.done_at).length;
  const next = plan.steps.find((s) => !s.done_at);
  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {plan.from_domain && <Favicon domain={plan.from_domain} size={20} />}
          {plan.from_name}
        </span>
        <span aria-hidden>×</span>
        <span className="flex items-center gap-2 font-medium text-foreground">
          {plan.company_domain && <Favicon domain={plan.company_domain} size={20} />}
          {them}
        </span>
      </header>

      <p className="mt-8 font-mono text-xs tracking-wide text-muted-foreground uppercase">Our plan</p>
      <h1 className="mt-2 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">{plan.goal}</h1>

      <div className="mt-6 flex items-center gap-3">
        <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-black/5">
          <div
            className="h-full rounded-full bg-brand-500 transition-all duration-500"
            style={{ width: `${plan.steps.length ? (done / plan.steps.length) * 100 : 0}%` }}
          />
        </div>
        <span className="text-sm text-muted-foreground tabular-nums">
          {done} of {plan.steps.length} done
        </span>
      </div>

      <ol className="mt-8 flex flex-col">
        {plan.steps.map((step, i) => {
          const theirs = step.side === "them";
          const isNext = step === next;
          return (
            <li key={step.id} className="relative flex gap-4 pb-6 last:pb-0">
              {/* The line down the timeline, from this step's mark to the next one's. */}
              {i < plan.steps.length - 1 && <span aria-hidden className="absolute top-8 bottom-0 left-[15px] w-px bg-border-strong" />}
              {theirs ? (
                <button
                  type="button"
                  onClick={() => tick(step)}
                  disabled={busy === step.id}
                  aria-pressed={!!step.done_at}
                  aria-label={step.done_at ? `Mark "${step.title}" not done` : `Mark "${step.title}" done`}
                  className={cn(
                    "relative z-10 flex size-8 shrink-0 items-center justify-center rounded-full border-2 transition-colors",
                    step.done_at ? "border-brand-600 bg-brand-600 text-white" : "border-foreground bg-card hover:bg-brand-50",
                  )}
                >
                  {busy === step.id ? <Spinner className="size-3.5" /> : step.done_at && <Check className="size-4" strokeWidth={3} />}
                </button>
              ) : (
                <span
                  className={cn(
                    "relative z-10 flex size-8 shrink-0 items-center justify-center rounded-full border-2",
                    step.done_at ? "border-foreground/70 bg-foreground/70 text-white" : "border-dashed border-border-strong bg-card",
                  )}
                >
                  {step.done_at && <Check className="size-4" strokeWidth={3} />}
                </span>
              )}
              <div className={cn("min-w-0 flex-1 rounded-2xl border bg-card p-4", isNext ? "border-foreground/20 shadow-card" : "border-border")}>
                <div className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-1">
                  <h2 className={cn("font-semibold text-balance", step.done_at && "text-muted-foreground line-through decoration-1")}>
                    {step.title}
                  </h2>
                  {step.due_date && <span className="text-sm text-muted-foreground tabular-nums">{dueLabel(step.due_date)}</span>}
                </div>
                {step.detail && <p className="mt-1 text-sm leading-relaxed text-pretty text-muted-foreground">{step.detail}</p>}
                <p className="mt-2 text-xs font-medium">
                  <span className={theirs ? "text-sky-700" : "text-muted-foreground"}>{theirs ? them : plan.from_name}</span>
                  {theirs && !step.done_at && <span className="font-normal text-muted-foreground"> · tick it off when it&apos;s done</span>}
                  {isNext && <span className="font-normal text-muted-foreground"> · next up</span>}
                </p>
              </div>
            </li>
          );
        })}
      </ol>
      {failed && <p className="mt-4 text-sm text-danger">That didn&apos;t save. Try again in a minute.</p>}

      <section className="mt-12 flex flex-col items-start gap-4 rounded-2xl bg-primary p-6 text-primary-foreground sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <p className="font-semibold text-balance">Need to change something in the plan?</p>
          <p className="mt-1 text-sm text-pretty opacity-75">
            {plan.booking_url
              ? `Pick a time${sender ? ` and ${sender} will go through it with you` : ""}.`
              : `Reply to the email${sender ? ` and ${sender} will update it` : ""}.`}
          </p>
        </div>
        {plan.booking_url ? (
          <a href={plan.booking_url} target="_blank" rel="noreferrer" className={buttonClass("brand", "lg", "shrink-0")}>
            <CalendarDays className="size-4" /> Book a call <ArrowRight className="size-4" />
          </a>
        ) : (
          plan.reply_to && (
            <a href={`mailto:${plan.reply_to}`} className={buttonClass("brand", "lg", "shrink-0")}>
              <Mail className="size-4" /> Write back
            </a>
          )
        )}
      </section>

      <p className="mt-6 text-xs text-pretty text-muted-foreground">
        A plan between {plan.first_name || "you"}
        {plan.company ? ` at ${plan.company}` : ""} and {sender ? `${sender} at ` : ""}
        {plan.from_name}. Ticking a step lets them know.
      </p>
    </Frame>
  );
}

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <main className="flex flex-1 flex-col items-center bg-muted px-4 py-10 sm:px-6 sm:py-16">
      <article className="w-full max-w-2xl">{children}</article>
      <p className="mt-12 flex items-center gap-1.5 text-xs text-muted-foreground">
        <LogoMark className="size-3.5" /> Made with Splee
      </p>
    </main>
  );
}
