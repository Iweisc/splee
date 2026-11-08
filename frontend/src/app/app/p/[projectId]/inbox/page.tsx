"use client";

import { use } from "react";
import { PageBody, PageHeader, ScreenPage } from "@/components/app/page-header";
import { InboxView } from "@/components/app/work/inbox-view";

export default function ProjectInboxPage({ params, searchParams }: PageProps<"/app/p/[projectId]/inbox">) {
  const { projectId } = use(params);
  // Hot-lead emails, Slack alerts and webhooks link here with ?lead= to open that conversation.
  const { lead } = use(searchParams);
  return (
    <ScreenPage>
      <PageHeader title="Inbox" description="Replies are classified automatically. Hot leads and questions get a reply draft with your booking link." />
      <PageBody className="flex min-h-0 flex-1 flex-col">
        <InboxView projectId={projectId} initialLead={typeof lead === "string" ? lead : undefined} />
      </PageBody>
    </ScreenPage>
  );
}
