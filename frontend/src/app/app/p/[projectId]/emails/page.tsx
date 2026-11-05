"use client";

import { use } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { EmailsView } from "@/components/app/work/emails-view";

export default function ProjectEmailsPage({ params }: PageProps<"/app/p/[projectId]/emails">) {
  const { projectId } = use(params);
  return (
    <>
      <PageHeader title="Emails" description="Review drafts before they go out. Approved emails are sent from your mailboxes inside the send window." />
      <PageBody>
        <EmailsView projectId={projectId} />
      </PageBody>
    </>
  );
}
