import { ArrowDown, ArrowUp } from "lucide-react";
import { Card, Skeleton } from "@/components/ui/misc";
import { cn, percent } from "@/lib/utils";
import { MicroLabel } from "./states";

/** How a number moved since the period before. `good` colors it: true green, false red, null for neither, like spend. */
export type Change = { text: string; up: boolean | null; good: boolean | null };

/** `warn` turns the note amber, for a number that needs attention. */
export type Kpi = { label: string; value?: string; title?: string; note?: string; warn?: boolean; change?: Change };

/** Headline numbers in one card. The grid columns come from the caller, since they depend on how many numbers there are. */
export function KpiCard({ kpis, className }: { kpis: Kpi[]; className: string }) {
  return (
    <Card className={cn("grid gap-y-1 py-1.5", className)}>
      {kpis.map((k) => (
        <div key={k.label} className="px-4 py-2" title={k.title}>
          <MicroLabel>{k.label}</MicroLabel>
          {k.value === undefined ? (
            <Skeleton className="mt-2 h-6 w-14" />
          ) : (
            <p className="mt-1 flex flex-wrap items-baseline gap-x-2">
              <span className="text-xl font-semibold tabular-nums">{k.value}</span>
              {k.change && <ChangeText change={k.change} />}
            </p>
          )}
          {k.note && <p className={cn("text-xs tabular-nums", k.warn ? "font-medium text-amber-700" : "text-muted-foreground")}>{k.note}</p>}
        </div>
      ))}
    </Card>
  );
}

function ChangeText({ change }: { change: Change }) {
  const Arrow = change.up === null ? null : change.up ? ArrowUp : ArrowDown;
  const tone = change.good === null ? "text-muted-foreground" : change.good ? "text-brand-700" : "text-red-700";
  return (
    <span className={cn("inline-flex items-center gap-0.5 text-xs font-medium whitespace-nowrap tabular-nums", tone)}>
      {Arrow && <Arrow className="size-3" aria-label={change.up ? "Up" : "Down"} />}
      {change.text}
    </span>
  );
}

/** "4.1% reply rate", or nothing before anyone was emailed. */
export function replyRate(replies: number, contacted: number) {
  return contacted > 0 ? `${percent(replies / contacted)} reply rate` : undefined;
}
