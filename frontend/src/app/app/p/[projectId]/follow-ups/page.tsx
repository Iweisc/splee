"use client";

import Link from "next/link";
import useSWR from "swr";
import { Plus, Smartphone, Trash2 } from "lucide-react";
import { AddStepButtons, delayProblem, FollowUpSteps, MAX_DELAY_DAYS, MAX_STEPS } from "@/components/app/settings/follow-up-steps";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { RepliesSection } from "@/components/app/settings/replies-section";
import { Button } from "@/components/ui/button";
import { NumberInput, Select, Switch, Textarea } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { FollowUp, ProjectSettings, StepStats, TextStep, TwilioStatus } from "@/lib/types";

export default function FollowUpsPage() {
  return (
    <ProjectSettingsPage
      title="Follow-ups"
      description="If a lead doesn't reply, the agent writes a short follow-up in the same email thread, or someone on your team calls them. A reply of any kind stops the sequence."
      problem={(s) =>
        delayProblem([...s.follow_ups, ...s.text_steps]) ??
        (!Number.isInteger(s.daily_text_limit) || s.daily_text_limit < 1 || s.daily_text_limit > 1000 ? "Texts a day must be 1 to 1,000" : null)
      }
    >
      {(s, update) => {
        const steps = s.follow_ups;
        const set = (next: FollowUp[]) => update({ follow_ups: next });

        return (
          <>
            <Section
              title="Sequence"
              description={`Up to ${MAX_STEPS} follow-ups and calls. Each delay counts from the step before. A call waits on the Calls page until someone logs or skips it. Every campaign follows these unless it has its own, set on its page.`}
              actions={
                steps.length > 0 && (
                  <div className="flex flex-wrap gap-2">
                    <AddStepButtons steps={steps} onChange={set} />
                  </div>
                )
              }
            >
              <FollowUpSteps steps={steps} onChange={set} />
              {/* A line above, so it reads as a setting for the whole sequence rather than part of the last step. */}
              <div className="mt-5 flex items-start justify-between gap-4 border-t border-border pt-5">
                <div className="text-sm">
                  <p className="font-medium">Stop the whole company on a reply</p>
                  <p className="mt-0.5 text-muted-foreground">
                    When a lead replies, your other leads at their company get nothing more from the sequence, including a first email. Out-of-office
                    replies don&apos;t count.
                  </p>
                </div>
                <Switch
                  checked={s.stop_company_on_reply}
                  onChange={(on) => update({ stop_company_on_reply: on })}
                  label="Stop the whole company on a reply"
                />
              </div>
            </Section>
            <TextSteps settings={s} update={update} />
            <RepliesSection<StepStats>
              stat="steps"
              title="Replies by step"
              column="Email"
              label={(r) => (r.step === 0 ? "First email" : `Follow-up ${r.step}`)}
            />
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}

/** Texts sent alongside the emails through the organization's Twilio account. A reply on any channel stops them too. */
function TextSteps({ settings: s, update }: { settings: ProjectSettings; update: (patch: Partial<ProjectSettings>) => void }) {
  const { data: twilio } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const steps = s.text_steps;
  const set = (text_steps: TextStep[]) => update({ text_steps });
  const patch = (i: number, p: Partial<TextStep>) => set(steps.map((t, j) => (j === i ? { ...t, ...p } : t)));
  const c = twilio?.connection;
  const channel = c?.sms_from || !c?.whatsapp_from ? "sms" : "whatsapp";
  const add = (
    <Button
      variant="outline"
      size="sm"
      onClick={() => set([...steps, { channel, delay_days: steps.length ? 3 : 2, instructions: "" }])}
      disabled={steps.length >= MAX_STEPS}
    >
      <Plus className="size-4" /> Add text message
    </Button>
  );
  const integrations = (
    <Link href={`/app/p/${s.project_id}/integrations`} className="font-medium text-foreground underline underline-offset-2">
      Integrations page
    </Link>
  );
  return (
    <Section
      title="Text messages"
      description="SMS or WhatsApp texts to leads with a phone number. The first goes its delay after their first email, each later one its delay after the text before it."
      actions={steps.length > 0 && add}
    >
      {twilio && !c && (
        <p className="mb-4 rounded-lg bg-amber-50 px-3 py-2 text-sm text-pretty text-amber-800">Connect Twilio on the {integrations} to send these.</p>
      )}
      {steps.length === 0 ? (
        <EmptyState icon={<Smartphone className="size-6" />} title="No text messages" action={add} className="py-8">
          Leads only get emails.
        </EmptyState>
      ) : (
        <ol className="space-y-3">
          {steps.map((t, i) => (
            <li key={i}>
              <Card className="p-4 shadow-none ring-1 ring-border">
                <div className="flex flex-wrap items-center gap-3">
                  <span className="font-medium">Text {i + 1}</span>
                  <Select
                    value={t.channel}
                    onChange={(e) => patch(i, { channel: e.target.value as TextStep["channel"] })}
                    className="h-8 w-auto"
                    aria-label={`Channel for text ${i + 1}`}
                  >
                    <option value="sms">SMS</option>
                    <option value="whatsapp">WhatsApp</option>
                  </Select>
                  <span className="flex items-center gap-2 text-sm text-muted-foreground">
                    after
                    <NumberInput
                      min={1}
                      max={MAX_DELAY_DAYS}
                      value={t.delay_days}
                      onValueChange={(delay_days) => patch(i, { delay_days })}
                      className="h-8 w-16 text-center tabular-nums"
                      aria-label={`Delay for text ${i + 1} in days`}
                    />
                    {t.delay_days === 1 ? "day" : "days"}
                  </span>
                  <Button variant="ghost" size="icon" className="ml-auto" onClick={() => set(steps.filter((_, j) => j !== i))} aria-label="Remove text">
                    <Trash2 className="size-4" />
                  </Button>
                </div>
                {t.channel === "sms" ? (
                  <Textarea
                    value={t.instructions}
                    onChange={(e) => patch(i, { instructions: e.target.value })}
                    placeholder="Optional: what this text should say. Texts stay under 320 characters and name you."
                    className="mt-3 min-h-[calc(2lh+1.125rem)] bg-card field-sizing-content"
                  />
                ) : (
                  <p className="mt-3 text-sm text-pretty text-muted-foreground">
                    {c?.whatsapp_template_sid
                      ? "Sends your approved WhatsApp template, filled in with the lead's details."
                      : <>Sends your approved WhatsApp template once you pick one on the {integrations}.</>}
                  </p>
                )}
              </Card>
            </li>
          ))}
        </ol>
      )}
      <div className="mt-4 flex flex-wrap items-center justify-between gap-3 rounded-lg border border-border px-3 py-2.5">
        <span>
          <span className="block text-sm font-medium">Texts a day</span>
          <span className="block text-xs text-pretty text-muted-foreground">Replies you send count too, but they never wait.</span>
        </span>
        <NumberInput
          min={1}
          max={1000}
          value={s.daily_text_limit}
          onValueChange={(daily_text_limit) => update({ daily_text_limit })}
          className="h-8 w-20 text-center tabular-nums"
          aria-label="Texts a day"
        />
      </div>
    </Section>
  );
}
