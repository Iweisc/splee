"use client";

import { useMemo, useState } from "react";
import { ChevronLeft, ChevronRight, Globe } from "lucide-react";
import { Select } from "@/components/ui/form";
import type { Ts } from "@/lib/types";
import { cn } from "@/lib/utils";

const ZONES = Intl.supportedValuesOf("timeZone");

export const browserZone = () => Intl.DateTimeFormat().resolvedOptions().timeZone;

/** "2026-11-17", the day a time falls on in `zone`. */
function dayKey(ts: Ts, zone: string): string {
  return new Intl.DateTimeFormat("en-CA", { timeZone: zone, year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date(ts));
}

// Days are kept as noon UTC, so adding a day never lands on the wrong date whatever the visitor's time zone does.
const noon = (key: string) => new Date(`${key}T12:00:00Z`);
const keyOf = (d: Date) => d.toISOString().slice(0, 10);
const addDays = (d: Date, n: number) => new Date(d.getTime() + n * 86_400_000);
const monday = (key: string) => {
  const d = noon(key);
  return addDays(d, -((d.getUTCDay() + 6) % 7));
};

export function time(ts: Ts, zone: string): string {
  return new Date(ts).toLocaleTimeString(undefined, { timeZone: zone, hour: "numeric", minute: "2-digit" });
}

/** "Tuesday, November 17, 9:00 AM – 9:30 AM" */
export function when(start: Ts, end: Ts, zone: string): string {
  const day = new Date(start).toLocaleDateString(undefined, { timeZone: zone, weekday: "long", month: "long", day: "numeric" });
  return `${day}, ${time(start, zone)} – ${time(end, zone)}`;
}

export function ZonePicker({ zone, onChange }: { zone: string; onChange: (zone: string) => void }) {
  const zones = ZONES.includes(zone) ? ZONES : [zone, ...ZONES];
  return (
    <label className="flex items-center gap-2 text-sm text-muted-foreground">
      <Globe className="size-4 shrink-0" />
      <span className="sr-only">Time zone</span>
      <Select value={zone} onChange={(e) => onChange(e.target.value)} className="h-9 min-w-0 flex-1 text-sm">
        {zones.map((z) => (
          <option key={z} value={z}>
            {z.replaceAll("_", " ")}
          </option>
        ))}
      </Select>
    </label>
  );
}

/** A week at a time, days without a free slot greyed out, then the chosen day's times. It opens on `near`'s day when that has
 * free times, like the day of a call being moved, else on the first free day. */
export function SlotPicker({
  slots,
  zone,
  picked,
  near,
  onPick,
}: {
  slots: Ts[];
  zone: string;
  picked: Ts | null;
  near?: Ts;
  onPick: (slot: Ts) => void;
}) {
  const byDay = useMemo(() => {
    const days = new Map<string, Ts[]>();
    for (const s of slots) {
      const key = dayKey(s, zone);
      days.set(key, [...(days.get(key) ?? []), s]);
    }
    return days;
  }, [slots, zone]);
  const days = [...byDay.keys()];
  const [chosen, setChosen] = useState<string | null>(null);
  const day = [chosen, picked && dayKey(picked, zone), near && dayKey(near, zone)].find((k) => k && byDay.has(k)) || days[0] || null;
  const [weekOf, setWeekOf] = useState<string | null>(null);
  const start = monday(weekOf ?? day ?? keyOf(new Date()));
  const first = days[0] ? monday(days[0]) : start;
  const last = days.at(-1) ? monday(days.at(-1)!) : start;
  const week = Array.from({ length: 7 }, (_, i) => keyOf(addDays(start, i)));

  if (!days.length) {
    return (
      <p className="rounded-xl bg-muted px-4 py-6 text-center text-sm text-balance text-muted-foreground">
        No free times in the next few weeks. Reply to the email that brought you here to find a time.
      </p>
    );
  }

  const month = start.toLocaleDateString(undefined, { timeZone: "UTC", month: "long", year: "numeric" });
  const go = (n: number) => {
    const next = keyOf(addDays(start, 7 * n));
    setWeekOf(next);
    const open = Array.from({ length: 7 }, (_, i) => keyOf(addDays(noon(next), i))).find((k) => byDay.has(k));
    if (open) setChosen(open);
  };

  return (
    <div className="flex flex-col gap-5">
      <div>
        <div className="mb-3 flex items-center justify-between">
          <p className="font-medium">{month}</p>
          <div className="flex gap-1">
            <button
              type="button"
              onClick={() => go(-1)}
              disabled={start <= first}
              className="rounded-lg p-1.5 hover:bg-accent disabled:opacity-30"
              aria-label="Previous week"
            >
              <ChevronLeft className="size-5" />
            </button>
            <button
              type="button"
              onClick={() => go(1)}
              disabled={start >= last}
              className="rounded-lg p-1.5 hover:bg-accent disabled:opacity-30"
              aria-label="Next week"
            >
              <ChevronRight className="size-5" />
            </button>
          </div>
        </div>
        <div className="grid grid-cols-7 gap-1">
          {week.map((key) => {
            const open = byDay.has(key);
            const d = noon(key);
            return (
              <button
                key={key}
                type="button"
                disabled={!open}
                aria-pressed={key === day}
                onClick={() => setChosen(key)}
                className={cn(
                  "flex flex-col items-center rounded-xl py-2 text-sm transition-colors",
                  key === day ? "bg-primary text-primary-foreground" : open ? "bg-brand-50 text-brand-700 hover:bg-brand-100" : "text-subtle-foreground",
                )}
              >
                <span className="text-xs">{d.toLocaleDateString(undefined, { timeZone: "UTC", weekday: "short" })}</span>
                <span className="text-base font-medium tabular-nums">{d.getUTCDate()}</span>
              </button>
            );
          })}
        </div>
      </div>
      {day && (
        <div>
          <p className="mb-3 text-sm font-medium">
            {noon(day).toLocaleDateString(undefined, { timeZone: "UTC", weekday: "long", month: "long", day: "numeric" })}
          </p>
          <div className="grid grid-cols-3 gap-2 sm:grid-cols-4">
            {byDay.get(day)!.map((s) => (
              <button
                key={s}
                type="button"
                aria-pressed={s === picked}
                onClick={() => onPick(s)}
                className={cn(
                  "rounded-lg border py-2.5 text-sm font-medium tabular-nums transition-colors",
                  s === picked ? "border-foreground bg-primary text-primary-foreground" : "border-border-strong bg-card hover:border-foreground",
                )}
              >
                {time(s, zone)}
              </button>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
