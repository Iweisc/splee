"use client";

import Link from "next/link";
import useSWR from "swr";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { MicroLabel } from "@/components/app/work/states";
import { NumberInput } from "@/components/ui/form";
import { Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { todayLimit } from "@/lib/mailbox";
import type { Mailbox } from "@/lib/types";
import { num } from "@/lib/utils";

const MIN = 1;
const MAX = 5000;
// Most projects sit well under this, so the slider stays precise. Larger limits go in the number box.
const SLIDER_MAX = 1000;

export default function DailyLimitPage() {
  const { data: mailboxes, error: mailboxesError } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const capacity = mailboxes?.filter((m) => m.status === "active").reduce((sum, m) => sum + todayLimit(m), 0);

  return (
    <ProjectSettingsPage
      title="Daily limit"
      description="How many emails this project may send per day."
      problem={(s) => (s.daily_email_limit < MIN || s.daily_email_limit > MAX ? `Daily limit must be between ${MIN} and ${num(MAX)}` : null)}
    >
      {(s, update) => (
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
            <Stat label="Your active mailboxes allow" value={capacity !== undefined ? `${num(capacity)}/day` : mailboxesError ? "Couldn't load" : null} />
          </div>
          {capacity === 0 ? (
            <p className="mt-3 text-sm text-muted-foreground">
              Nothing sends until you{" "}
              <Link href="/app/domains" className="font-medium text-foreground hover:underline">
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
      )}
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
