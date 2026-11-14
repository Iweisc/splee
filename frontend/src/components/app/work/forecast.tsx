"use client";

import { useState } from "react";
import { AlertTriangle, Loader2, RefreshCw, Target, TrendingUp } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, ForecastCategory, ForecastView } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { moneyList, sumByCurrency } from "./deal-dialog";
import { type Kpi, KpiCard } from "./kpi-card";
import { MicroLabel, toastError } from "./states";

const COLUMNS: { category: ForecastCategory; label: string; dot: string }[] = [
  { category: "commit", label: "Commit", dot: "bg-brand-500" },
  { category: "best_case", label: "Best case", dot: "bg-amber-400" },
  { category: "later", label: "Later", dot: "bg-border" },
];

const shortDate = (d: string) => new Date(`${d}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" });

/** The pipeline's Forecast view: the agent's sort of the project's open deals for the month, and what to work on. See backend/src/agent/forecast.rs. */
export function ForecastPanel({
  projectId,
  deals,
  currency,
  onOpen,
}: {
  projectId: string;
  deals: Deal[];
  currency: string;
  onOpen: (d: Deal) => void;
}) {
  const key = `/projects/${projectId}/forecast`;
  const { data, mutate } = useSWR<ForecastView>(key, fetcher, { refreshInterval: (v) => (v?.latest?.status === "writing" ? 3000 : 0) });
  const [busy, setBusy] = useState(false);
  const open = deals.filter((d) => d.status === "open" && d.project_id === projectId);

  const write = async () => {
    setBusy(true);
    try {
      await mutate(api<ForecastView>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  if (!data) return null;
  const latest = data.latest;
  const writing = latest?.status === "writing";
  const writeButton = (
    <Button type="button" size="sm" variant="outline" onClick={write} loading={busy} disabled={writing || !open.length}>
      {writing ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />} {latest ? "Write a new one" : "Write a forecast"}
    </Button>
  );
  const f = latest?.status === "ready" ? latest.forecast : null;
  if (!f) {
    return (
      <Card>
        <EmptyState
          icon={writing ? <Loader2 className="size-5 animate-spin" /> : <TrendingUp className="size-5" />}
          title={writing ? "The agent is reviewing your open deals…" : latest ? "The last forecast couldn't be written" : "No forecast yet"}
          action={!writing && writeButton}
        >
          {!writing &&
            (open.length
              ? "Every Monday morning the agent sorts your open deals into what will close this month, what could and what won't, and picks the deals to work on this week."
              : "Once the project has open deals, the agent forecasts the month every Monday morning.")}
        </EmptyState>
      </Card>
    );
  }

  const byId = new Map(deals.map((d) => [d.id, d]));
  // Deals closed or deleted since the forecast drop out of its totals.
  const placed = f.deals.flatMap((p) => {
    const deal = byId.get(p.deal_id);
    return deal && deal.status === "open" ? [{ ...p, deal }] : [];
  });
  const kpis: Kpi[] = COLUMNS.map((c) => {
    const list = placed.filter((p) => p.category === c.category);
    return {
      label: c.category === "later" ? "Later" : `${c.label} by ${shortDate(f.month_end)}`,
      value: moneyList(sumByCurrency(list.map((p) => p.deal)), currency),
      note: plural(list.length, "deal"),
    };
  });
  const picks = (list: { deal_id: string; text: string }[]) =>
    list.flatMap((p) => (byId.get(p.deal_id) ? [{ ...p, deal: byId.get(p.deal_id)! }] : []));

  return (
    <div className="flex flex-col gap-4">
      <Card className="flex items-start gap-3 p-4">
        <TrendingUp className="mt-0.5 size-4 shrink-0 text-brand-700" />
        <div className="min-w-0 flex-1">
          <p className="text-sm text-pretty">{f.headline}</p>
          <p className="mt-1 text-xs text-muted-foreground">The agent&apos;s forecast {timeAgo(latest!.created_at)}</p>
        </div>
        {writeButton}
      </Card>
      <KpiCard kpis={kpis} className="shrink-0 sm:grid-cols-3" />
      <div className="grid gap-4 lg:grid-cols-2">
        <PickList
          icon={<Target className="size-3.5" />}
          title="Focus this week"
          items={picks(f.focus)}
          empty="Nothing stands out this week."
          onOpen={onOpen}
        />
        <PickList
          icon={<AlertTriangle className="size-3.5" />}
          title="Slipping"
          items={picks(f.slipping)}
          empty="No deal is slipping."
          tone="text-amber-700"
          onOpen={onOpen}
        />
      </div>
      <div className="grid gap-4 lg:grid-cols-3">
        {COLUMNS.map((c) => {
          const list = placed.filter((p) => p.category === c.category);
          return (
            <Card key={c.category} className="p-4">
              <MicroLabel className="mb-2 flex items-center gap-1.5">
                <span className={cn("size-1.5 rounded-full", c.dot)} /> {c.label}
              </MicroLabel>
              {list.length === 0 ? (
                <p className="py-2 text-sm text-muted-foreground">None.</p>
              ) : (
                <ul className="flex flex-col gap-1">
                  {list.map((p) => (
                    <li key={p.deal_id}>
                      <button type="button" onClick={() => onOpen(p.deal)} className="w-full rounded-lg px-2 py-1.5 text-left hover:bg-muted">
                        <p className="flex items-baseline justify-between gap-3 text-sm">
                          <span className="truncate font-medium">{p.deal.company_name || p.deal.lead_name}</span>
                          <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{moneyList(sumByCurrency([p.deal]), currency)}</span>
                        </p>
                        {p.why && <p className="text-xs text-pretty text-muted-foreground">{p.why}</p>}
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </Card>
          );
        })}
      </div>
    </div>
  );
}

function PickList({
  icon,
  title,
  items,
  empty,
  tone,
  onOpen,
}: {
  icon: React.ReactNode;
  title: string;
  items: { deal_id: string; text: string; deal: Deal }[];
  empty: string;
  tone?: string;
  onOpen: (d: Deal) => void;
}) {
  return (
    <Card className="p-4">
      <MicroLabel className={cn("mb-2 flex items-center gap-1.5", tone)}>
        {icon} {title}
      </MicroLabel>
      {items.length === 0 ? (
        <p className="py-2 text-sm text-muted-foreground">{empty}</p>
      ) : (
        <ul className="flex flex-col divide-y divide-border">
          {items.map((p) => (
            <li key={p.deal_id}>
              <button type="button" onClick={() => onOpen(p.deal)} className="w-full py-2.5 text-left text-sm">
                <span className="font-medium">{p.deal.company_name || p.deal.lead_name}</span>
                <span className="text-muted-foreground"> · </span>
                <span className="text-pretty">{p.text}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
