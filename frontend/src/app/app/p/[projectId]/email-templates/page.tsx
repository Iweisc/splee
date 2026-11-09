"use client";

import { useState } from "react";
import { useParams } from "next/navigation";
import { toast } from "sonner";
import useSWR from "swr";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { OptionCards } from "@/components/app/settings/option-cards";
import { isActive, RunProgress, useProjectRuns } from "@/components/app/work/run-progress";
import { MicroLabel, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Textarea } from "@/components/ui/form";
import { Badge, Card } from "@/components/ui/misc";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { EmailRow, Paginated, ProjectSettings } from "@/lib/types";
import { num } from "@/lib/utils";

const TONES = [
  { value: "friendly", label: "Friendly", description: "Warm and casual", greeting: "Hi Sarah,", signoff: "Best," },
  { value: "direct", label: "Direct", description: "Short, to the point", greeting: "Sarah,", signoff: "Thanks," },
  { value: "formal", label: "Formal", description: "Polite and professional", greeting: "Dear Ms. Chen,", signoff: "Kind regards," },
  { value: "playful", label: "Playful", description: "Light, a bit of humor", greeting: "Hey Sarah!", signoff: "Cheers," },
];

// Stored as the English name: the agent gets it verbatim in its writing prompt.
const LANGUAGES = [
  "English", "Spanish", "French", "German", "Italian", "Portuguese", "Dutch", "Swedish", "Norwegian", "Danish",
  "Finnish", "Polish", "Czech", "Slovak", "Hungarian", "Romanian", "Bulgarian", "Greek", "Turkish", "Russian",
  "Ukrainian", "Hebrew", "Arabic", "Hindi", "Indonesian", "Malay", "Thai", "Vietnamese", "Japanese", "Korean",
  "Chinese (Simplified)", "Chinese (Traditional)",
];

const MIN_WORDS = 30;
const MAX_WORDS = 250;

export default function EmailTemplatesPage() {
  return (
    <ProjectSettingsPage
      title="Email templates"
      description="How the agent writes. These settings shape the style of every email it writes for your leads."
      problem={(s) => (s.max_words < MIN_WORDS || s.max_words > MAX_WORDS ? `Max words must be between ${MIN_WORDS} and ${MAX_WORDS}` : null)}
    >
      {(s, update, dirty) => (
        <>
          <Section title="Sender" description="The name the agent signs with and the signature added under every email.">
            <div className="grid gap-4">
              <Field label="Sender name">
                <Input value={s.sender_name} onChange={(e) => update({ sender_name: e.target.value })} placeholder="e.g. Alex Morgan" />
              </Field>
              <Field label="Signature">
                <Textarea
                  value={s.signature}
                  onChange={(e) => update({ signature: e.target.value })}
                  placeholder={"Alex Morgan\nFounder, Acme\nacme.com"}
                  rows={3}
                />
              </Field>
            </div>
          </Section>

          <Section title="Voice" description="Tone and language for every email and follow-up. With Auto the agent picks the language for each lead.">
            <OptionCards
              name="tone"
              value={s.tone}
              onChange={(tone) => update({ tone })}
              options={TONES}
              className="@lg:grid-cols-2"
            />
            <Field label="Language" className="mt-5 max-w-xs">
              <Select value={s.email_language} onChange={(e) => update({ email_language: e.target.value })}>
                <option value="auto">Auto</option>
                {LANGUAGES.map((l) => (
                  <option key={l} value={l}>
                    {l}
                  </option>
                ))}
              </Select>
            </Field>
          </Section>

          <Section title="Length and call to action" description="The word limit applies to the email body. The call to action is the question each first email ends with.">
            <div className="grid gap-4 sm:grid-cols-[10rem_1fr]">
              <Field label="Max words">
                <NumberInput
                  min={MIN_WORDS}
                  max={MAX_WORDS}
                  value={s.max_words}
                  onValueChange={(max_words) => update({ max_words })}
                  className="tabular-nums"
                />
              </Field>
              <Field label="Call to action" hint="Leave empty to let the agent pick a low-friction question.">
                <Input value={s.cta} onChange={(e) => update({ cta: e.target.value })} placeholder="e.g. Worth a 15-minute call next week?" />
              </Field>
            </div>
          </Section>

          <Section title="Extra instructions" description="Rules the agent follows on top of everything above.">
            <Textarea
              value={s.email_instructions}
              onChange={(e) => update({ email_instructions: e.target.value })}
              placeholder={"e.g. Never mention pricing.\nMention that setup takes one day."}
              rows={4}
            />
          </Section>

          <ExampleEmail s={s} />
          <RewriteUnsent dirty={dirty} />
        </>
      )}
    </ProjectSettingsPage>
  );
}

function RewriteUnsent({ dirty }: { dirty: boolean }) {
  const { projectId } = useParams<{ projectId: string }>();
  const { runs, mutate } = useProjectRuns(projectId);
  const { data: unsent } = useSWR<Paginated<EmailRow>>(
    `/emails${qs({ project_id: projectId, status: "draft,approved,scheduled", kind: "initial,follow_up", per_page: 1 })}`,
    fetcher,
  );
  const [starting, setStarting] = useState(false);
  const [started, setStarted] = useState<string | null>(null);
  const run = runs?.find((r) => r.kind === "write_emails" && isActive(r)) ?? null;
  const count = unsent?.total ?? 0;

  // RunProgress doesn't show the run's final message, so surface "Rewrote N of M" here.
  useAgentEvents(started ? `/api/runs/${started}/events` : null, (e) => {
    if (e.kind === "done") toast.success(e.message);
    if (e.kind === "error") toast.error(e.message || "Rewrite failed");
    if (e.kind === "done" || e.kind === "error") setStarted(null);
  });

  const rewrite = async () => {
    setStarting(true);
    try {
      const { run_id } = await api<{ run_id: string }>(`/projects/${projectId}/rewrite-emails`, { method: "POST" });
      setStarted(run_id);
      await mutate();
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(false);
    }
  };

  return (
    <Section
      title="Rewrite unsent emails"
      description="Changes above apply to emails written from now on. Rewrite the drafts and scheduled emails too, so they follow the new rules. Those leads are held from sending until the rewrite finishes."
    >
      {run ? (
        <RunProgress key={run.id} run={run} />
      ) : (
        <div className="flex flex-wrap items-center justify-between gap-3">
          <p className="text-sm text-muted-foreground">
            {dirty ? "Save your changes first." : count > 0 ? `${num(count)} unsent ${count === 1 ? "email" : "emails"}` : "No unsent emails."}
          </p>
          <Button variant="outline" onClick={rewrite} loading={starting} disabled={dirty || count === 0}>
            Rewrite {count > 0 ? num(count) : ""} {count === 1 ? "email" : "emails"}
          </Button>
        </div>
      )}
    </Section>
  );
}

const EXAMPLE_BODY =
  "I saw Northwind Studio just opened a second showroom in Portland, congrats on the expansion. " +
  "Teams growing that fast usually find new clients through referrals until the referrals run out. " +
  "We help interior studios book qualified projects every week without hiring a salesperson, by reaching out to the right people on your behalf.";
const DEFAULT_CTA = "Would it be worth a quick chat next week?";

function ExampleEmail({ s }: { s: ProjectSettings }) {
  const tone = TONES.find((t) => t.value === s.tone) ?? TONES[0]!;
  const body = `${EXAMPLE_BODY} ${s.cta.trim() || DEFAULT_CTA}`;
  const words = body.split(/\s+/);
  const limit = Math.max(s.max_words, 1);
  const shown = words.length > limit ? `${words.slice(0, limit).join(" ")}…` : body;

  return (
    <Section
      title="Example"
      description="Formatting only. The agent writes the actual words for each lead."
      actions={s.email_language !== "auto" && <Badge>Written in {s.email_language}</Badge>}
    >
      <Card className="overflow-hidden bg-background shadow-none">
        <div className="space-y-1 border-b border-border px-5 py-3 text-sm">
          <p>
            <span className="text-muted-foreground">From </span>
            {s.sender_name || "Your name"}
          </p>
          <p>
            <span className="text-muted-foreground">To </span>Sarah Chen, Northwind Studio
          </p>
          <p>
            <span className="text-muted-foreground">Subject </span>
            <span className="font-medium">Northwind Studio x your company</span>
          </p>
        </div>
        <div className="space-y-4 px-5 py-4 text-sm leading-relaxed">
          <p>{tone.greeting}</p>
          <p>{shown}</p>
          <div>
            <p>{tone.signoff}</p>
            <p className="whitespace-pre-line">{s.signature.trim() || s.sender_name || "Your name"}</p>
          </div>
        </div>
      </Card>
      <MicroLabel className="mt-3">
        {Math.min(words.length, limit)} / {s.max_words} words
      </MicroLabel>
    </Section>
  );
}
