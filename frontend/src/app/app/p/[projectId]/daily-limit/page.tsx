"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import useSWR from "swr";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { MicroLabel } from "@/components/app/work/states";
import { Field, NumberInput, Switch } from "@/components/ui/form";
import { Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { mailboxFamily, SIGN_IN_LABEL, todayLimit } from "@/lib/mailbox";
import type { Mailbox, MonthlyCap, ProjectSettings } from "@/lib/types";
import { money, num } from "@/lib/utils";

const MIN = 1;
const MAX = 5000;
// Most projects sit well under this, so the slider stays precise. Larger limits go in the number box.
const SLIDER_MAX = 1000;
const CAP_USD = { min: 1, max: 1_000_000, start: 100 };

function problem(s: ProjectSettings) {
  if (s.daily_email_limit < MIN || s.daily_email_limit > MAX) return `Daily limit must be between ${MIN} and ${num(MAX)}`;
  if (s.mailbox_ids?.length === 0) return "Pick at least one mailbox to send from";
  const cap = s.monthly_cap_micros;
  if (cap !== null && !(cap >= CAP_USD.min * 1_000_000 && cap <= CAP_USD.max * 1_000_000)) {
    return `The monthly cap must be between $${CAP_USD.min} and ${money(CAP_USD.max * 1_000_000, { cents: false })}`;
  }
  return null;
}

export default function DailyLimitPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const { data: mailboxes, error: mailboxesError } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const { data: cap } = useSWR<MonthlyCap>(`/projects/${projectId}/monthly-cap`, fetcher);
  // Without a price on any mailbox, sent emails cost nothing here and a cap never stops anything.
  const unpriced = mailboxes !== undefined && mailboxes.every((m) => m.cost_per_email_micros === 0);

  return (
    <ProjectSettingsPage title="Daily limit" description="How much this project sends, and from which mailboxes." problem={problem}>
      {(s, update) => {
        const active = mailboxes?.filter((m) => m.status === "active" && (s.mailbox_ids === null || s.mailbox_ids.includes(m.id)));
        const capacity = active?.reduce((sum, m) => sum + todayLimit(m), 0);
        const families = new Set(active?.map(mailboxFamily));
        // When nothing can send, the Emails per day card already says so, so this stays quiet.
        const missing = !capacity ? [] : (["google", "microsoft"] as const).filter((f) => !families.has(f));
        const picked = s.mailbox_ids;
        const toggle = (id: string) => picked && update({ mailbox_ids: picked.includes(id) ? picked.filter((m) => m !== id) : [...picked, id] });
        return (
          <>
            <Section
              title="Emails per day"
              description="Cold emails stop for the day once this project has sent this many. Replies to leads count toward it but are never held back."
            >
              <div className="flex items-center gap-4">
                <input
                  type="range"
                  min={MIN}
                  max={SLIDER_MAX}
                  step={1}
                  value={Math.min(Math.max(s.daily_email_limit, MIN), SLIDER_MAX)}
                  onChange={(e) => update({ daily_email_limit: Number(e.target.value) })}
                  className="h-2 flex-1 cursor-pointer accent-black"
                  aria-label="Daily limit"
                />
                <div className="relative w-32">
                  <NumberInput
                    min={MIN}
                    max={MAX}
                    step={1}
                    value={s.daily_email_limit}
                    onValueChange={(n) => update({ daily_email_limit: Math.round(n) })}
                    className="pr-12 tabular-nums"
                    aria-label="Emails per day"
                  />
                  <span className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm text-muted-foreground">/day</span>
                </div>
              </div>
              <div className="mt-2 flex justify-between font-mono text-[11px] text-muted-foreground">
                <span>{MIN}</span>
                <span>{num(SLIDER_MAX)}</span>
              </div>
              <div className="mt-5 grid gap-3 sm:grid-cols-2">
                <Stat label="Per 30 days" value={`up to ${num(s.daily_email_limit * 30)}`} />
                <Stat
                  label={picked ? "Its active mailboxes allow" : "Your active mailboxes allow"}
                  value={capacity !== undefined ? `${num(capacity)}/day` : mailboxesError ? "Couldn't load" : null}
                />
              </div>
              {capacity === 0 ? (
                <p className="mt-3 text-sm text-muted-foreground">
                  Nothing sends until you{" "}
                  <Link href="/app/domains" className="font-medium whitespace-nowrap text-foreground hover:underline">
                    connect a mailbox
                  </Link>
                  .
                </p>
              ) : (
                capacity !== undefined &&
                capacity < s.daily_email_limit && (
                  <p className="mt-3 text-sm text-muted-foreground">
                    Your mailboxes&apos; own limits are lower, so this project will send at most {num(capacity)} a day, shared with your other projects.
                  </p>
                )
              )}
            </Section>

            <Section
              title="Send from"
              description="The mailboxes that send this project's first emails, so mailboxes on another brand's domain stay off it. Follow-ups and replies stay with the mailbox the lead already talks to."
              toggle={
                <Switch
                  checked={picked !== null}
                  onChange={(on) => update({ mailbox_ids: on ? (mailboxes ?? []).map((m) => m.id) : null })}
                  label="Only the mailboxes I pick"
                />
              }
            >
              {picked !== null &&
                (!mailboxes ? (
                  <Skeleton className="h-24 w-full" />
                ) : (
                  <ul className="divide-y divide-border rounded-xl border border-border">
                    {mailboxes.map((m) => (
                      <li key={m.id}>
                        <label className="flex cursor-pointer items-center gap-3 px-3 py-2.5 text-sm">
                          <input type="checkbox" checked={picked.includes(m.id)} onChange={() => toggle(m.id)} className="accent-black" />
                          <span className="min-w-0 flex-1 truncate font-medium">{m.email}</span>
                          {m.status !== "active" && <span className="shrink-0 text-xs text-muted-foreground capitalize">{m.status}</span>}
                        </label>
                      </li>
                    ))}
                  </ul>
                ))}
            </Section>

            <Section
              title="Monthly cap"
              description="The most this project may spend on emails in a calendar month (UTC), at each mailbox's price per email. Once it's reached, first emails and follow-ups wait for the 1st. Replies to leads still go out."
              toggle={
                <Switch
                  checked={s.monthly_cap_micros !== null}
                  onChange={(on) => update({ monthly_cap_micros: on ? CAP_USD.start * 1_000_000 : null })}
                  label="Monthly cap"
                />
              }
            >
              {s.monthly_cap_micros !== null && (
                <>
                  <Field label="Email spend" hint={cap ? `${money(cap.spent_this_month_micros)} spent so far this month.` : "Sent emails this month."}>
                    <div className="relative w-48">
                      <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
                      <NumberInput
                        min={CAP_USD.min}
                        max={CAP_USD.max}
                        step={1}
                        value={s.monthly_cap_micros / 1_000_000}
                        onValueChange={(usd) => update({ monthly_cap_micros: Math.round(usd * 1_000_000) })}
                        className="pr-16 pl-7 tabular-nums"
                        aria-label="Monthly cap"
                      />
                      <span className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm text-muted-foreground">/month</span>
                    </div>
                  </Field>
                  {cap?.reached && cap.resumes_at && (
                    <p className="mt-3 text-sm text-muted-foreground">
                      Reached. First emails and follow-ups wait until{" "}
                      {new Date(cap.resumes_at).toLocaleDateString("en-US", { month: "long", day: "numeric", timeZone: "UTC" })}, or raise the cap.
                    </p>
                  )}
                  {unpriced && (
                    <p className="mt-3 text-sm text-muted-foreground">
                      Your mailboxes have no price per email, so nothing counts toward the cap. Set one on{" "}
                      <Link href="/app/domains" className="font-medium whitespace-nowrap text-foreground hover:underline">
                        each mailbox
                      </Link>
                      .
                    </p>
                  )}
                </>
              )}
            </Section>

            <Section
              title="Match the lead's provider"
              description="A first email to a lead whose mail is on Google or Microsoft goes out from one of your mailboxes on the same provider, which tends to land it in the inbox more often. When none of those has room left today, another mailbox sends it, so nothing waits. Follow-ups stay in the first email's thread."
              toggle={<Switch checked={s.match_provider} onChange={(on) => update({ match_provider: on })} label="Match the lead's provider" />}
            >
              {s.match_provider && missing.length > 0 && (
                <p className="text-sm text-muted-foreground">
                  {missing.length === 2
                    ? "None of your active mailboxes are on Google or Microsoft, so this changes nothing until you "
                    : `None of your active mailboxes are on ${SIGN_IN_LABEL[missing[0]]}, so leads on ${SIGN_IN_LABEL[missing[0]]} get first emails from whichever mailbox has room until you `}
                  <Link href="/app/domains" className="font-medium whitespace-nowrap text-foreground hover:underline">
                    connect one
                  </Link>
                  .
                </p>
              )}
            </Section>
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}

function Stat({ label, value }: { label: string; value: string | null }) {
  return (
    <div className="rounded-xl bg-muted px-4 py-3">
      <MicroLabel>{label}</MicroLabel>
      {value === null ? <Skeleton className="mt-1.5 h-6 w-20" /> : <p className="mt-1 text-lg font-semibold tabular-nums">{value}</p>}
    </div>
  );
}
