"use client";

import { Field, Select } from "@/components/ui/form";
import { cn } from "@/lib/utils";

export const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

export const hour = (h: number) => `${String(h).padStart(2, "0")}:00`;

/** [1,2,3,4,5] -> "Mon–Fri", [1,3,5] -> "Mon, Wed, Fri", [1,2,3,6,7] -> "Mon–Wed, Sat, Sun" */
export function formatDays(days: number[]): string {
  if (days.length === 7) return "every day";
  const sorted = [...days].sort((a, b) => a - b);
  const runs: number[][] = [];
  for (const d of sorted) {
    const last = runs.at(-1);
    if (last && last.at(-1) === d - 1) last.push(d);
    else runs.push([d]);
  }
  return runs
    .flatMap((r) => (r.length >= 3 ? [`${DAYS[r[0]! - 1]}–${DAYS[r.at(-1)! - 1]}`] : r.map((d) => DAYS[d - 1])))
    .join(", ");
}

export interface SendWindow {
  days: number[];
  start_hour: number;
  end_hour: number;
}

/** What's wrong with a send window, or null. Mirrors check_send_window in backend/src/routes/settings.rs. */
export const windowProblem = (w: SendWindow) =>
  !w.days.length ? "Pick at least one day" : w.end_hour <= w.start_hour ? "End time must be after start time" : null;

/** The send days and the From and Until hours, the project's on the Schedule page and a campaign's own on its page. */
export function SendWindowFields({ value, onChange }: { value: SendWindow; onChange: (w: SendWindow) => void }) {
  return (
    <>
      <Field label="Days" group>
        {/* Seven columns as wide as the From/Until row below, so the week stays on one row on phones instead of breaking 4 + 3. */}
        <div className="grid max-w-sm grid-cols-7 gap-1 sm:gap-1.5">
          {DAYS.map((label, i) => {
            const day = i + 1;
            const on = value.days.includes(day);
            return (
              <button
                key={day}
                type="button"
                aria-pressed={on}
                onClick={() => onChange({ ...value, days: on ? value.days.filter((d) => d !== day) : [...value.days, day].sort((a, b) => a - b) })}
                className={cn(
                  // A column is only about 30px wide on a 320px phone, where "Wed" at text-sm splits in two. Two letters fit, but
                  // then the visible word isn't the button's name, which voice control users say aloud.
                  "rounded-lg border py-2 text-sm font-medium whitespace-nowrap transition-colors max-sm:py-2.5 max-sm:text-xs",
                  on ? "border-foreground bg-primary text-primary-foreground" : "border-border-strong bg-card hover:bg-accent",
                )}
              >
                {label}
              </button>
            );
          })}
        </div>
      </Field>
      <div className="mt-5 grid max-w-sm grid-cols-2 gap-4">
        <Field label="From">
          <Select value={value.start_hour} onChange={(e) => onChange({ ...value, start_hour: Number(e.target.value) })}>
            {Array.from({ length: 24 }, (_, h) => (
              <option key={h} value={h}>
                {hour(h)}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Until">
          <Select value={value.end_hour} onChange={(e) => onChange({ ...value, end_hour: Number(e.target.value) })}>
            {Array.from({ length: 24 }, (_, i) => i + 1).map((h) => (
              <option key={h} value={h}>
                {hour(h)}
              </option>
            ))}
          </Select>
        </Field>
      </div>
    </>
  );
}
