"use client";

import { useState } from "react";
import { ClipboardList } from "lucide-react";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { useOrigin } from "@/hooks/use-origin";
import { AppSteps } from "./shared";

const FORM_APPS = [
  ["Tally", "Integrations → Webhooks."],
  ["Typeform", "Connect → Webhooks."],
  ["Fillout", "Integrate → Webhook, in Simple mode."],
  ["Jotform", "Settings → Integrations → WebHooks."],
  ["Paperform", "After Submission → Integrations & Webhooks → Add Webhook, on New Submission."],
  ["Formbricks", "Configuration → Integrations → Manage Webhooks, on Response Finished."],
  ["Netlify", "Forms → Submission notifications → Add notification → HTTP POST request."],
  ["Webflow", "Site settings → Integrations → Webhooks, with Form submission."],
] as const;

/** The form webhook URL, with the campaign new leads join in its query. */
export function FormSection({ projectId, token }: { projectId: string; token: string }) {
  const origin = useOrigin();
  const [segmentId, setSegmentId] = useState("");
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <ClipboardList className="size-4" /> Website forms
        </span>
      }
      description="Whoever fills in one of your forms becomes a lead and pings your chat."
    >
      <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} />
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-4 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Form webhook URL</span>
        <CopyField value={`${origin}/api/forms/${token}${segmentId ? `?campaign=${segmentId}` : ""}`} />
        <p className="text-xs text-pretty text-muted-foreground">
          Anything that can POST <code>{`{"email": "...", "name": "..."}`}</code> works too. Answers that aren&apos;t a name, email,
          company, title, website or LinkedIn go in the lead&apos;s notes.
        </p>
        <AppSteps steps={FORM_APPS} />
      </div>
    </Section>
  );
}
