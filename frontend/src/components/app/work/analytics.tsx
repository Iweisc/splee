"use client";

import { useState } from "react";
import { ArrowDown, ArrowUp, Download } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import { downloadCsv, toCsv } from "@/lib/csv";
import { cn, num, percent } from "@/lib/utils";
import { MicroLabel, Td, Th } from "./states";

type Cell = string | number | null;

/**
 * Tighter on phones, so three columns fit 390px. The first column's names wrap when they don't fit, because a
 * long campaign name on one line pushed the last column off the card at every width.
 */
const CELL = "px-3 sm:px-4";

export type Column<T> = {
  label: string;
  /** What the column sorts by and exports. */
  value: (row: T) => Cell;
  /** What the table shows, when it isn't the value as is. */
  render?: (row: T) => React.ReactNode;
  numeric?: boolean;
  /** For columns that only fit on wider screens. */
  className?: string;
};

/** Nulls sort last whichever way the column goes, so "no rate yet" never tops a list. */
function compare(a: Cell, b: Cell, desc: boolean) {
  if (a === null || b === null) return a === b ? 0 : a === null ? 1 : -1;
  const order = typeof a === "number" && typeof b === "number" ? a - b : String(a).localeCompare(String(b));
  return desc ? -order : order;
}

/** A breakdown card: a table sorted by clicking a column, and its rows as a CSV file. */
export function Breakdown<T>({
  title,
  hint,
  hintClass,
  rows,
  columns,
  sortBy = 1,
  file,
  rowKey,
  rowClass,
}: {
  title: string;
  hint?: React.ReactNode;
  /** For a hint about columns that only wider screens show. */
  hintClass?: string;
  rows: T[];
  columns: Column<T>[];
  /** The column to start sorted by, biggest first. */
  sortBy?: number;
  file: string;
  rowKey: (row: T) => string;
  rowClass?: (row: T) => string | undefined;
}) {
  const [sort, setSort] = useState({ col: sortBy, desc: sortBy > 0 });
  const col = columns[sort.col]!;
  const sorted = [...rows].sort((a, b) => compare(col.value(a), col.value(b), sort.desc));
  const exportCsv = () => downloadCsv(file, toCsv(columns.map((c) => c.label), sorted.map((r) => columns.map((c) => c.value(r)))));

  return (
    <Card className="overflow-hidden">
      <div className="flex items-center justify-between gap-3 px-5 pt-4 pb-2">
        <MicroLabel>{title}</MicroLabel>
        <Button variant="ghost" size="sm" onClick={exportCsv} disabled={rows.length === 0} className="print:hidden">
          <Download className="size-3.5" /> CSV
        </Button>
      </div>
      {hint && <p className={cn("px-5 pb-2 text-xs text-muted-foreground", hintClass)}>{hint}</p>}
      <div className="overflow-x-auto">
        <table className="w-full text-sm">
          <thead>
            <tr>
              {columns.map((c, i) => {
                const active = sort.col === i;
                const Arrow = sort.desc ? ArrowDown : ArrowUp;
                return (
                  <Th
                    key={c.label}
                    className={cn(CELL, "whitespace-nowrap", c.numeric && "text-right", c.className)}
                    aria-sort={active ? (sort.desc ? "descending" : "ascending") : undefined}
                  >
                    <button
                      onClick={() => setSort({ col: i, desc: active ? !sort.desc : c.numeric === true })}
                      className={cn("inline-flex items-center gap-1 uppercase hover:text-foreground", active && "text-foreground")}
                    >
                      {c.label}
                      {active && <Arrow className="size-3" />}
                    </button>
                  </Th>
                );
              })}
            </tr>
          </thead>
          <tbody>
            {sorted.length === 0 && (
              <tr className="border-t border-border">
                <Td colSpan={columns.length} className="text-muted-foreground">
                  Nothing in these dates.
                </Td>
              </tr>
            )}
            {sorted.map((r) => (
              <tr key={rowKey(r)} className={cn("border-t border-border", rowClass?.(r))}>
                {columns.map((c, i) => (
                  <Td key={c.label} className={cn(CELL, c.numeric && "text-right tabular-nums", i === 0 && "font-medium", c.className)}>
                    {c.render ? c.render(r) : formatCell(c.value(r))}
                  </Td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

function formatCell(v: Cell) {
  return v === null ? "—" : typeof v === "number" ? num(v) : v;
}

/** A rate as text, "—" before there's anything to divide by. */
export function rateText(rate: number | null) {
  return rate === null ? "—" : percent(rate);
}

/** Exports keep rates as numbers, so a spreadsheet can sort and chart them. */
export function rateCell(rate: number | null): Cell {
  return rate === null ? null : Math.round(rate * 1000) / 10;
}

/** A step's reply rate with a bar, scaled to the best step so the drop-off between them stands out. */
export function RateBar({ rate, best }: { rate: number | null; best: number }) {
  return (
    <span className="inline-flex items-center justify-end gap-2">
      <span className="hidden h-1.5 w-20 overflow-hidden rounded-full bg-black/[0.06] sm:block">
        <span className="block h-full rounded-full bg-sky-500" style={{ width: `${best > 0 && rate ? (rate / best) * 100 : 0}%` }} />
      </span>
      <span className="w-12 tabular-nums">{rateText(rate)}</span>
    </span>
  );
}

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
/** Under this many emails or calls, one reply or pickup swings a slot's rate too far to call it the best time. Fewer calls are
 *  made than emails sent, and more of them connect. */
const MIN_SENT = { email: 30, call: 10 };

function hourText(h: number) {
  return `${h % 12 || 12}${h < 12 ? "am" : "pm"}`;
}

/** What each heatmap counts, and its words. */
const HEATMAPS = {
  email: {
    title: "Reply rate by send time",
    unit: "emails",
    did: "replied",
    none: "Nothing sent",
    zero: "No replies",
    scale: "Higher reply rate",
  },
  call: {
    title: "Connect rate by call time",
    unit: "calls",
    did: "picked up",
    none: "No calls",
    zero: "Nobody picked up",
    scale: "Higher connect rate",
  },
} as const;

/**
 * Reply rate by the weekday and hour the email went out, or the share of calls someone picked up by when they were made. A
 * slot's shade is its rate against the best slot's, so the pattern shows even when every rate is small. Slots with nothing
 * stay blank, which is different from a rate of zero.
 */
export function Heatmap({
  kind,
  cells,
  where,
}: {
  kind: keyof typeof HEATMAPS;
  /** `total` emails or calls, `hits` of them replied to or picked up. */
  cells: { weekday: number; hour: number; total: number; hits: number }[];
  /** The time zone the times are in, as a sentence. */
  where: string;
}) {
  const words = HEATMAPS[kind];
  const at = new Map(cells.map((c) => [`${c.weekday}-${c.hour}`, c]));
  const rated = cells.filter((c) => c.total > 0);
  const top = Math.max(0, ...rated.map((c) => c.hits / c.total));
  const best = rated.filter((c) => c.total >= MIN_SENT[kind] && c.hits > 0).sort((a, b) => b.hits / b.total - a.hits / a.total)[0];

  return (
    <Card className="p-5">
      <MicroLabel>{words.title}</MicroLabel>
      <p className="mt-1 mb-4 text-xs text-muted-foreground">
        {best
          ? `Best: ${DAYS[best.weekday - 1]} ${hourText(best.hour)}–${hourText((best.hour + 1) % 24)}, ${percent(best.hits / best.total)} of ${num(best.total)} ${words.unit}. `
          : ""}
        {where}
      </p>
      <div className="grid grid-cols-[2.25rem_repeat(24,minmax(0,1fr))] gap-0.5" role="img" aria-label={`${words.title}, by weekday and hour`}>
        <span />
        {Array.from({ length: 24 }, (_, h) => (
          // Left-aligned and allowed to run over the next empty labels, since a column is narrower than "12am" on a phone.
          <span key={h} className="font-mono text-[10px] whitespace-nowrap text-muted-foreground">
            {h % 6 === 0 ? hourText(h) : ""}
          </span>
        ))}
        {DAYS.map((day, d) => (
          <Row key={day} day={day}>
            {Array.from({ length: 24 }, (_, h) => {
              const c = at.get(`${d + 1}-${h}`);
              const rate = c && c.total > 0 ? c.hits / c.total : null;
              const shade = rate && top > 0 ? 15 + Math.round((rate / top) * 85) : 0;
              return (
                <span
                  key={h}
                  title={
                    c
                      ? `${day} ${hourText(h)}: ${num(c.total)} ${words.unit}, ${num(c.hits)} ${words.did} (${rateText(rate)})`
                      : `${day} ${hourText(h)}: ${words.none.toLowerCase()}`
                  }
                  className={cn("aspect-square rounded-[3px]", !c ? "bg-black/[0.025]" : !rate && "bg-black/[0.08]")}
                  style={shade ? { backgroundColor: `color-mix(in oklab, var(--color-brand-600) ${shade}%, transparent)` } : undefined}
                />
              );
            })}
          </Row>
        ))}
      </div>
      <div className="mt-4 flex flex-wrap items-center gap-x-5 gap-y-2 text-xs text-muted-foreground">
        <span className="inline-flex items-center gap-1.5">
          <span className="size-2.5 rounded-[3px] bg-black/[0.025] ring-1 ring-black/5" /> {words.none}
        </span>
        <span className="inline-flex items-center gap-1.5">
          <span className="size-2.5 rounded-[3px] bg-black/[0.08]" /> {words.zero}
        </span>
        <span className="inline-flex items-center gap-1.5">
          Lower
          <span className="h-2.5 w-16 rounded-[3px] bg-gradient-to-r from-brand-600/15 to-brand-600" />
          {words.scale}
        </span>
      </div>
    </Card>
  );
}

function Row({ day, children }: { day: string; children: React.ReactNode }) {
  return (
    <>
      <span className="self-center font-mono text-[11px] text-muted-foreground">{day}</span>
      {children}
    </>
  );
}
