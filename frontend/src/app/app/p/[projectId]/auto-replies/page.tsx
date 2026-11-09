"use client";

import { isUrl } from "@/components/app/settings/is-url";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { OptionCards } from "@/components/app/settings/option-cards";
import { Field, Input, Textarea } from "@/components/ui/form";
import type { ProjectSettings } from "@/lib/types";

const MODES: { value: ProjectSettings["auto_reply_mode"]; label: string; description: string }[] = [
  { value: "off", label: "Off", description: "The agent classifies replies but never answers. You reply from the inbox." },
  { value: "approve", label: "Draft for approval", description: "The agent drafts an answer and waits in the inbox until you approve it." },
  { value: "auto", label: "Send automatically", description: "The agent answers interested leads and questions on its own, within your send schedule." },
];

export default function AutoRepliesPage() {
  return (
    <ProjectSettingsPage
      title="Auto-replies"
      description="What happens when a lead replies with interest or a question. Not-interested, out-of-office and unsubscribe replies are handled without an answer."
      problem={(s) => (s.booking_link && !isUrl(s.booking_link) ? "Booking link must be a full URL starting with https://" : null)}
    >
      {(s, update) => (
        <>
          <Section title="Mode" description="How much the agent does on its own when someone replies.">
            <OptionCards
              name="auto_reply_mode"
              value={s.auto_reply_mode}
              onChange={(auto_reply_mode) => update({ auto_reply_mode })}
              options={MODES}
              className="@2xl:grid-cols-3"
            />
          </Section>

          <Section title="Booking link" description="The agent includes this link when a lead wants to meet, so they can pick a time themselves.">
            <Field label="URL" error={s.booking_link && !isUrl(s.booking_link) ? "Enter a full URL, e.g. https://cal.com/you/intro" : null}>
              <Input
                type="url"
                value={s.booking_link}
                onChange={(e) => update({ booking_link: e.target.value.trim() })}
                placeholder="https://cal.com/you/intro"
              />
            </Field>
          </Section>

          <Section title="Reply instructions" description="Facts and rules for answers, e.g. pricing you're happy to share or questions to hand back to you.">
            <Textarea
              value={s.auto_reply_instructions}
              onChange={(e) => update({ auto_reply_instructions: e.target.value })}
              placeholder={"e.g. Plans start at $99/month.\nIf they ask about integrations, say a teammate will follow up."}
              rows={5}
            />
          </Section>
        </>
      )}
    </ProjectSettingsPage>
  );
}
