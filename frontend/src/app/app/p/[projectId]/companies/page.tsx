"use client";

import { use } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { DirectoryView } from "@/components/app/work/directory-view";

export default function CompaniesPage({ params }: PageProps<"/app/p/[projectId]/companies">) {
  const { projectId } = use(params);
  return (
    <>
      <PageHeader
        title="Companies"
        description="Businesses with their own website, from Overture Maps' open data. Add them to a campaign and the agent finds their decision makers."
      />
      <PageBody>
        <DirectoryView projectId={projectId} />
      </PageBody>
    </>
  );
}
