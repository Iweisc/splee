"use client";

import { use, useState } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { DirectoryView } from "@/components/app/work/directory-view";
import { HiringSignals } from "@/components/app/work/hiring";
import { TechSignals } from "@/components/app/work/tech";
import { Tabs } from "@/components/ui/misc";

type Tab = "database" | "hiring" | "tech";

const DESCRIPTIONS: Record<Tab, string> = {
  database: "Businesses with their own website, from Overture Maps' open data. Add them to a campaign and the agent finds their decision makers.",
  hiring: "Your companies that are hiring for the roles you watch. A job post makes a warm opener, and the agent uses it in their emails.",
  tech: "The tools your companies' websites show they use. The ones you look for come first, and the agent can open their emails with them.",
};

export default function CompaniesPage({ params }: PageProps<"/app/p/[projectId]/companies">) {
  const { projectId } = use(params);
  const [tab, setTab] = useState<Tab>("database");
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
              { value: "database", label: "Company database" },
              { value: "hiring", label: "Hiring" },
              { value: "tech", label: "Tech stack" },
            ]}
          />
        </div>
        {tab === "database" && <DirectoryView projectId={projectId} />}
        {tab === "hiring" && <HiringSignals projectId={projectId} />}
        {tab === "tech" && <TechSignals projectId={projectId} />}
      </PageBody>
    </>
  );
}
