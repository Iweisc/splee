"use client";

import { use, useState } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CompaniesTable } from "@/components/app/work/companies-table";
import { DirectoryView } from "@/components/app/work/directory-view";
import { HiringSignals } from "@/components/app/work/hiring";
import { LocalBusinesses } from "@/components/app/work/local-businesses";
import { TechSignals } from "@/components/app/work/tech";
import { VisitorsView } from "@/components/app/work/visitors";
import { Tabs } from "@/components/ui/misc";

const TABS = ["yours", "database", "local", "hiring", "tech", "visitors"] as const;
type Tab = (typeof TABS)[number];

const DESCRIPTIONS: Record<Tab, string> = {
  yours: "Every company in your campaigns, scored by how well it fits your targeting and what it did lately. Open a score to see why.",
  database: "Businesses with their own website, from Overture Maps' open data. Add them to a campaign and the agent finds their decision makers.",
  local: "Dentists, gyms, restaurants and other local businesses from Google Maps, through your own key. Add them to a campaign and Splee finds an email on each website.",
  hiring: "Your companies that are hiring for the roles you watch. A job post makes a warm opener, and the agent uses it in their emails.",
  tech: "The tools your companies' websites show they use. The ones you look for come first, and the agent can open their emails with them.",
  visitors: "Companies that visited your website, the pages they read and how they found you. Add the ones you aren't talking to yet to a campaign.",
};

export default function CompaniesPage({ params, searchParams }: PageProps<"/app/p/[projectId]/companies">) {
  const { projectId } = use(params);
  // ?tab=visitors is where hot-visitor alerts link to.
  const { tab: linked } = use(searchParams);
  const [tab, setTab] = useState<Tab>(TABS.find((t) => t === linked) ?? "yours");
  return (
    <>
      <PageHeader title="Companies" description={DESCRIPTIONS[tab]} />
      <PageBody>
        {/* Not a flex column with a gap: the country loader above the search keeps its own margin. */}
        <div className="mb-4">
          <Tabs
            value={tab}
            onChange={setTab}
            // Six short labels only fit a 390px phone with tighter padding.
            className="max-sm:[&>button]:px-2"
            options={[
              // Six tabs don't fit a phone with their full names.
              { value: "yours", label: <ShortLabel short="Yours" full="Your companies" /> },
              { value: "database", label: <ShortLabel short="Database" full="Company database" /> },
              { value: "local", label: <ShortLabel short="Local" full="Local businesses" /> },
              { value: "hiring", label: "Hiring" },
              { value: "tech", label: <ShortLabel short="Tech" full="Tech stack" /> },
              { value: "visitors", label: "Visitors" },
            ]}
          />
        </div>
        {tab === "yours" && <CompaniesTable projectId={projectId} />}
        {tab === "database" && <DirectoryView projectId={projectId} />}
        {tab === "local" && <LocalBusinesses projectId={projectId} />}
        {tab === "hiring" && <HiringSignals projectId={projectId} />}
        {tab === "tech" && <TechSignals projectId={projectId} />}
        {tab === "visitors" && <VisitorsView projectId={projectId} />}
      </PageBody>
    </>
  );
}

function ShortLabel({ short, full }: { short: string; full: string }) {
  return (
    <>
      <span className="sm:hidden">{short}</span>
      <span className="max-sm:hidden">{full}</span>
    </>
  );
}
