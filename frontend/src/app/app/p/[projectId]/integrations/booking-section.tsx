"use client";

import { CalendarCheck } from "lucide-react";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { useOrigin } from "@/hooks/use-origin";
import type { ProjectSettings } from "@/lib/types";
import { AppSteps } from "./shared";

const BOOKING_APPS = [
  ["Cal.com", "Settings → Developer → Webhooks → New, paste this URL and pick Booking created, rescheduled and cancelled."],
  ["Calendly", <>subscribe it to <code>invitee.created</code> and <code>invitee.canceled</code> with Calendly&apos;s webhook API.</>],
  ["SavvyCal", "Settings → Integrations, then + next to Webhooks."],
  ["Zcal", "Integrations, then Connect next to Webhooks, and paste this URL as the Subscriber URL."],
  ["TidyCal", "Integrations → Automations → Add webhook, with booking created, approved, rescheduled and cancelled."],
  ["OnceHub", "Account Integrations → APIs & Webhooks → Create webhook, with booking scheduled, rescheduled and canceled."],
  ["Appointlet", "Settings → Webhooks → Add Webhook."],
  ["Chili Piper", "Command Center → Integrations → Create Custom Webhook, once each for New Meeting, Meeting Update and Canceled Meeting, then Enabled."],
] as const;

export function BookingSection({ settings: s }: { settings: ProjectSettings }) {
  const origin = useOrigin();
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <CalendarCheck className="size-4" /> Booking tools
        </span>
      }
      description="When a lead books a call, Splee marks them Meeting booked and stops emailing them. If they cancel, they go back to Hot. Your chat channel and webhook hear about both."
    >
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="flex flex-col gap-1.5">
        <span className="text-sm font-medium">Booking webhook URL</span>
        <CopyField value={`${origin}/api/bookings/${s.booking_token}`} />
        <p className="text-xs text-pretty text-muted-foreground">
          Splee finds the lead by email and shows the call&apos;s time on it. Anything else, like a Zapier step, can POST{" "}
          <code>{`{"email": "...", "start_time": "..."}`}</code>.
        </p>
        <AppSteps steps={BOOKING_APPS} />
      </div>
    </Section>
  );
}
