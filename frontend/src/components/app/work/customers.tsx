"use client";

import { HeartHandshake, TrendingUp, UserPlus } from "lucide-react";
import useSWR from "swr";
import { Badge, Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { Customer } from "@/lib/types";
import { dealMoney, plural, timeAgo } from "@/lib/utils";
import { LoadError, MicroLabel } from "./states";
import { Kpi } from "./win-back";

const money = (cents: number, currency: string) => dealMoney(cents / 100, currency.toUpperCase());

/** Sums per currency, the biggest first, like "$12,400 + €900". */
function totals(customers: Customer[], cents: (c: Customer) => number): string {
  const by = new Map<string, number>();
  for (const c of customers) by.set(c.currency, (by.get(c.currency) ?? 0) + cents(c));
  const parts = [...by].filter(([, n]) => n !== 0).sort((a, b) => b[1] - a[1]);
  return parts.length ? parts.map(([currency, n]) => money(n, currency)).join(" + ") : "–";
}

function Row({ c, onOpen }: { c: Customer; onOpen: (dealId: string) => void }) {
  const atRisk = c.risk === "high" || c.risk === "medium";
  const why = atRisk ? c.risk_why : c.growth;
  return (
    <li>
      <button
        type="button"
        onClick={() => onOpen(c.deal_id)}
        className="grid w-full gap-x-4 gap-y-1 px-4 py-3 text-left hover:bg-accent/60 md:grid-cols-[minmax(0,1fr)_7rem_7rem_7rem]"
      >
        <div className="min-w-0">
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
            {c.company_domain && <Favicon domain={c.company_domain} size={14} />}
            <span className="font-medium">{c.company_name || c.lead_name}</span>
            {c.risk === "high" && <Badge tone="red">High risk</Badge>}
            {c.risk === "medium" && <Badge tone="amber">Watch</Badge>}
            {c.growth && (
              <Badge tone="green">
                <TrendingUp className="size-3" /> Ready to grow
              </Badge>
            )}
            {(c.asked || c.referred > 0) && (
              <Badge tone="blue">
                <HeartHandshake className="size-3" /> {c.referred > 0 ? `${plural(c.referred, "lead")} referred` : "Asked for intros"}
              </Badge>
            )}
          </p>
          <p className="truncate text-xs text-muted-foreground">{[c.company_name && c.lead_name, c.owner_name].filter(Boolean).join(" · ")}</p>
          {why && <p className="mt-0.5 line-clamp-2 text-xs text-pretty text-muted-foreground">{why}</p>}
        </div>
        <Cell label="Deal" value={c.value_cents > 0 ? money(c.value_cents, c.currency) : "–"} />
        <Cell label="Paid, 12 months" value={c.revenue_cents ? money(c.revenue_cents, c.currency) : "–"} />
        <Cell label="Last contact" value={c.last_contact_at ? timeAgo(c.last_contact_at) : "Never"} />
      </button>
    </li>
  );
}

/** A number with its label beside it on phones and above it in the columns' header otherwise. */
function Cell({ label, value }: { label: string; value: string }) {
  return (
    <p className="flex justify-between gap-2 text-sm tabular-nums md:block md:text-right">
      <span className="text-xs text-muted-foreground md:hidden">{label}</span>
      {value}
    </p>
  );
}

/** The Customers page's overview: every customer with what they paid, when you last talked, and what the agent found. */
export function CustomerList({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<Customer[]>(`/projects/${projectId}/customers`, fetcher);
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="your customers" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;
  if (!data.length) {
    return (
      <Card>
        <EmptyState icon={<HeartHandshake className="size-5" />} title="No customers yet">
          Win a deal on the Pipeline page, or connect Stripe, Paddle or Lemon Squeezy so a payment wins it, and the customer shows up here with what
          they paid and what the agent finds about them.
        </EmptyState>
      </Card>
    );
  }

  const risky = data.filter((c) => c.risk === "high" || c.risk === "medium").length;
  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        <Kpi label="Customers" value={data.length} hint={`${totals(data, (c) => c.value_cents)} in deals`} />
        <Card className="p-4">
          <MicroLabel>Paid, 12 months</MicroLabel>
          <p className="mt-1 truncate text-2xl font-semibold tabular-nums">{totals(data, (c) => c.revenue_cents)}</p>
          <p className="text-xs text-muted-foreground">from Stripe, Paddle or Lemon Squeezy</p>
        </Card>
        <Kpi label="At risk" value={risky} hint="high risk or on watch" />
        <Kpi label="Ready to grow" value={data.filter((c) => c.growth).length} hint="expansion emails waiting" />
      </div>
      <Card className="overflow-hidden p-0">
        <div className="hidden gap-x-4 border-b border-border px-4 py-2 font-mono text-[11px] tracking-wide text-muted-foreground uppercase md:grid md:grid-cols-[minmax(0,1fr)_7rem_7rem_7rem]">
          <span>Customer</span>
          <span className="text-right">Deal</span>
          <span className="text-right">Paid, 12 months</span>
          <span className="text-right">Last contact</span>
        </div>
        <ul className="divide-y divide-border">
          {data.map((c) => (
            <Row key={c.deal_id} c={c} onOpen={onOpen} />
          ))}
        </ul>
      </Card>
      {data.some((c) => c.referred > 0) && (
        <p className="flex items-center gap-1.5 text-xs text-muted-foreground">
          <UserPlus className="size-3.5" />{" "}
          {plural(
            data.reduce((n, c) => n + c.referred, 0),
            "lead",
          )}{" "}
          came from your customers&apos; referral pages.
        </p>
      )}
    </div>
  );
}
