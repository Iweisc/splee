"use client";

import { CalendarClock, X } from "lucide-react";
import { useState } from "react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { RepliesSection } from "@/components/app/settings/replies-section";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import type { HourStats, WeekdayStats } from "@/lib/types";
import { cn, zoneName } from "@/lib/utils";

const DAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
const TIMEZONES = Intl.supportedValuesOf("timeZone");

const hour = (h: number) => `${String(h).padStart(2, "0")}:00`;
// Keep in sync with MAX_DAYS_OFF in backend/src/routes/settings.rs.
const MAX_DAYS_OFF = 100;

/** "Fri, Dec 25, 2026" for "2026-12-25", read as a calendar date in UTC so the viewer's time zone can't move it a day. */
const dayLabel = (d: string) =>
  new Date(`${d}T00:00:00Z`).toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric", year: "numeric", timeZone: "UTC" });

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
        const zone = zoneName(s.timezone);
        const valid = s.send_days.length > 0 && s.send_end_hour > s.send_start_hour;
        return (
          <>
            <Section
              title="Time zone"
              description={
                s.lead_time_zone
                  ? "Send days and hours are counted in each lead's own time zone, and in this one for leads whose zone isn't known."
                  : "Send days and hours are counted in this time zone. Use the one most of your leads are in."
              }
            >
              <div className="flex items-center gap-2">
                <Select value={s.timezone} onChange={(e) => update({ timezone: e.target.value })} className="flex-1" aria-label="Time zone">
                  {zones.map((z) => (
                    <option key={z} value={z}>
                      {zoneName(z)}
                    </option>
                  ))}
                </Select>
                {browserZone !== s.timezone && (
                  <Button variant="outline" onClick={() => update({ timezone: browserZone })}>
                    Use {zoneName(browserZone)}
                  </Button>
                )}
              </div>
              <div className="mt-5 flex items-start justify-between gap-4">
                <div className="text-sm">
                  <p className="font-medium">In each lead&apos;s own time zone</p>
                  <p className="mt-0.5 text-muted-foreground">
                    From their phone number or country. Leads in a country with several time zones, or with neither, use the one above.
                  </p>
                </div>
                <Switch checked={s.lead_time_zone} onChange={(on) => update({ lead_time_zone: on })} label="In each lead's own time zone" />
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
                        aria-label={label}
                        onClick={() =>
                          update({ send_days: on ? s.send_days.filter((d) => d !== day) : [...s.send_days, day].sort((a, b) => a - b) })
                        }
                        className={cn(
                          "rounded-lg border py-2 text-sm font-medium transition-colors",
                          on ? "border-foreground bg-primary text-primary-foreground" : "border-border-strong bg-card hover:bg-accent",
                        )}
                      >
                        {/* A column is only about 30px wide on a 320px phone, which splits "Wed" in two, so phones get two letters. */}
                        <span className="sm:hidden">{label.slice(0, 2)}</span>
                        <span className="max-sm:hidden">{label}</span>
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
                  Sends {formatDays(s.send_days)}, {hour(s.send_start_hour)}–{hour(s.send_end_hour)}{" "}
                  {s.lead_time_zone ? `in each lead's time zone, else ${zone}` : zone}
                </p>
              )}
            </Section>
            <DaysOff days={s.days_off} onChange={(days_off) => update({ days_off })} />
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

function DaysOff({ days, onChange }: { days: string[]; onChange: (days: string[]) => void }) {
  const [picked, setPicked] = useState("");
  const add = () => {
    onChange([...days, picked].sort());
    setPicked("");
  };
  return (
    <Section
      title="Days off"
      description="No first emails, follow-ups, texts or LinkedIn steps go out on these dates, like holidays. Replies still do."
    >
      <div className="flex max-w-sm gap-2">
        <Input type="date" value={picked} onChange={(e) => setPicked(e.target.value)} aria-label="Day off" className="flex-1" />
        <Button variant="outline" onClick={add} disabled={!picked || days.includes(picked) || days.length >= MAX_DAYS_OFF}>
          Add
        </Button>
      </div>
      {days.length > 0 && (
        <ul className="mt-4 flex flex-wrap gap-2">
          {days.map((d) => (
            <li key={d} className="flex items-center gap-1 rounded-lg border border-border-strong bg-card py-1 pr-1 pl-3 text-sm">
              {dayLabel(d)}
              <button
                type="button"
                onClick={() => onChange(days.filter((x) => x !== d))}
                aria-label={`Remove ${dayLabel(d)}`}
                className="rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
              >
                <X className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}
