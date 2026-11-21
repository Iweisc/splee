"use client";

import { use } from "react";
import { GoogleSheetsSection } from "@/components/app/settings/google-sheets";
import { ProjectSettingsPage } from "@/components/app/settings/layout";
import { AdAudiencesSection } from "./ads-section";
import { AutomationSection } from "./automation-section";
import { BlogSection, DevSection, HashnodeSection } from "./blog-sections";
import { BookingSection } from "./booking-section";
import { DiscordSection, TelegramSection } from "./bot-sections";
import { CalendarFeedSection } from "./calendar-section";
import { ChatSection } from "./chat-section";
import { BccSection, CrmSection } from "./crm-section";
import { FinderSection } from "./finder-section";
import { FormSection } from "./form-section";
import { DiscourseSection, StackExchangeSection } from "./forum-sections";
import { GoogleMapsSection } from "./google-maps-section";
import { MeetingsSection } from "./meetings-section";
import { MetaSection } from "./meta-section";
import { PaymentsSection } from "./payments-section";
import {
  BlueskySection,
  LemmySection,
  MastodonSection,
  NostrSection,
  RedditSection,
  ThreadsSection,
  TumblrSection,
  XSection,
  YouTubeSection,
} from "./social-sections";
import { TwilioSection } from "./twilio-section";
import { UnipileSection } from "./unipile-section";
import { VerifierSection } from "./verifier-section";
import { VisitorsSection } from "./visitors-section";
import { WebhooksSection } from "./webhooks-section";

export default function IntegrationsPage({ params }: PageProps<"/app/p/[projectId]/integrations">) {
  const { projectId } = use(params);
  return (
    <ProjectSettingsPage
      title="Integrations"
      description="Get told the moment a lead replies, in your own tools."
    >
      {(s, update) => (
        <>
          <ChatSection
            projectId={projectId}
            weekly={s.chat_weekly_report}
            setWeekly={(chat_weekly_report) => update({ chat_weekly_report })}
            briefs={s.meeting_briefs}
            setBriefs={(meeting_briefs) => update({ meeting_briefs })}
          />

          <CrmSection projectId={projectId} settings={s} />
          <GoogleSheetsSection projectId={projectId} />
          <PaymentsSection projectId={projectId} />
          <MeetingsSection projectId={projectId} />
          <AdAudiencesSection projectId={projectId} />
          <BccSection settings={s} update={update} />
          <VerifierSection projectId={projectId} settings={s} />
          <FinderSection projectId={projectId} />
          <GoogleMapsSection />
          <UnipileSection projectId={projectId} settings={s} />
          <TwilioSection settings={s} />
          <XSection projectId={projectId} settings={s} />
          <RedditSection projectId={projectId} settings={s} />
          <BlueskySection projectId={projectId} settings={s} />
          <MastodonSection projectId={projectId} settings={s} />
          <ThreadsSection projectId={projectId} settings={s} />
          <YouTubeSection projectId={projectId} settings={s} />
          <StackExchangeSection projectId={projectId} settings={s} />
          <DevSection projectId={projectId} settings={s} />
          <DiscourseSection projectId={projectId} />
          <BlogSection projectId={projectId} />
          <LemmySection projectId={projectId} settings={s} />
          <NostrSection projectId={projectId} settings={s} />
          <TumblrSection projectId={projectId} settings={s} />
          <HashnodeSection projectId={projectId} settings={s} />
          <DiscordSection projectId={projectId} settings={s} />
          <TelegramSection projectId={projectId} settings={s} />
          <MetaSection projectId={projectId} settings={s} />

          <AutomationSection projectId={projectId} />
          <WebhooksSection projectId={projectId} settings={s} update={update} />
          <FormSection projectId={projectId} token={s.form_token} />
          <VisitorsSection projectId={projectId} />
          <BookingSection settings={s} />
          <CalendarFeedSection settings={s} />
        </>
      )}
    </ProjectSettingsPage>
  );
}
