"use client";

import { use } from "react";
import { Upload } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LeadsView } from "@/components/app/work/leads-view";
import { LinkButton } from "@/components/ui/button";

export default function LeadsPage({ params }: PageProps<"/app/p/[projectId]/leads">) {
  const { projectId } = use(params);
  return (
    <>
      <PageHeader
        title="Leads"
        description="Decision makers the agent found, plus everyone you imported."
        actions={
          <LinkButton href={`/app/p/${projectId}/import`} variant="outline">
            <Upload className="size-4" /> Import CSV
          </LinkButton>
        }
      />
      <PageBody>
        <LeadsView projectId={projectId} />
      </PageBody>
    </>
  );
}
