"use client";

import { useEffect, useState } from "react";
import { RotateCcw, UserPlus } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Field, NumberInput } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { qs } from "@/lib/api";
import type { Capacity, CapacityAssumptions } from "@/lib/types";
import { cn, plural } from "@/lib/utils";

type Key = keyof CapacityAssumptions;

const monthShort = (d: string) => new Date(`${d.slice(0, 7)}-01T00:00:00Z`).toLocaleString("en-US", { month: "short", timeZone: "UTC" });
const monthLong = (d: string) =>
  new Date(`${d.slice(0, 7)}-01T00:00:00Z`).toLocaleString("en-US", { month: "long", year: "numeric", timeZone: "UTC" });
const dayName = (d: string) =>
  new Date(`${d}T00:00:00Z`).toLocaleString("en-US", { month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });
const today = () => new Date().toISOString().slice(0, 10);

function list(xs: string[]) {
  return xs.length <= 2 ? xs.join(" and ") : `${xs.slice(0, -1).join(", ")} and ${xs.at(-1)}`;
}

/** Hires that start together, as one row each. */
function waves(hires: Capacity["plan"]["hires"]) {
  const out: { hire: Capacity["plan"]["hires"][number]; count: number }[] = [];
  for (const h of hires) {
    const same = out.find((w) => w.hire.start_by === h.start_by);
    if (same) same.count += 1;
    else out.push({ hire: h, count: 1 });
  }
  return out;
}

/** Each assumption as the inputs show it: percents as percents, money in the currency's main unit. */
const FIELDS: { key: Key; label: string; unit: "pct" | "money" | "number"; step: number }[] = [
  { key: "win_rate", label: "Win rate (%)", unit: "pct", step: 1 },
  { key: "deal_cents", label: "Average deal", unit: "money", step: 100 },
  { key: "cycle_days", label: "Sales cycle (days)", unit: "number", step: 1 },
  { key: "deals_per_rep", label: "New deals a rep opens a month", unit: "number", step: 0.5 },
  { key: "ramp_months", label: "Months to full speed", unit: "number", step: 1 },
  { key: "target_cents", label: "Team target a month", unit: "money", step: 1000 },
  { key: "growth_pct", label: "Target growth a month (%)", unit: "number", step: 1 },
];

const shown = (key: Key, unit: string, v: number) =>
  unit === "pct" ? Math.round(v * 1000) / 10 : unit === "money" ? Math.round(v / 100) : key === "deals_per_rep" ? Math.round(v * 10) / 10 : v;
const stored = (unit: string, v: number) => (unit === "pct" ? v / 100 : unit === "money" ? Math.round(v * 100) : v);

function source(key: Key, c: Capacity) {
  if (key === "growth_pct") return "Flat unless you grow it";
  if (key === "target_cents") return c.history.target_cents !== null ? "Your teammates' targets" : "Set targets above";
  if (key === "deal_cents" && c.history.deal_cents === null) return "Needed to plan";
  if (key === "ramp_months" && c.history.ramp_months !== null) return "Your reps' first wins, less the cycle";
  return c.history[key] !== null ? "From your last year" : "A starting guess";
}

/** On the Targets tab, for owners and admins: whether the team books its target over the next year, and when each rep it
 * still needs has to start. Every assumption can be changed to see what happens. */
export function CapacityCard() {
  const [changes, setChanges] = useState<Partial<CapacityAssumptions>>({});
  const [query, setQuery] = useState(changes);
  useEffect(() => {
    const t = setTimeout(() => setQuery(changes), 300);
    return () => clearTimeout(t);
  }, [changes]);
  const { data: c } = useSWR<Capacity>(`/capacity${qs(query)}`, fetcher, { keepPreviousData: true });
  if (!c) return null;

  const money = (cents: number) =>
    (cents / 100).toLocaleString("en-US", { style: "currency", currency: c.currency.toUpperCase(), maximumFractionDigits: 0 });
  const { plan, used } = c;
  const changed = Object.keys(changes).length > 0;

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-start gap-3 px-4 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-64">
          <p className="text-sm font-semibold">Capacity plan</p>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            <Summary c={c} money={money} />
          </p>
        </div>
        {changed && (
          <Button type="button" size="sm" variant="ghost" onClick={() => setChanges({})}>
            <RotateCcw className="size-3.5" /> Back to your numbers
          </Button>
        )}
      </div>
      <div className="grid gap-x-3 gap-y-3 border-t border-border px-4 py-4 sm:grid-cols-2 lg:grid-cols-4">
        {FIELDS.map((f) => (
          <Field key={f.key} label={f.label} hint={changes[f.key] !== undefined ? "Changed" : source(f.key, c)}>
            <NumberInput
              min={0}
              step={f.step}
              value={shown(f.key, f.unit, used[f.key])}
              onValueChange={(v) => setChanges({ ...changes, [f.key]: stored(f.unit, Math.max(0, v)) })}
              className={cn(changes[f.key] !== undefined && "border-sky-600/50 bg-sky-50/40")}
            />
          </Field>
        ))}
      </div>
      {used.deal_cents > 0 && used.target_cents > 0 && (
        <div className="border-t border-border px-4 py-4">
          <Chart c={c} money={money} />
          {plan.hires.length > 0 && (
            <ol className="mt-4 flex flex-col divide-y divide-border rounded-lg border border-border">
              {waves(plan.hires).map(({ hire: h, count }) => (
                <li key={h.start_by} className="flex flex-wrap items-center gap-x-4 gap-y-1 px-3 py-2 text-sm">
                  <span className="flex items-center gap-1.5 font-medium">
                    <UserPlus className="size-3.5 text-sky-700" /> {plural(count, "rep")}
                  </span>
                  <span>{h.start_by <= today() ? "Start now" : `Start by ${dayName(h.start_by)}`}</span>
                  <span className="text-muted-foreground">
                    {count === 1 ? "closes" : "close"} deals from {monthLong(h.closing_from)}, at full speed by {monthLong(h.full_speed)}
                  </span>
                </li>
              ))}
            </ol>
          )}
          <p className="mt-3 text-xs text-pretty text-muted-foreground">
            A rep at full speed books {money(plan.per_rep_cents)} a month: {Math.round(used.deals_per_rep * 10) / 10} new deals,{" "}
            {Math.round(used.win_rate * 100)}% won at {money(used.deal_cents)}, closing {plural(used.cycle_days, "day")} after they open. A new rep
            opens none in their first month and climbs to full speed over {plural(used.ramp_months, "month")}. Counts {plural(c.reps, "rep")}:
            teammates with a target or a deal opened in the last six months.
          </p>
        </div>
      )}
    </Card>
  );
}

function Summary({ c, money }: { c: Capacity; money: (cents: number) => string }) {
  const { plan, used } = c;
  if (used.deal_cents <= 0)
    return <>Splee needs an average deal to plan, and you haven&apos;t won one in the last year. Enter what you expect below.</>;
  if (used.target_cents <= 0) return <>Set your teammates&apos; targets above, or a team target below, to plan against.</>;
  if (plan.per_rep_cents <= 0) return <>At these numbers a rep books nothing, so no hire would help. Check the win rate and the deals a rep opens.</>;
  const short = plan.short.map(monthShort);
  const tooSoon = short.length > 0 ? ` ${list(short)} ${short.length === 1 ? "falls" : "fall"} short whatever you do now.` : "";
  if (plan.hires.length === 0) {
    return short.length === 0 ? (
      <>
        The {plural(c.reps, "rep")} you have book {money(plan.months[0].target_cents)} a month or more for the next year. No hires needed.
      </>
    ) : (
      <>
        No hires needed for the year, but {list(short)} {short.length === 1 ? "falls" : "fall"} short while the team ramps up.
      </>
    );
  }
  const first = plan.hires[0].start_by;
  return (
    <>
      Hire {plural(plan.hires.length, "more rep")} to keep up with {money(plan.months.at(-1)!.target_cents)} a month by{" "}
      {monthLong(plan.months.at(-1)!.month)}. The first has to start {first <= today() ? "now" : `by ${dayName(first)}`}.{tooSoon}
    </>
  );
}

const H = 150;
const W = 40;

/** The team's bookings and the hires' on top, one bar a month, under each month's target. */
function Chart({ c, money }: { c: Capacity; money: (cents: number) => string }) {
  const [hover, setHover] = useState<number | null>(null);
  const months = c.plan.months;
  const max = Math.max(1, ...months.map((m) => Math.max(m.target_cents, m.team_cents + m.hires_cents))) * 1.08;
  const y = (cents: number) => H - (cents / max) * H;
  const at = months[hover ?? months.length - 1];
  const hires = months.some((m) => m.hires_cents > 0);
  const legend = [
    { label: "Your team", value: at.team_cents, swatch: <span className="size-2 rounded-sm bg-brand-600" /> },
    ...(hires ? [{ label: "New hires", value: at.hires_cents, swatch: <span className="size-2 rounded-sm bg-sky-600" /> }] : []),
    { label: "Target", value: at.target_cents, swatch: <span className="h-0.5 w-3 rounded-full bg-foreground" /> },
  ];

  return (
    <div>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3 text-xs">
        <div className="flex flex-wrap gap-x-4 gap-y-1">
          {legend.map((l) => (
            <span key={l.label} className="inline-flex items-center gap-1.5 text-muted-foreground">
              {l.swatch}
              {l.label}
              <span className="font-medium text-foreground tabular-nums">{money(l.value)}</span>
            </span>
          ))}
        </div>
        <span className="font-mono text-muted-foreground">{monthLong(at.month)}</span>
      </div>
      <svg
        viewBox={`0 0 ${months.length * W} ${H}`}
        preserveAspectRatio="none"
        className="h-40 w-full"
        onMouseLeave={() => setHover(null)}
        role="img"
        aria-label="Bookings from your team and new hires against the target, month by month"
      >
        {months.map((m, i) => {
          const team = H - y(m.team_cents);
          const hired = H - y(m.hires_cents);
          return (
            <g key={m.month} onMouseEnter={() => setHover(i)}>
              <rect x={i * W} y={0} width={W} height={H} className={hover === i ? "fill-black/[0.03]" : "fill-transparent"} />
              {team > 0 && <rect x={i * W + 8} y={H - team} width={W - 16} height={team} rx={2} className="fill-brand-600" />}
              {hired > 2 && <rect x={i * W + 8} y={H - team - hired} width={W - 16} height={hired - 2} rx={2} className="fill-sky-600" />}
              <line
                x1={i * W + 3}
                x2={(i + 1) * W - 3}
                y1={y(m.target_cents)}
                y2={y(m.target_cents)}
                strokeWidth={2}
                vectorEffect="non-scaling-stroke"
                className="stroke-foreground"
              />
            </g>
          );
        })}
      </svg>
      <div
        className="mt-1.5 grid font-mono text-[11px] text-muted-foreground"
        style={{ gridTemplateColumns: `repeat(${months.length}, minmax(0, 1fr))` }}
      >
        {months.map((m) => (
          <span key={m.month} className={cn("text-center", c.plan.short.includes(m.month) && "text-amber-700")}>
            {monthShort(m.month)}
          </span>
        ))}
      </div>
      <p className="sr-only">
        {months
          .map((m) => `${monthLong(m.month)}: team ${money(m.team_cents)}, hires ${money(m.hires_cents)}, target ${money(m.target_cents)}.`)
          .join(" ")}
      </p>
    </div>
  );
}
