"use client";

import { use, useState } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { DirectoryView } from "@/components/app/work/directory-view";
import { HiringSignals } from "@/components/app/work/hiring";
import { TechSignals } from "@/components/app/work/tech";
import { VisitorsView } from "@/components/app/work/visitors";
import { Tabs } from "@/components/ui/misc";

const TABS = ["database", "hiring", "tech", "visitors"] as const;
type Tab = (typeof TABS)[number];

const DESCRIPTIONS: Record<Tab, string> = {
  database: "Businesses with their own website, from Overture Maps' open data. Add them to a campaign and the agent finds their decision makers.",
  hiring: "Your companies that are hiring for the roles you watch. A job post makes a warm opener, and the agent uses it in their emails.",
  tech: "The tools your companies' websites show they use. The ones you look for come first, and the agent can open their emails with them.",
  visitors: "Companies that visited your website, the pages they read and how they found you. Add the ones you aren't talking to yet to a campaign.",
};

export default function CompaniesPage({ params, searchParams }: PageProps<"/app/p/[projectId]/companies">) {
  const { projectId } = use(params);
  // ?tab=visitors is where hot-visitor alerts link to.
  const { tab: linked } = use(searchParams);
  const [tab, setTab] = useState<Tab>(TABS.find((t) => t === linked) ?? "database");
  return (
    <>
      <PageHeader title="Companies" description={DESCRIPTIONS[tab]} />
      <PageBody>
        {/* Not a flex column with a gap: the country loader above the search keeps its own margin. */}
        <div className="mb-4">
          <Tabs
            value={tab}
            onChange={setTab}
            options={[
              {
                value: "database",
                // Four tabs don't fit a phone with the full name.
                label: (
                  <>
                    <span className="sm:hidden">Database</span>
                    <span className="max-sm:hidden">Company database</span>
                  </>
                ),
              },
              { value: "hiring", label: "Hiring" },
              { value: "tech", label: "Tech stack" },
              { value: "visitors", label: "Visitors" },
            ]}
          />
        </div>
        {tab === "database" && <DirectoryView projectId={projectId} />}
        {tab === "hiring" && <HiringSignals projectId={projectId} />}
        {tab === "tech" && <TechSignals projectId={projectId} />}
        {tab === "visitors" && <VisitorsView projectId={projectId} />}
      </PageBody>
    </>
  );
}
