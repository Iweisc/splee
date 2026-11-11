"use client";

import { useState } from "react";
import useSWR from "swr";
import { CalendarCheck, CircleAlert, ExternalLink } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyField } from "@/components/app/settings/copy-button";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { MicroLabel } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Switch } from "@/components/ui/form";
import { Card, EmptyState, Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { BookingPage, BookingPageInput, Mailbox } from "@/lib/types";

const DAYS = ["Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday", "Sunday"];
const TIMEZONES = Intl.supportedValuesOf("timeZone");
const LENGTHS = ["15", "30", "45", "60"] as const;
const BUFFERS = [0, 5, 10, 15, 30, 60];
const NOTICE: [number, string][] = [
  [0, "None"],
  [60, "1 hour"],
  [120, "2 hours"],
  [240, "4 hours"],
  [720, "12 hours"],
  [1440, "1 day"],
  [2880, "2 days"],
];
/** The preset choices, plus a page's own value when it was set to something else. */
const noticeOptions = (value: number) => (NOTICE.some(([m]) => m === value) ? NOTICE : [...NOTICE, [value, `${value} minutes`] as [number, string]]);
const clock = (m: number) => `${String(Math.floor(m / 60)).padStart(2, "0")}:${String(m % 60).padStart(2, "0")}`;
const HALF_HOURS = Array.from({ length: 49 }, (_, i) => i * 30);

export default function BookingPagesPage() {
  const { data: pages, error, mutate } = useSWR<BookingPage[]>("/booking-pages", fetcher);
  const { data: mailboxes } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const without = (mailboxes ?? []).filter((m) => !pages?.some((p) => p.mailbox_id === m.id));
  const [mailbox, setMailbox] = useState("");
  const { pending, run } = useAction();

  const create = () =>
    run("create", async () => {
      const page = await api<BookingPage>("/booking-pages", { body: { mailbox_id: mailbox || without[0]!.id } });
      await mutate((all) => [...(all ?? []), page], { revalidate: false });
      setMailbox("");
    });

  return (
    <>
      <PageHeader
        title="Booking pages"
        description="A page where leads pick a free time on your calendar, for teams without Calendly or Cal.com. Each mailbox can have one."
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="booking pages" retry={() => mutate()} />
          ) : !pages ? (
            <SectionsSkeleton count={2} />
          ) : (
            <>
              {pages.map((p) => (
                <PageCard key={p.id} page={p} onSaved={(next) => mutate((all) => all?.map((x) => (x.id === next.id ? next : x)), { revalidate: false })} />
              ))}
              {mailboxes && !mailboxes.length ? (
                <Card>
                  <EmptyState icon={<CalendarCheck className="size-6" />} title="Connect a mailbox first">
                    Bookings are emailed from a mailbox, so each booking page belongs to one. Add one on the Mailboxes page.
                  </EmptyState>
                </Card>
              ) : (
                without.length > 0 && (
                  <Section
                    title={pages.length ? "Add another page" : "Add a booking page"}
                    description="Pick the mailbox whose owner takes the calls. Invites go out from it."
                  >
                    <div className="flex flex-wrap gap-2">
                      <Select value={mailbox || without[0]!.id} onChange={(e) => setMailbox(e.target.value)} className="min-w-0 flex-1 basis-60">
                        {without.map((m) => (
                          <option key={m.id} value={m.id}>
                            {m.from_name ? `${m.from_name} (${m.email})` : m.email}
                          </option>
                        ))}
                      </Select>
                      <Button onClick={create} loading={pending === "create"}>
                        Add page
                      </Button>
                    </div>
                  </Section>
                )
              )}
            </>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}

type Draft = Required<Omit<BookingPageInput, "calendar_url">> & { calendar_url: string };

function draftOf(p: BookingPage): Draft {
  const { slug, name, duration_minutes, timezone, hours, buffer_minutes, notice_minutes, days_ahead, location } = p;
  return { slug, name, duration_minutes, timezone, hours, buffer_minutes, notice_minutes, days_ahead, location, calendar_url: "" };
}

function PageCard({ page, onSaved }: { page: BookingPage; onSaved: (p: BookingPage) => void }) {
  const [d, setD] = useState<Draft>(() => draftOf(page));
  const { pending, run } = useAction();
  const set = (patch: Partial<Draft>) => setD((prev) => ({ ...prev, ...patch }));
  const saved = draftOf(page);
  const dirty = JSON.stringify(d) !== JSON.stringify(saved);
  const zones = TIMEZONES.includes(d.timezone) ? TIMEZONES : [d.timezone, ...TIMEZONES];
  const badHours = d.hours.some((h) => h && h[0] >= h[1]);
  const valid = d.name.trim() && /^[a-z0-9][a-z0-9-]{1,47}$/.test(d.slug) && !badHours && d.days_ahead >= 1 && d.days_ahead <= 90;

  const save = (body: BookingPageInput, success: string) =>
    run(
      "save",
      async () => {
        const next = await api<BookingPage>(`/booking-pages/${page.id}`, { method: "PATCH", body });
        onSaved(next);
        setD(draftOf(next));
      },
      success,
    );
  const submit = () => {
    const { calendar_url, ...rest } = d;
    save(calendar_url.trim() ? { ...rest, calendar_url: calendar_url.trim() } : rest, "Booking page saved");
  };

  return (
    <Section
      title={page.name}
      description={`${page.host}, from ${page.mailbox_email}`}
      actions={
        <a href={page.url} target="_blank" rel="noreferrer" className="flex items-center gap-1.5 text-sm font-medium hover:underline">
          Open <ExternalLink className="size-3.5" />
        </a>
      }
    >
      <div className="grid gap-6">
        <CopyField value={page.url} />

        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Call name">
            <Input value={d.name} maxLength={80} onChange={(e) => set({ name: e.target.value })} />
          </Field>
          <Field label="Link" hint={d.slug && !/^[a-z0-9][a-z0-9-]{1,47}$/.test(d.slug) ? "Lowercase letters, numbers and dashes" : undefined}>
            <div className="flex items-center rounded-lg border border-border-strong bg-muted focus-within:border-foreground/40">
              <span className="pl-3 text-sm text-muted-foreground">/book/</span>
              <Input
                value={d.slug}
                onChange={(e) => set({ slug: e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, "") })}
                className="border-0 pl-0.5 focus:ring-0"
              />
            </div>
          </Field>
        </div>

        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Length" group>
            <Tabs
              value={String(d.duration_minutes) as (typeof LENGTHS)[number]}
              onChange={(v) => set({ duration_minutes: Number(v) as Draft["duration_minutes"] })}
              options={LENGTHS.map((m) => ({ value: m, label: `${m} min` }))}
            />
          </Field>
          <Field label="Time zone">
            <Select value={d.timezone} onChange={(e) => set({ timezone: e.target.value })}>
              {zones.map((z) => (
                <option key={z} value={z}>
                  {z.replaceAll("_", " ")}
                </option>
              ))}
            </Select>
          </Field>
        </div>

        <fieldset className="grid gap-2">
          <MicroLabel>Working hours</MicroLabel>
          {DAYS.map((day, i) => {
            const h = d.hours[i] ?? null;
            const setDay = (next: [number, number] | null) => set({ hours: d.hours.map((x, j) => (j === i ? next : x)) });
            return (
              <div key={day} className="grid grid-cols-[2.25rem_2.5rem_minmax(0,1fr)] items-center gap-3 sm:grid-cols-[2.25rem_7rem_minmax(0,1fr)]">
                <Switch checked={!!h} onChange={(on) => setDay(on ? [540, 1020] : null)} label={day} />
                <span className="text-sm font-medium">
                  <span className="sm:hidden">{day.slice(0, 3)}</span>
                  <span className="max-sm:hidden">{day}</span>
                </span>
                {h ? (
                  // On phones the two times share what is left of the row, about 90px each. That fits "09:00" only with less padding
                  // around the native arrow.
                  <div className="flex min-w-0 items-center gap-1.5 sm:gap-2">
                    <Select
                      value={h[0]}
                      onChange={(e) => setDay([Number(e.target.value), h[1]])}
                      className="min-w-0 flex-1 max-sm:pr-1 max-sm:pl-2 sm:w-28 sm:flex-none"
                      aria-label={`${day} from`}
                    >
                      {HALF_HOURS.slice(0, -1).map((m) => (
                        <option key={m} value={m}>
                          {clock(m)}
                        </option>
                      ))}
                    </Select>
                    <span className="text-sm text-muted-foreground">to</span>
                    <Select
                      value={h[1]}
                      onChange={(e) => setDay([h[0], Number(e.target.value)])}
                      className="min-w-0 flex-1 max-sm:pr-1 max-sm:pl-2 sm:w-28 sm:flex-none"
                      aria-label={`${day} until`}
                    >
                      {HALF_HOURS.slice(1).map((m) => (
                        <option key={m} value={m}>
                          {m === 1440 ? "24:00" : clock(m)}
                        </option>
                      ))}
                    </Select>
                  </div>
                ) : (
                  <span className="text-sm text-muted-foreground">Not available</span>
                )}
              </div>
            );
          })}
          {badHours && <p className="text-xs text-danger">Each day needs to end after it starts.</p>}
        </fieldset>

        <div className="grid gap-4 sm:grid-cols-3">
          <Field label="Gap between calls">
            <Select value={d.buffer_minutes} onChange={(e) => set({ buffer_minutes: Number(e.target.value) })}>
              {(BUFFERS.includes(d.buffer_minutes) ? BUFFERS : [...BUFFERS, d.buffer_minutes].sort((a, b) => a - b)).map((m) => (
                <option key={m} value={m}>
                  {m ? `${m} minutes` : "None"}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Minimum notice">
            <Select value={d.notice_minutes} onChange={(e) => set({ notice_minutes: Number(e.target.value) })}>
              {noticeOptions(d.notice_minutes).map(([m, label]) => (
                <option key={m} value={m}>
                  {label}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="Days ahead">
            <NumberInput min={1} max={90} value={d.days_ahead} onValueChange={(days_ahead) => set({ days_ahead })} className="tabular-nums" />
          </Field>
        </div>

        <Field label="Where" hint="A video link goes only in the invite. Leads see that it's a video call.">
          <Input
            value={d.location}
            maxLength={500}
            onChange={(e) => set({ location: e.target.value })}
            placeholder="https://meet.google.com/abc-defg-hij"
          />
        </Field>

        <Field
          label="Busy times from your calendar"
          hint={
            page.calendar_host
              ? "Paste a new address to replace it."
              : "In Google Calendar, open the calendar's settings and copy Secret address in iCal format. " +
                "Outlook and iCloud can publish a calendar as an ICS link too."
          }
        >
          {page.calendar_host && (
            <div className="flex items-center justify-between gap-2 rounded-lg bg-muted py-1 pr-1 pl-3 text-sm">
              <span className="min-w-0 break-words">
                Reading busy times from <strong className="font-medium">{page.calendar_host}</strong>
              </span>
              <Button
                size="sm"
                variant="ghost"
                className="shrink-0"
                onClick={() => save({ calendar_url: "" }, "Calendar removed")}
                disabled={pending === "save"}
              >
                Remove
              </Button>
            </div>
          )}
          <Input
            type="url"
            value={d.calendar_url}
            onChange={(e) => set({ calendar_url: e.target.value })}
            placeholder="https://calendar.google.com/calendar/ical/…/basic.ics"
            autoComplete="off"
          />
        </Field>
        {page.calendar_error && (
          <p className="-mt-3 flex items-start gap-2 rounded-xl bg-amber-50 px-4 py-3 text-sm text-amber-800">
            <CircleAlert className="mt-0.5 size-4 shrink-0" />
            Couldn&apos;t read the calendar the last time, so its busy times may be out of date: {page.calendar_error}
          </p>
        )}

        <div className="flex flex-wrap items-center justify-end gap-2">
          {dirty && (
            <Button variant="ghost" onClick={() => setD(saved)} disabled={pending === "save"}>
              Discard
            </Button>
          )}
          <Button onClick={submit} disabled={!dirty || !valid} loading={pending === "save"}>
            Save
          </Button>
        </div>

        <div>
          <MicroLabel>Upcoming calls</MicroLabel>
          {page.upcoming.length ? (
            <ul className="mt-2 divide-y divide-border rounded-xl border border-border">
              {page.upcoming.map((b) => (
                <li key={b.id} className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1 px-4 py-3 text-sm">
                  <span className="min-w-0">
                    <span className="font-medium">{b.name}</span> <span className="break-all text-muted-foreground">{b.email}</span>
                    {b.note && <span className="mt-0.5 block text-muted-foreground">{b.note}</span>}
                  </span>
                  <span className="whitespace-nowrap tabular-nums">
                    {new Date(b.starts_at).toLocaleString(undefined, {
                      timeZone: page.timezone,
                      weekday: "short",
                      month: "short",
                      day: "numeric",
                      hour: "numeric",
                      minute: "2-digit",
                    })}
                  </span>
                </li>
              ))}
            </ul>
          ) : (
            <p className="mt-2 text-sm text-muted-foreground">No calls booked yet. Share the link above, or pick it as a mailbox&apos;s booking link.</p>
          )}
        </div>
      </div>
    </Section>
  );
}
