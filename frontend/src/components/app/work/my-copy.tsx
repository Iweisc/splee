"use client";

import { useRef, useState } from "react";
import { PenLine } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Card, Skeleton, Tabs } from "@/components/ui/misc";
import { useLeaveGuard } from "@/hooks/use-leave-guard";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { CampaignCopy, CopyPreview, FirstEmail, ProjectCopy } from "@/lib/types";
import { cn, num, percent } from "@/lib/utils";
import { MicroLabel, toastError, useDebounced } from "./states";

/** What the chips insert, in the backend's order (my_copy::VARIABLES), then an option and an AI line. */
const SNIPPETS = [
  "{{first_name}}",
  "{{company}}",
  "{{job_title}}",
  "{{company_city|your city}}",
  "{{sender_first_name}}",
  "{{sender_name}}",
  "{{sender_company}}",
  "{Hi|Hello}",
  "{{ai: one line about their company}}",
];

const MODES = [
  { value: "instructions" as const, label: "Instructions" },
  { value: "my_copy" as const, label: "My copy" },
];

type Scope = { kind: "campaign"; segmentId: string } | { kind: "project"; projectId: string };

/** The first email: the agent's, written from instructions, or the user's own text. On a campaign page, also whether it's the
 *  project's or the campaign's own. */
export function MyCopyCard({ scope, className }: { scope: Scope; className?: string }) {
  const key = scope.kind === "campaign" ? `/segments/${scope.segmentId}/my-copy` : `/projects/${scope.projectId}/my-copy`;
  const { data, mutate } = useSWR<CampaignCopy | ProjectCopy>(key, fetcher);
  if (!data) return <Skeleton className="h-40 rounded-2xl" />;
  // Keyed on the saved text, so the editor starts over from what a save or a source switch returned.
  const version = JSON.stringify([data.mode, data.subject, data.body, "source" in data && data.source]);
  return <Editor key={version} path={key} copy={data} onSaved={(c) => mutate(c, { revalidate: false })} className={className} />;
}

function Editor({
  path,
  copy,
  onSaved,
  className,
}: {
  path: string;
  copy: CampaignCopy | ProjectCopy;
  onSaved: (c: CampaignCopy | ProjectCopy) => void;
  className?: string;
}) {
  const [draft, setDraft] = useState<FirstEmail>({ mode: copy.mode, subject: copy.subject, body: copy.body });
  const [saving, setSaving] = useState(false);
  const subjectRef = useRef<HTMLInputElement>(null);
  const bodyRef = useRef<HTMLTextAreaElement>(null);
  const lastField = useRef<"subject" | "body">("body");
  const campaign = "source" in copy ? copy : null;
  const followsProject = campaign?.source === "project";
  const dirty = draft.mode !== copy.mode || draft.subject !== copy.subject || draft.body !== copy.body;
  useLeaveGuard(dirty);

  const save = async () => {
    setSaving(true);
    try {
      onSaved(await api<CampaignCopy | ProjectCopy>(path, { method: "PATCH", body: { ...draft } }));
      toast.success("First email saved");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  const setSource = async (source: "project" | "campaign") => {
    if (dirty && !confirm("Switching drops your unsaved changes. Switch anyway?")) return;
    try {
      onSaved(await api<CampaignCopy>(`${path}/source`, { method: "PUT", body: { source } }));
    } catch (e) {
      toastError(e);
    }
  };

  // Puts the snippet where the cursor was in the field last used, and selects nothing, so typing carries on after it.
  const insert = (snippet: string) => {
    const field = lastField.current;
    const el = field === "subject" ? subjectRef.current : bodyRef.current;
    const text = draft[field];
    const [start, end] = el ? [el.selectionStart ?? text.length, el.selectionEnd ?? text.length] : [text.length, text.length];
    setDraft({ ...draft, [field]: text.slice(0, start) + snippet + text.slice(end) });
    requestAnimationFrame(() => {
      el?.focus();
      el?.setSelectionRange(start + snippet.length, start + snippet.length);
    });
  };

  return (
    <Card className={cn("flex flex-col gap-5 p-5", className)}>
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <PenLine className="size-3.5" /> First email
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          The agent writes each first email from {campaign ? "the campaign's angle" : "your settings above"}, or you write it once and each lead gets
          it with their details filled in. With My copy on, one lead in ten still gets the AI email so you can compare the two. Follow-ups are written
          as before.
        </p>
      </div>

      <div className="flex flex-wrap gap-3">
        {!followsProject && <Tabs value={draft.mode} onChange={(mode) => setDraft({ ...draft, mode })} options={MODES} />}
        {campaign && (
          <Tabs
            value={campaign.source}
            onChange={setSource}
            options={[
              { value: "project", label: "From project" },
              { value: "campaign", label: "Custom" },
            ]}
          />
        )}
      </div>

      {followsProject ? (
        <FollowsProject copy={copy as CampaignCopy} />
      ) : (
        <>
          {draft.mode === "instructions" && (
            <p className="text-sm text-pretty text-muted-foreground">
              {copy.instructions.trim()
                ? `The AI email is written from ${campaign ? "the campaign's angle" : "your extra instructions"}: “${copy.instructions.trim()}”. Your text below is kept for when you turn My copy on.`
                : "Your text below is kept for when you turn My copy on."}
            </p>
          )}
          <Field label="Subject" hint="Leave it empty to send “Your company x Their company”.">
            <Input
              ref={subjectRef}
              value={draft.subject}
              maxLength={300}
              onFocus={() => {
                lastField.current = "subject";
              }}
              onChange={(e) => setDraft({ ...draft, subject: e.target.value })}
              placeholder="{Quick|Short} question, {{first_name}}"
            />
          </Field>
          <Field label="Body" hint="Your signature goes under it, unless it already ends with your name.">
            <Textarea
              ref={bodyRef}
              value={draft.body}
              maxLength={3000}
              rows={9}
              onFocus={() => {
                lastField.current = "body";
              }}
              onChange={(e) => setDraft({ ...draft, body: e.target.value })}
              placeholder={"Hi {{first_name}},\n\n{{ai: one line about their company}}\n\nWould it help {{company}} to …?"}
            />
          </Field>
          <div className="flex flex-col gap-2">
            <p className="text-sm text-pretty text-muted-foreground">
              Click to insert. A lead missing a variable gets the AI email, unless it has a fallback after a |. {"{Hi|Hello}"} picks one for each lead, and
              each <span className="whitespace-nowrap">{"{{ai: …}}"}</span> line is written for each lead.
            </p>
            <div className="flex flex-wrap gap-1.5">
              {SNIPPETS.map((s) => (
                <button
                  key={s}
                  type="button"
                  onClick={() => insert(s)}
                  className="rounded-lg bg-black/5 px-2 py-1 font-mono text-xs break-all text-foreground/80 transition-colors hover:bg-black/10 hover:text-foreground"
                >
                  {s}
                </button>
              ))}
            </div>
          </div>
          <PreviewPanel path={`${path}/preview`} draft={draft} />
          {campaign && <Comparison results={campaign.results} />}
          {dirty && (
            <div className="flex flex-wrap justify-end gap-2">
              <Button variant="ghost" onClick={() => setDraft({ mode: copy.mode, subject: copy.subject, body: copy.body })}>
                Cancel
              </Button>
              <Button onClick={save} loading={saving}>
                Save
              </Button>
            </div>
          )}
        </>
      )}
    </Card>
  );
}

function FollowsProject({ copy }: { copy: CampaignCopy }) {
  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm text-pretty text-muted-foreground">
        {copy.mode === "my_copy"
          ? "This campaign sends the project's own first email. Pick Custom to give it its own, starting from a copy of the project's."
          : "This campaign sends the AI email, like the project. Pick Custom to give it its own first email."}
      </p>
      {copy.mode === "my_copy" && <PreviewPanel path={`/segments/${copy.campaign_id}/my-copy/preview`} draft={{}} />}
      <Comparison results={copy.results} />
      <LinkButton href={`/app/p/${copy.project_id}/email-templates`} variant="outline" size="sm" className="self-start">
        Edit the project&apos;s first email
      </LinkButton>
    </div>
  );
}

/** "What leads get": the draft rendered for the first 3 leads, re-rendered a moment after typing stops. */
function PreviewPanel({ path, draft }: { path: string; draft: Partial<FirstEmail> }) {
  const body = useDebounced(JSON.stringify(draft), 400);
  const { data, error, isLoading } = useSWR<CopyPreview>([path, body], ([p, b]: [string, string]) => api<CopyPreview>(p, { method: "POST", body: JSON.parse(b) }), {
    keepPreviousData: true,
  });

  return (
    <div className="flex flex-col gap-2">
      <MicroLabel>What leads get</MicroLabel>
      {error ? (
        <p className="text-sm text-pretty text-danger">{error instanceof Error ? error.message : "Couldn't render the preview."}</p>
      ) : !data && isLoading ? (
        <Skeleton className="h-28 rounded-xl" />
      ) : !data || data.emails.length === 0 ? (
        <p className="text-sm text-pretty text-muted-foreground">
          {draft.body !== undefined && !draft.body.trim() ? "Write the email to see it here for your first three leads." : "No leads with an email yet."}
        </p>
      ) : (
        <div className="grid gap-3 lg:grid-cols-3">
          {data.emails.map((e) => (
            <div key={e.lead_id} className="flex min-w-0 flex-col gap-2 rounded-xl border border-border p-4 text-sm">
              <p className="truncate text-muted-foreground">
                To <span className="text-foreground">{e.lead_name || "this lead"}</span>
                {e.lead_company && `, ${e.lead_company}`}
              </p>
              {e.note ? (
                <p className="text-pretty text-muted-foreground">{e.note}</p>
              ) : (
                <>
                  <p className="font-medium break-words">{e.subject}</p>
                  <p className="leading-relaxed break-words whitespace-pre-wrap">{e.body}</p>
                </>
              )}
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

function Comparison({ results }: { results: CampaignCopy["results"] }) {
  if (results.my_copy.sent + results.ai.sent === 0) return null;
  const side = (label: string, s: { sent: number; replies: number }) => (
    <div className="min-w-0 rounded-xl bg-black/[0.03] px-4 py-3">
      <MicroLabel>{label}</MicroLabel>
      <p className="mt-1 text-sm tabular-nums">
        {s.sent ? percent(s.replies / s.sent) : "–"} replied <span className="text-muted-foreground">of {num(s.sent)} sent</span>
      </p>
    </div>
  );
  return (
    <div className="flex flex-col gap-2">
      <div className="grid gap-2 sm:grid-cols-2">
        {side("Your text", results.my_copy)}
        {side("AI email", results.ai)}
      </div>
      <p className="text-sm text-pretty text-muted-foreground">{results.note}</p>
    </div>
  );
}
