"use client";

import { PageBody, PageHeader } from "@/components/app/page-header";
import { EmailsView } from "@/components/app/work/emails-view";

export default function AllEmailsPage() {
  return (
    <>
      <PageHeader title="Emails" description="Outbound emails across all projects." />
      <PageBody>
        <EmailsView />
      </PageBody>
    </>
  );
}
