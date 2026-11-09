"use client";

import { CalendarClock } from "lucide-react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { RepliesSection } from "@/components/app/settings/replies-section";
import { Button } from "@/components/ui/button";
import { Field, Select } from "@/components/ui/form";
import type { HourStats, WeekdayStats } from "@/lib/types";
import { cn } from "@/lib/utils";

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const TIMEZONES = Intl.supportedValuesOf("timeZone");

const hour = (h: number) => `${String(h).padStart(2, "0")}:00`;

/** [1,2,3,4,5] -> "Mon–Fri", [1,3,5] -> "Mon, Wed, Fri", [1,2,3,6,7] -> "Mon–Wed, Sat, Sun" */
function formatDays(days: number[]): string {
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

export default function SchedulePage() {
  return (
    <ProjectSettingsPage
      title="Schedule"
      description="When the agent is allowed to send. Replies are read around the clock."
      problem={(s) =>
        !s.send_days.length ? "Pick at least one day" : s.send_end_hour <= s.send_start_hour ? "End time must be after start time" : null
      }
    >
      {(s, update) => {
        const zones = TIMEZONES.includes(s.timezone) ? TIMEZONES : [s.timezone, ...TIMEZONES];
        const browserZone = Intl.DateTimeFormat().resolvedOptions().timeZone;
        const valid = s.send_days.length > 0 && s.send_end_hour > s.send_start_hour;
        return (
          <>
            <Section title="Time zone" description="Send days and hours are counted in this time zone. Use the one most of your leads are in.">
              <div className="flex items-center gap-2">
                <Select value={s.timezone} onChange={(e) => update({ timezone: e.target.value })} className="flex-1" aria-label="Time zone">
                  {zones.map((z) => (
                    <option key={z} value={z}>
                      {z.replaceAll("_", " ")}
                    </option>
                  ))}
                </Select>
                {browserZone !== s.timezone && (
                  <Button variant="outline" onClick={() => update({ timezone: browserZone })}>
                    Use {browserZone.replaceAll("_", " ")}
                  </Button>
                )}
              </div>
            </Section>

            <Section title="Send window" description="Emails due outside this window wait until it opens again.">
              <Field label="Days" group>
                {/* Seven columns as wide as the From/Until row below, so the week stays on one row on phones instead of breaking 4 + 3. */}
                <div className="grid max-w-sm grid-cols-7 gap-1 sm:gap-1.5">
                  {DAYS.map((label, i) => {
                    const day = i + 1;
                    const on = s.send_days.includes(day);
                    return (
                      <button
                        key={day}
                        type="button"
                        aria-pressed={on}
                        onClick={() =>
                          update({ send_days: on ? s.send_days.filter((d) => d !== day) : [...s.send_days, day].sort((a, b) => a - b) })
                        }
                        className={cn(
                          "rounded-lg border py-2 text-sm font-medium transition-colors",
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
                  <Select value={s.send_start_hour} onChange={(e) => update({ send_start_hour: Number(e.target.value) })}>
                    {Array.from({ length: 24 }, (_, h) => (
                      <option key={h} value={h}>
                        {hour(h)}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Until">
                  <Select value={s.send_end_hour} onChange={(e) => update({ send_end_hour: Number(e.target.value) })}>
                    {Array.from({ length: 24 }, (_, i) => i + 1).map((h) => (
                      <option key={h} value={h}>
                        {hour(h)}
                      </option>
                    ))}
                  </Select>
                </Field>
              </div>
              {valid && (
                <p className="mt-5 flex items-center gap-2 rounded-xl bg-muted px-4 py-3 text-sm">
                  <CalendarClock className="size-4 text-muted-foreground" />
                  Sends {formatDays(s.send_days)}, {hour(s.send_start_hour)}–{hour(s.send_end_hour)} {s.timezone}
                </p>
              )}
            </Section>
            <RepliesSection<WeekdayStats> stat="weekdays" title="Replies by send day" column="Day" label={(r) => DAYS[r.weekday - 1]!} />
            <RepliesSection<HourStats>
              stat="hours"
              title="Replies by send hour"
              column="Sent between"
              label={(r) => `${hour(r.hour)}–${hour(r.hour + 1)}`}
            />
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}
