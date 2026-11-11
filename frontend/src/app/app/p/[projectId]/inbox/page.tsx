"use client";

import { use } from "react";
import { useRouter } from "next/navigation";
import { PageBody, PageHeader, ScreenPage } from "@/components/app/page-header";
import { ChatsView } from "@/components/app/work/chats-view";
import { InboxView } from "@/components/app/work/inbox-view";
import { Tabs } from "@/components/ui/misc";

const DESCRIPTIONS = {
  email: "Replies are classified automatically. Hot leads and questions get a reply draft with your booking link.",
  chats: "Conversations from the chat on your website. Take one over to answer it yourself.",
};

export default function ProjectInboxPage({ params, searchParams }: PageProps<"/app/p/[projectId]/inbox">) {
  const { projectId } = use(params);
  const router = useRouter();
  // Hot-lead emails, Slack alerts and webhooks link here with ?lead= to open that conversation, and chat alerts with
  // ?tab=chats&chat=.
  const { lead, tab: linked, chat } = use(searchParams);
  const tab = linked === "chats" ? "chats" : "email";
  return (
    <ScreenPage>
      <PageHeader title="Inbox" description={DESCRIPTIONS[tab]} />
      <PageBody className="flex min-h-0 flex-1 flex-col gap-4">
        <Tabs
          value={tab}
          onChange={(t) => router.replace(t === "chats" ? `/app/p/${projectId}/inbox?tab=chats` : `/app/p/${projectId}/inbox`)}
          options={[
            { value: "email", label: "Email" },
            { value: "chats", label: "Chats" },
          ]}
          className="self-start"
        />
        {tab === "chats" ? (
          <ChatsView projectId={projectId} initialChat={typeof chat === "string" ? chat : undefined} />
        ) : (
          <InboxView projectId={projectId} initialLead={typeof lead === "string" ? lead : undefined} />
        )}
      </PageBody>
    </ScreenPage>
  );
}
