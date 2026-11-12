"use client";

import { use, useState } from "react";
import { Upload } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { JobChangesView } from "@/components/app/work/job-changes";
import { LeadModal } from "@/components/app/work/lead-modal";
import { LeadsView } from "@/components/app/work/leads-view";
import { LinkButton } from "@/components/ui/button";
import { Tabs } from "@/components/ui/misc";

const TABS = ["all", "job-changes"] as const;
type Tab = (typeof TABS)[number];

const DESCRIPTIONS: Record<Tab, string> = {
  all: "Decision makers the agent found, plus everyone you imported.",
  "job-changes": "People who bought, booked a meeting or replied with interest, and the new companies they moved to. Each move becomes a lead there.",
};

export default function LeadsPage({ params, searchParams }: PageProps<"/app/p/[projectId]/leads">) {
  const { projectId } = use(params);
  // Job change alerts link to ?lead= for the new lead, or to ?tab=job-changes.
  const { tab: linked, lead: linkedLead } = use(searchParams);
  const [tab, setTab] = useState<Tab>(TABS.find((t) => t === linked) ?? "all");
  const [openLead, setOpenLead] = useState(typeof linkedLead === "string" ? linkedLead : null);
  return (
    <>
      <PageHeader
        title="Leads"
        description={DESCRIPTIONS[tab]}
        actions={
          <LinkButton href={`/app/p/${projectId}/import`} variant="outline">
            <Upload className="size-4" /> Import CSV
          </LinkButton>
        }
      />
      <PageBody>
        <div className="mb-4">
          <Tabs
            value={tab}
            onChange={setTab}
            options={[
              { value: "all", label: "All leads" },
              { value: "job-changes", label: "Job changes" },
            ]}
          />
        </div>
        {tab === "all" ? <LeadsView projectId={projectId} /> : <JobChangesView projectId={projectId} />}
        <LeadModal leadId={openLead} onClose={() => setOpenLead(null)} />
      </PageBody>
    </>
  );
}
