"use client";

import { use } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { InboxView } from "@/components/app/work/inbox-view";

export default function ProjectInboxPage({ params }: PageProps<"/app/p/[projectId]/inbox">) {
  const { projectId } = use(params);
  return (
    <>
      <PageHeader title="Inbox" description="Replies are classified automatically. Hot leads and questions get a reply draft with your booking link." />
      <PageBody>
        <InboxView projectId={projectId} />
      </PageBody>
    </>
  );
}
