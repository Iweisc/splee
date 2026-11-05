"use client";

import { PageBody, PageHeader } from "@/components/app/page-header";
import { InboxView } from "@/components/app/work/inbox-view";

export default function AllInboxPage() {
  return (
    <>
      <PageHeader title="Inbox" description="Replies across all projects." />
      <PageBody>
        <InboxView />
      </PageBody>
    </>
  );
}
