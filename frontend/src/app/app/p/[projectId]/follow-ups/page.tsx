"use client";

import Link from "next/link";
import useSWR from "swr";
import { ArrowDown, ArrowUp, Phone, Plus, Repeat, Smartphone, Trash2 } from "lucide-react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { RepliesSection } from "@/components/app/settings/replies-section";
import { Button } from "@/components/ui/button";
import { NumberInput, Select, Switch, Textarea } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { FollowUp, ProjectSettings, StepStats, TextStep, TwilioStatus } from "@/lib/types";
import { cn } from "@/lib/utils";

const MAX_STEPS = 5;
const MAX_DELAY_DAYS = 30;

export default function FollowUpsPage() {
  return (
    <ProjectSettingsPage
      title="Follow-ups"
      description="If a lead doesn't reply, the agent writes a short follow-up in the same email thread, or someone on your team calls them. A reply of any kind stops the sequence."
      problem={(s) =>
        [...s.follow_ups, ...s.text_steps].some((f) => !Number.isInteger(f.delay_days) || f.delay_days < 1 || f.delay_days > MAX_DELAY_DAYS)
          ? `Each delay must be 1 to ${MAX_DELAY_DAYS} days`
          : !Number.isInteger(s.daily_text_limit) || s.daily_text_limit < 1 || s.daily_text_limit > 1000
            ? "Texts a day must be 1 to 1,000"
            : null
      }
    >
      {(s, update) => {
        const steps = s.follow_ups;
        const set = (next: FollowUp[]) => update({ follow_ups: next });
        const patch = (i: number, p: Partial<FollowUp>) => set(steps.map((f, j) => (j === i ? { ...f, ...p } : f)));
        const move = (i: number, to: number) => {
          const next = [...steps];
          [next[i], next[to]] = [next[to]!, next[i]!];
          set(next);
        };
        const add = (
          <>
            <Button
              variant="outline"
              size="sm"
              onClick={() => set([...steps, { delay_days: steps.at(-1)?.delay_days ?? 3, enabled: true, instructions: "" }])}
              disabled={steps.length >= MAX_STEPS}
            >
              <Plus className="size-4" /> Add follow-up
            </Button>
            <Button
              variant="outline"
              size="sm"
              onClick={() => set([...steps, { kind: "call", delay_days: steps.at(-1)?.delay_days ?? 3, enabled: true, instructions: "" }])}
              disabled={steps.length >= MAX_STEPS}
            >
              <Phone className="size-4" /> Add call
            </Button>
          </>
        );

        return (
          <>
            <Section
              title="Sequence"
              description={`Up to ${MAX_STEPS} follow-ups and calls. Each delay counts from the step before. A call waits on the Calls page until someone logs or skips it.`}
              actions={steps.length > 0 && <div className="flex flex-wrap gap-2">{add}</div>}
            >
              {steps.length === 0 ? (
                <EmptyState icon={<Repeat className="size-6" />} title="No follow-ups" action={<div className="flex flex-wrap justify-center gap-2">{add}</div>} className="py-8">
                  Only the first email is sent to each lead.
                </EmptyState>
              ) : (
                <ol className="space-y-3">
                  {steps.map((f, i) => (
                    <li key={i}>
                      <Card className={cn("p-4 shadow-none ring-1 ring-border", !f.enabled && "bg-muted")}>
                        <div className="flex flex-wrap items-center gap-3">
                          <Switch checked={f.enabled} onChange={(enabled) => patch(i, { enabled })} label={`Enable step ${i + 1}`} />
                          {f.kind === "call" ? (
                            <span className="flex items-center gap-1.5 font-medium">
                              <Phone className="size-4 text-muted-foreground" /> Call
                            </span>
                          ) : (
                            <span className="font-medium">Follow-up {i + 1}</span>
                          )}
                          <span className="flex items-center gap-2 text-sm text-muted-foreground">
                            after
                            <NumberInput
                              min={1}
                              max={MAX_DELAY_DAYS}
                              value={f.delay_days}
                              onValueChange={(delay_days) => patch(i, { delay_days })}
                              className="h-8 w-16 text-center tabular-nums"
                              aria-label={`Delay for step ${i + 1} in days`}
                            />
                            {f.delay_days === 1 ? "day" : "days"}
                          </span>
                          <div className="ml-auto flex items-center gap-1">
                            <Button variant="ghost" size="icon" onClick={() => move(i, i - 1)} disabled={i === 0} aria-label="Move up">
                              <ArrowUp className="size-4" />
                            </Button>
                            <Button variant="ghost" size="icon" onClick={() => move(i, i + 1)} disabled={i === steps.length - 1} aria-label="Move down">
                              <ArrowDown className="size-4" />
                            </Button>
                            <Button variant="ghost" size="icon" onClick={() => set(steps.filter((_, j) => j !== i))} aria-label="Remove step">
                              <Trash2 className="size-4" />
                            </Button>
                          </div>
                        </div>
                        {/* At least two lines, and taller when the hint or the instructions need it, as the hint does on phones. 1.125rem is the padding plus border. */}
                        <Textarea
                          value={f.instructions}
                          onChange={(e) => patch(i, { instructions: e.target.value })}
                          placeholder={
                            f.kind === "call"
                              ? "Optional: what to talk about on the call, e.g. ask how they book appointments today."
                              : "Optional: what this follow-up should say, e.g. share a case study, or ask if someone else handles this."
                          }
                          className="mt-3 min-h-[calc(2lh+1.125rem)] bg-card field-sizing-content"
                        />
                      </Card>
                    </li>
                  ))}
                </ol>
              )}
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
