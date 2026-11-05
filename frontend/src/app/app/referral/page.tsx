"use client";

import useSWR from "swr";
import { Gift } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyField } from "@/components/app/settings/copy-button";
import { LoadError, MicroLabel, Section, SettingsColumn } from "@/components/app/settings/layout";
import { Badge, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { Referral } from "@/lib/types";
import { money, num, timeAgo } from "@/lib/utils";

// Mirrors billing::REFERRAL_BONUS_MICROS and REFERRAL_QUALIFYING_SPEND_MICROS on the backend.
const REWARD_MICROS = 30_000_000;
const QUALIFYING_SPEND_MICROS = 10_000_000;

const STEPS = [
  "Share your link with a team that does outbound.",
  "They sign up and get the usual signup credits.",
  `Once they've spent ${money(QUALIFYING_SPEND_MICROS, { cents: false })} on sent emails, ${money(REWARD_MICROS, { cents: false })} lands in your balance.`,
];

export default function ReferralPage() {
  const { data, error } = useSWR<Referral>("/referral", fetcher);
  const rewarded = data?.referred.filter((r) => r.rewarded).length ?? 0;

  return (
    <>
      <PageHeader
        title="Referral"
        description={`Get ${money(REWARD_MICROS, { cents: false })} in credits for every team you bring to Splee.`}
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="your referral link" />
          ) : (
            <>
              <Section title="Your link" description="Anyone who signs up through it is linked to your organization.">
                {data ? <CopyField value={data.link} /> : <Skeleton className="h-10 w-full" />}
                <ol className="mt-5 grid gap-3 sm:grid-cols-3">
                  {STEPS.map((step, i) => (
                    <li key={i} className="rounded-xl bg-muted px-4 py-3 text-sm">
                      <MicroLabel>Step {i + 1}</MicroLabel>
                      <p className="mt-1">{step}</p>
                    </li>
                  ))}
                </ol>
              </Section>

              <Section
                title="Referred teams"
                description={data ? `${num(data.referred.length)} signed up, ${num(rewarded)} rewarded.` : undefined}
                actions={
                  <div className="text-right">
                    <MicroLabel>Earned</MicroLabel>
                    {data ? (
                      <p className="text-xl font-semibold text-brand-700 tabular-nums">{money(data.earned_micros)}</p>
                    ) : (
                      <Skeleton className="mt-1 h-7 w-16" />
                    )}
                  </div>
                }
              >
                {!data ? (
                  <Skeleton className="h-16 w-full" />
                ) : data.referred.length === 0 ? (
                  <EmptyState icon={<Gift className="size-6" />} title="No referrals yet" className="py-6">
                    Teams you refer show up here once they sign up.
                  </EmptyState>
                ) : (
                  <ul className="divide-y divide-border rounded-xl ring-1 ring-border">
                    {data.referred.map((r, i) => (
                      <li key={i} className="flex items-center gap-3 px-4 py-3 text-sm">
                        <span className="min-w-0 flex-1 truncate font-medium">{r.org_name}</span>
                        <span className="text-xs text-muted-foreground">joined {timeAgo(r.created_at)}</span>
                        {r.rewarded ? (
                          <Badge tone="green">+{money(REWARD_MICROS, { cents: false })}</Badge>
                        ) : (
                          <Badge>Not yet at {money(QUALIFYING_SPEND_MICROS, { cents: false })}</Badge>
                        )}
                      </li>
                    ))}
                  </ul>
                )}
              </Section>
            </>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
