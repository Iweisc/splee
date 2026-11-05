"use client";

import Link from "next/link";
import { useState } from "react";
import { ArrowRight, Check } from "lucide-react";
import { LinkButton } from "@/components/ui/button";
import { Tabs } from "@/components/ui/misc";
import { PricingCard } from "@/components/marketing/pricing-card";

type Tab = "agent" | "database";

const explorerRows = [
  { label: "Search", value: "Free" },
  { label: "CSV export", value: "Free" },
  { label: "Source", value: "Live web research" },
];

export function PricingTabs() {
  const [tab, setTab] = useState<Tab>("agent");
  return (
    <>
      <div className="sticky top-20 z-30">
        <Tabs
          value={tab}
          onChange={setTab}
          options={[
            { value: "agent", label: "Outreach Agent" },
            { value: "database", label: "Database" },
          ]}
        />
      </div>
      {tab === "agent" ? (
        <section className="mt-10">
          <h2 className="mb-2 text-3xl font-bold">Outreach Agent</h2>
          <p className="mb-2 max-w-[680px] text-muted-foreground">
            An AI agent that researches your market, finds the right people, writes them personal emails and answers replies. Pay as you
            go with no subscription
          </p>
          <TryLink href="/">Try for free here</TryLink>
          <div className="max-w-[680px]">
            <PricingCard />
          </div>
        </section>
      ) : (
        <section className="mt-10">
          <h2 className="mb-2 text-3xl font-bold">Database</h2>
          <p className="mb-2 max-w-[680px] text-muted-foreground">
            Splee doesn&apos;t sell a bulk database. The Segments Explorer finds companies on demand: describe who you want, and the agent
            searches the web live and returns a list with a reason for each one
          </p>
          <TryLink href="/tools/explorer">Try the explorer</TryLink>
          <div className="max-w-[680px] rounded-2xl bg-card p-6 shadow-plaque">
            <p className="text-xl font-medium">Segments Explorer</p>
            <p className="mt-1 text-muted-foreground">Describe a segment in plain words, optionally pick a country, export the result as CSV</p>
            <dl className="mt-6 divide-y divide-border">
              {explorerRows.map((r) => (
                <div key={r.label} className="flex justify-between gap-4 py-3">
                  <dt className="text-muted-foreground">{r.label}</dt>
                  <dd className="flex items-center gap-1.5 text-right font-medium">
                    {r.value === "Free" && <Check className="size-4 text-brand-600" />}
                    {r.value}
                  </dd>
                </div>
              ))}
            </dl>
            <LinkButton href="/tools/explorer" className="mt-6">
              Open the explorer
              <ArrowRight className="size-4" />
            </LinkButton>
          </div>
        </section>
      )}
    </>
  );
}

function TryLink({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <Link href={href} className="mb-6 inline-flex items-center gap-1 text-sm font-medium text-brand-600 hover:text-brand-700">
      {children}
      <ArrowRight className="size-3.5" />
    </Link>
  );
}
