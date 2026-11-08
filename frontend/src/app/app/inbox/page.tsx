"use client";

import { PageBody, PageHeader, ScreenPage } from "@/components/app/page-header";
import { InboxView } from "@/components/app/work/inbox-view";

export default function AllInboxPage() {
  return (
    <ScreenPage>
      <PageHeader title="Inbox" description="Replies across all projects." />
      <PageBody className="flex min-h-0 flex-1 flex-col">
        <InboxView />
      </PageBody>
    </ScreenPage>
  );
}
