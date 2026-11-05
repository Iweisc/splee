"use client";

import Link from "next/link";
import useSWR from "swr";
import { MicroLabel, ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { EMAIL_COST_MICROS } from "@/components/app/settings/pricing";
import { Input } from "@/components/ui/form";
import { Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { BillingSummary } from "@/lib/types";
import { cn, money, num } from "@/lib/utils";

const MIN_USD = 1;
const MAX_USD = 200;

export default function BudgetPage() {
  const { data: billing } = useSWR<BillingSummary>("/billing", fetcher);

  return (
    <ProjectSettingsPage
      title="Budget"
      description={`How much this project may spend on sending per day. You pay ${money(EMAIL_COST_MICROS)} per sent email.`}
      problem={(s) =>
        s.daily_budget_cents < MIN_USD * 100 || s.daily_budget_cents > MAX_USD * 100 ? `Budget must be between $${MIN_USD} and $${MAX_USD}` : null
      }
    >
      {(s, update) => {
        const usd = s.daily_budget_cents / 100;
        return (
          <>
            <Section
              title="Daily budget"
              description="The agent stops sending for the day once this project's spend reaches the budget, or as soon as your balance hits $0."
            >
              <div className="flex items-center gap-4">
                <input
                  type="range"
                  min={MIN_USD}
                  max={MAX_USD}
                  step={1}
                  value={Math.min(Math.max(usd, MIN_USD), MAX_USD)}
                  onChange={(e) => update({ daily_budget_cents: Number(e.target.value) * 100 })}
                  className="h-2 flex-1 cursor-pointer accent-black"
                  aria-label="Daily budget"
                />
                <div className="relative w-32">
                  <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
                  <Input
                    type="number"
                    min={MIN_USD}
                    max={MAX_USD}
                    step={1}
                    value={usd}
                    onChange={(e) => update({ daily_budget_cents: Math.round(Number(e.target.value) * 100) })}
                    className="pr-12 pl-6 tabular-nums"
                    aria-label="Daily budget in dollars"
                  />
                  <span className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm text-muted-foreground">/day</span>
                </div>
              </div>
              <div className="mt-2 flex justify-between font-mono text-[11px] text-muted-foreground">
                <span>${MIN_USD}</span>
                <span>${MAX_USD}</span>
              </div>
              <div className="mt-5 grid gap-3 sm:grid-cols-2">
                <Stat label="Emails per day" value={`up to ${num(Math.floor((s.daily_budget_cents * 10_000) / EMAIL_COST_MICROS))}`} />
                <Stat label="Max per 30 days" value={money(s.daily_budget_cents * 10_000 * 30)} />
              </div>
            </Section>

            <Section
              title="Today"
              description="The balance is shared by every project in your organization."
              actions={
                billing?.payments_enabled && (
                  <Link href="/app/billing" className="text-sm font-medium hover:underline">
                    Add funds
                  </Link>
                )
              }
            >
              <div className="grid gap-3 sm:grid-cols-2">
                <Stat label="Spent today, all projects" value={billing ? money(billing.spent_today_micros) : null} />
                <Stat
                  label="Balance"
                  value={billing ? (billing.payments_enabled ? money(billing.balance_micros) : "Not enforced") : null}
                  danger={!!billing?.payments_enabled && billing.balance_micros <= 0}
                />
              </div>
              {billing?.payments_enabled && billing.balance_micros <= 0 && (
                <p className="mt-3 text-sm text-danger">Your balance is $0, so nothing is being sent. Add funds to resume.</p>
              )}
            </Section>
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}

function Stat({ label, value, danger }: { label: string; value: string | null; danger?: boolean }) {
  return (
    <div className="rounded-xl bg-muted px-4 py-3">
      <MicroLabel>{label}</MicroLabel>
      {value === null ? (
        <Skeleton className="mt-1.5 h-6 w-20" />
      ) : (
        <p className={cn("mt-1 text-lg font-semibold tabular-nums", danger && "text-danger")}>{value}</p>
      )}
    </div>
  );
}
