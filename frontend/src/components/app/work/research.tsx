"use client";

import { useState } from "react";
import { ExternalLink, Mail, Plus, Sparkles } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch, Textarea } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon, Modal, Spinner, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { ResearchAnswer, ResearchGrid, ResearchQuestion, ResearchRow, ResearchStarted } from "@/lib/types";
import { cn, money, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced } from "./states";
import { SearchField } from "./search-field";
import { ScoreBar } from "./score";

type Draft = { id?: string; question: string; label: string; answer_type: ResearchQuestion["answer_type"]; use_in_emails: boolean };

// Questions most teams ask first. A click opens the editor with it filled in, to adjust before saving.
const PRESETS: Draft[] = [
  { question: "Do they sell to other businesses?", label: "Sells B2B", answer_type: "yes_no", use_in_emails: false },
  { question: "Do they offer a free trial or a free plan?", label: "Free trial", answer_type: "yes_no", use_in_emails: false },
  { question: "Do they have a mobile app?", label: "Mobile app", answer_type: "yes_no", use_in_emails: true },
  { question: "Which CRM do they use?", label: "CRM", answer_type: "text", use_in_emails: true },
  { question: "How many locations do they have?", label: "Locations", answer_type: "text", use_in_emails: true },
  { question: "Who are their main customers?", label: "Customers", answer_type: "text", use_in_emails: true },
];

const BLANK: Draft = { question: "", label: "", answer_type: "yes_no", use_in_emails: false };

/** The filter's options for one question, as `{question id}:{answer}`. */
function filterOptions(q: ResearchQuestion): [string, string][] {
  const answers: [string, string][] =
    q.answer_type === "yes_no"
      ? [
          ["yes", "Yes"],
          ["no", "No"],
        ]
      : [["answered", "Answered"]];
  return [...answers, ["unknown", "Unknown"], ["none", "Not researched"]].map(([a, label]) => [`${q.id}:${a}`, `${q.label}: ${label}`]);
}

/** Questions the agent answers about every company, as columns over the project's companies. */
export function ResearchView({ projectId }: { projectId: string }) {
  const [q, setQ] = useState("");
  const [filter, setFilter] = useState("");
  const [page, setPage] = useState(1);
  const [editing, setEditing] = useState<Draft | null>(null);
  const [cell, setCell] = useState<{ row: ResearchRow; question: ResearchQuestion } | null>(null);
  const [confirm, setConfirm] = useState<{ redo: boolean; estimate: ResearchStarted | null } | null>(null);
  const query = useDebounced(q);
  const [questionId, answer] = filter ? filter.split(":") : [null, null];
  const key = `/projects/${projectId}/research${qs({ q: query, question_id: questionId, answer, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<ResearchGrid>(key, fetcher, {
    keepPreviousData: true,
    // Answers come in a company at a time while research runs.
    refreshInterval: (latest) => (latest && latest.researching > 0 ? 4000 : 0),
  });
  const questions = data?.questions ?? [];

  const estimate = async (redo: boolean) => {
    setConfirm({ redo, estimate: null });
    try {
      const counted = await api<ResearchStarted>(`/projects/${projectId}/research/run`, { body: { redo, dry_run: true } });
      setConfirm({ redo, estimate: counted });
    } catch (e) {
      setConfirm(null);
      toastError(e);
    }
  };

  const research = async (body: Record<string, unknown>) => {
    try {
      const started = await api<ResearchStarted>(`/projects/${projectId}/research/run`, { body });
      await mutate();
      return started;
    } catch (e) {
      toastError(e);
      return null;
    }
  };

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
        <SearchField
          value={q}
          onChange={(v) => {
            setQ(v);
            setPage(1);
          }}
          placeholder="Search companies"
          label="Search companies"
          className="min-w-48 flex-[999_1_0%] sm:max-w-xs"
        />
        {questions.length > 0 && (
          <Select
            value={filter}
            onChange={(e) => {
              setFilter(e.target.value);
              setPage(1);
            }}
            className="h-9 w-auto min-w-0 flex-auto sm:max-w-56 sm:flex-none"
            aria-label="Filter by answer"
          >
            <option value="">Every answer</option>
            {questions.map((question) => (
              <optgroup key={question.id} label={question.label}>
                {filterOptions(question).map(([value, label]) => (
                  <option key={value} value={value}>
                    {label}
                  </option>
                ))}
              </optgroup>
            ))}
          </Select>
        )}
        <div className="flex flex-auto justify-end gap-2">
          <Button size="sm" variant="outline" onClick={() => setEditing(BLANK)} disabled={!data || questions.length >= 10}>
            <Plus className="size-4" /> Question
          </Button>
          <Button size="sm" onClick={() => estimate(false)} disabled={!questions.length || !data?.total}>
            <Sparkles className="size-4" /> Research
          </Button>
        </div>
      </div>

      {data && data.researching > 0 && (
        <p className="flex items-center gap-2 border-b border-border bg-sky-50/60 px-3 py-2 text-sm" role="status">
          <Spinner className="text-sky-700" />
          Researching {plural(data.researching, "answer")}. They fill in as each company is done.
        </p>
      )}

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="research" />
      ) : !data ? (
        <ListSkeleton />
      ) : questions.length === 0 ? (
        <EmptyState icon={<Sparkles className="size-5" />} title="Ask a question about every company">
          <p className="text-pretty">
            The agent reads each company&apos;s website and answers it, with the page that shows it. Filter by the answers, and let the writer open emails
            with them.
          </p>
          <div className="mt-4 flex flex-wrap justify-center gap-2">
            {PRESETS.map((p) => (
              <button
                key={p.question}
                type="button"
                onClick={() => setEditing(p)}
                className="rounded-full border border-border-strong bg-card px-3 py-1 text-sm text-foreground hover:bg-accent"
              >
                {p.question}
              </button>
            ))}
            <button type="button" onClick={() => setEditing(BLANK)} className="rounded-full px-3 py-1 text-sm font-medium text-foreground hover:bg-accent">
              Write your own
            </button>
          </div>
        </EmptyState>
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Sparkles className="size-5" />} title={query || filter ? "No companies match" : "No companies yet"}>
          {!query && !filter && "Companies you add to a campaign show here, ready to research."}
        </EmptyState>
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  {/* Sticky so the company stays in view while the questions scroll sideways. */}
                  <Th className="sticky left-0 z-10 min-w-44 bg-card">Company</Th>
                  <Th className="w-[1%]">Score</Th>
                  {questions.map((question) => (
                    <Th key={question.id} className="min-w-40 normal-case">
                      <button
                        type="button"
                        onClick={() => setEditing({ ...question })}
                        title={question.question}
                        className="-mx-1 inline-flex max-w-56 items-center gap-1 rounded px-1 py-0.5 text-left font-sans text-xs font-medium text-foreground hover:bg-accent"
                      >
                        <span className="truncate">{question.label}</span>
                        {question.use_in_emails && <Mail className="size-3 shrink-0 text-muted-foreground" aria-label="Used in emails" />}
                      </button>
                    </Th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {data.items.map((row) => (
                  <Tr key={row.id} className={cn(row.status === "excluded" && "text-muted-foreground")}>
                    <Td className="sticky left-0 z-10 max-w-56 bg-card">
                      <a href={`https://${row.domain}`} target="_blank" rel="noreferrer" className="flex items-center gap-2 hover:underline">
                        <Favicon domain={row.domain} />
                        <span className="min-w-0">
                          <span className="block truncate font-medium">{row.name || row.domain}</span>
                          <span className="block truncate text-xs text-muted-foreground">{row.segment_name ?? row.domain}</span>
                        </span>
                      </a>
                    </Td>
                    <Td>
                      <span className="flex items-center gap-2" title={`Score ${row.score} of 100`}>
                        <span className="min-w-6 text-right font-medium tabular-nums">{row.score}</span>
                        <ScoreBar fit={row.score_fit} intent={row.score_intent} className="max-sm:hidden" />
                      </span>
                    </Td>
                    {questions.map((question) => (
                      <Td key={question.id} className="py-1.5">
                        <AnswerCell answer={row.answers[question.id]} onOpen={() => setCell({ row, question })} />
                      </Td>
                    ))}
                  </Tr>
                ))}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={setPage} />
        </>
      )}

      <QuestionEditor
        projectId={projectId}
        draft={editing}
        onClose={() => setEditing(null)}
        onSaved={async (created) => {
          setEditing(null);
          await mutate();
          // A new question has no answers yet, so offer to fill its column straight away.
          if (created && data?.total) estimate(false);
        }}
      />

      <AnswerDetail
        cell={cell}
        onClose={() => setCell(null)}
        onAgain={async () => {
          if (!cell) return;
          const started = await research({ question_ids: [cell.question.id], company_ids: [cell.row.id], redo: true });
          if (started) setCell(null);
        }}
      />

      <Modal open={!!confirm} onClose={() => setConfirm(null)} title="Research your companies">
        {confirm && <ConfirmBody confirm={confirm} onRedo={(redo) => estimate(redo)} />}
        <div className="mt-5 flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setConfirm(null)}>
            Cancel
          </Button>
          <Button
            disabled={!confirm?.estimate?.companies}
            onClick={async () => {
              const redo = confirm?.redo ?? false;
              setConfirm(null);
              await research({ redo });
            }}
          >
            Research {confirm?.estimate ? plural(confirm.estimate.companies, "company", "companies") : ""}
          </Button>
        </div>
      </Modal>
    </Card>
  );
}

function ConfirmBody({ confirm, onRedo }: { confirm: { redo: boolean; estimate: ResearchStarted | null }; onRedo: (redo: boolean) => void }) {
  const e = confirm.estimate;
  return (
    <div className="flex flex-col gap-4 text-sm">
      {!e ? (
        <div className="flex items-center gap-2 text-muted-foreground">
          <Spinner /> Counting what needs an answer…
        </div>
      ) : e.companies === 0 ? (
        <p className="text-pretty">Every company already has an answer to every question. Tick below to ask again.</p>
      ) : (
        <p className="text-pretty">
          The agent answers {plural(e.answers, "question")} at {plural(e.companies, "company", "companies")}, best score first, with one Claude call per
          company.
          {e.cost_per_company_micros !== null && ` About ${money(e.cost_per_company_micros * e.companies)}, going by your last research.`}
          {e.more > 0 && ` ${plural(e.more, "more company", "more companies")} can go in the next batch.`}
        </p>
      )}
      <label className="flex items-center gap-3">
        <Switch checked={confirm.redo} onChange={onRedo} label="Ask again where there's an answer" />
        <span>Ask again where there&apos;s an answer</span>
      </label>
    </div>
  );
}

function AnswerCell({ answer, onOpen }: { answer?: ResearchAnswer; onOpen: () => void }) {
  return (
    <button type="button" onClick={onOpen} className="-mx-2 block w-[calc(100%+1rem)] rounded-md px-2 py-1 text-left hover:bg-accent">
      <AnswerText answer={answer} />
    </button>
  );
}

function AnswerText({ answer, large }: { answer?: ResearchAnswer; large?: boolean }) {
  if (!answer) return <span className="text-muted-foreground">—</span>;
  switch (answer.status) {
    case "queued":
      return (
        <span className="inline-flex items-center gap-1.5 text-muted-foreground">
          <Spinner className="size-3.5" /> Researching
        </span>
      );
    case "failed":
      return <span className="text-red-700">Failed</span>;
    case "unknown":
      return <span className="text-muted-foreground">Unknown</span>;
  }
  if (answer.answer === "yes") return <Badge tone="green">Yes</Badge>;
  if (answer.answer === "no") return <Badge>No</Badge>;
  return <span className={cn("text-pretty", !large && "line-clamp-2 max-w-64")}>{answer.answer}</span>;
}

function AnswerDetail({ cell, onClose, onAgain }: { cell: { row: ResearchRow; question: ResearchQuestion } | null; onClose: () => void; onAgain: () => Promise<void> }) {
  const [busy, setBusy] = useState(false);
  const answer = cell ? cell.row.answers[cell.question.id] : undefined;
  return (
    <Modal open={!!cell} onClose={onClose} title={cell?.row.name || cell?.row.domain}>
      {cell && (
        <div className="flex flex-col gap-4 text-sm">
          <div>
            <p className="text-muted-foreground">{cell.question.question}</p>
            <div className="mt-1.5 text-base">
              <AnswerText answer={answer} large />
            </div>
          </div>
          {answer?.evidence && <p className="rounded-xl bg-black/[0.03] px-3 py-2.5 text-pretty">{answer.evidence}</p>}
          {answer?.source_url && (
            <a href={answer.source_url} target="_blank" rel="noreferrer" className="inline-flex min-w-0 items-center gap-1 text-sm hover:underline">
              <span className="truncate">{answer.source_url.replace(/^https?:\/\//, "")}</span>
              <ExternalLink className="size-3 shrink-0" />
            </a>
          )}
          {!answer && <p className="text-muted-foreground">Not researched yet.</p>}
          {answer?.status === "failed" && <p className="text-muted-foreground">The agent couldn&apos;t finish this one. Ask again to retry.</p>}
          <div className="flex items-center justify-between gap-2">
            <span className="text-xs text-muted-foreground">{answer && answer.status !== "queued" && `Checked ${timeAgo(answer.updated_at)}`}</span>
            <Button
              size="sm"
              variant="outline"
              loading={busy}
              disabled={answer?.status === "queued"}
              onClick={async () => {
                setBusy(true);
                await onAgain();
                setBusy(false);
              }}
            >
              <Sparkles className="size-4" /> {answer ? "Ask again" : "Research"}
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}

function QuestionEditor({
  projectId,
  draft,
  onClose,
  onSaved,
}: {
  projectId: string;
  draft: Draft | null;
  onClose: () => void;
  onSaved: (created: boolean) => void;
}) {
  return (
    <Modal open={!!draft} onClose={onClose} title={draft?.id ? "Edit question" : "New research question"}>
      {/* Keyed so each opening starts from its own draft. */}
      {draft && <QuestionForm key={draft.id ?? draft.question} projectId={projectId} draft={draft} onClose={onClose} onSaved={onSaved} />}
    </Modal>
  );
}

function QuestionForm({ projectId, draft, onClose, onSaved }: { projectId: string; draft: Draft; onClose: () => void; onSaved: (created: boolean) => void }) {
  const [form, setForm] = useState(draft);
  const [busy, setBusy] = useState(false);
  const changesAnswers = !!draft.id && (form.question.trim() !== draft.question || form.answer_type !== draft.answer_type);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const body = { question: form.question, label: form.label, answer_type: form.answer_type, use_in_emails: form.use_in_emails };
      if (draft.id) await api(`/research/questions/${draft.id}`, { method: "PATCH", body });
      else await api(`/projects/${projectId}/research/questions`, { body });
      onSaved(!draft.id);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!draft.id || !confirm(`Delete "${draft.label}" and its answers?`)) return;
    setBusy(true);
    try {
      await api(`/research/questions/${draft.id}`, { method: "DELETE" });
      onSaved(false);
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  return (
    <form onSubmit={save} className="flex flex-col gap-4">
      <Field label="Question" hint={changesAnswers ? "Changing the question or its answer clears the answers found so far." : "About one company, like you'd ask a colleague."}>
        <Textarea
          value={form.question}
          onChange={(e) => setForm({ ...form, question: e.target.value })}
          placeholder="Do they sell to hospitals?"
          maxLength={300}
          rows={2}
          required
          autoFocus
        />
      </Field>
      <Field label="Column name" hint="The question's first words if you leave it empty.">
        <Input value={form.label} onChange={(e) => setForm({ ...form, label: e.target.value })} maxLength={40} placeholder="Sells to hospitals" />
      </Field>
      <Field label="Answer" group>
        <Tabs
          value={form.answer_type}
          onChange={(answer_type) => setForm({ ...form, answer_type })}
          options={[
            { value: "yes_no", label: "Yes or no" },
            { value: "text", label: "Short answer" },
          ]}
          className="self-start"
        />
      </Field>
      <label className="flex items-start gap-3 text-sm">
        <Switch checked={form.use_in_emails} onChange={(use_in_emails) => setForm({ ...form, use_in_emails })} label="Use the answers in emails" />
        <span className="text-pretty">
          Use the answers in emails
          <span className="block text-xs text-muted-foreground">The writer can open a first email with what the agent found.</span>
        </span>
      </label>
      <div className="mt-1 flex items-center gap-2">
        {draft.id && (
          <Button type="button" variant="ghost" className="text-danger" onClick={remove} disabled={busy}>
            Delete
          </Button>
        )}
        <span className="flex-1" />
        <Button type="button" variant="ghost" onClick={onClose}>
          Cancel
        </Button>
        <Button type="submit" loading={busy}>
          {draft.id ? "Save" : "Add question"}
        </Button>
      </div>
    </form>
  );
}
