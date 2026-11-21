"use client";

import { CalendarClock } from "lucide-react";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { useOrigin } from "@/hooks/use-origin";
import type { ProjectSettings } from "@/lib/types";

export function CalendarFeedSection({ settings: s }: { settings: ProjectSettings }) {
  const origin = useOrigin();
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <CalendarClock className="size-4" /> Reminders in your calendar
        </span>
      }
      description="Inbox reminders become calendar events with the lead's notes and a link."
    >
      <div className="flex flex-col gap-1.5">
        <span className="text-sm font-medium">Calendar feed URL</span>
        <CopyField value={`${origin}/api/calendar/${s.calendar_token}.ics`} />
        <p className="text-xs text-pretty text-muted-foreground">
          Anyone with this URL sees your reminders. Google Calendar: Other calendars → + → From URL. Outlook: Add calendar → Subscribe
          from web. Apple Calendar: File → New Calendar Subscription.
        </p>
      </div>
    </Section>
  );
}
