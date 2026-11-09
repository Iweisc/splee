import { Card, Skeleton } from "@/components/ui/misc";
import { cn, percent } from "@/lib/utils";
import { MicroLabel } from "./states";

/** `warn` turns the note amber, for a number that needs attention. */
export type Kpi = { label: string; value?: string; title?: string; note?: string; warn?: boolean };

/** Headline numbers in one card. The grid columns come from the caller, since they depend on how many numbers there are. */
export function KpiCard({ kpis, className }: { kpis: Kpi[]; className: string }) {
  return (
    <Card className={cn("grid gap-y-1 py-1.5", className)}>
      {kpis.map((k) => (
        <div key={k.label} className="px-4 py-2" title={k.title}>
          <MicroLabel>{k.label}</MicroLabel>
          {k.value === undefined ? <Skeleton className="mt-2 h-6 w-14" /> : <p className="mt-1 text-xl font-semibold tabular-nums">{k.value}</p>}
          {k.note && <p className={cn("text-xs tabular-nums", k.warn ? "font-medium text-amber-700" : "text-muted-foreground")}>{k.note}</p>}
        </div>
      ))}
    </Card>
  );
}

/** "4.1% reply rate", or nothing before anyone was emailed. */
export function replyRate(replies: number, contacted: number) {
  return contacted > 0 ? `${percent(replies / contacted)} reply rate` : undefined;
}
