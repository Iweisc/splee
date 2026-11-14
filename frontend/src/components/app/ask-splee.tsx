"use client";

import Link from "next/link";
import { Fragment, useCallback, useEffect, useRef, useState } from "react";
import { CircleAlert, Loader2, RotateCcw, SendHorizontal, Sparkles, SquarePen, X } from "lucide-react";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { api } from "@/lib/api";
import { Kbd, useShortcut } from "./work/list-keys";
import { LeadModal } from "./work/lead-modal";

/** One question and, once the run finishes, its answer or why there isn't one. */
interface Turn {
  id: string;
  question: string;
  run_id?: string;
  answer?: string;
  error?: string;
}

const SUGGESTIONS = [
  "Who replied this week?",
  "Which campaign gets the most replies?",
  "Which deals are closest to closing?",
  "What meetings are coming up?",
];

const storageKey = (projectId: string) => `splee:ask:${projectId}`;

/** The sidebar button and A open a panel that answers questions about the project's pipeline. See agent/ask.rs. */
export function AskSplee({ projectId }: { projectId: string }) {
  const [open, setOpen] = useState(false);
  useShortcut(
    "a",
    useCallback(() => setOpen(true), []),
  );
  return (
    <>
      <button
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        className="flex w-full items-center gap-2 rounded-lg border border-border bg-card px-2.5 py-1.5 text-sm text-muted-foreground shadow-card transition-colors hover:border-border-strong hover:text-foreground"
      >
        <Sparkles className="size-4 text-brand-600" />
        <span className="flex-1 text-left">Ask Splee</span>
        <Kbd>A</Kbd>
      </button>
      {/* Keyed so another project starts from its own conversation. */}
      {open && <AskPanel key={projectId} projectId={projectId} onClose={() => setOpen(false)} />}
    </>
  );
}

function AskPanel({ projectId, onClose }: { projectId: string; onClose: () => void }) {
  // The panel only mounts after a click or a key, so localStorage is there and the first render can read it. A question
  // whose request was still out when the panel closed never learned its run, so it can only be asked again.
  const [turns, setTurns] = useState<Turn[]>(() =>
    (JSON.parse(localStorage.getItem(storageKey(projectId)) ?? "[]") as Turn[]).map((t) =>
      t.run_id || t.answer !== undefined || t.error ? t : { ...t, error: "The panel closed before this was asked." },
    ),
  );
  const [draft, setDraft] = useState("");
  const [lead, setLead] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);
  const busy = turns.some((t) => t.answer === undefined && !t.error);

  useEffect(() => {
    localStorage.setItem(storageKey(projectId), JSON.stringify(turns));
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight, behavior: "smooth" });
  }, [projectId, turns]);

  useEffect(() => inputRef.current?.focus(), []);

  const update = useCallback((id: string, patch: Partial<Turn>) => setTurns((ts) => ts.map((t) => (t.id === id ? { ...t, ...patch } : t))), []);

  const ask = async (text: string, before: Turn[] = turns) => {
    const question = text.trim();
    if (!question || busy) return;
    const id = crypto.randomUUID();
    const history = before.filter((t) => t.answer).map((t) => ({ question: t.question, answer: t.answer }));
    setTurns([...before, { id, question }]);
    setDraft("");
    try {
      const { run_id } = await api<{ run_id: string }>(`/projects/${projectId}/ask`, { body: { question, history } });
      update(id, { run_id });
    } catch (e) {
      update(id, { error: e instanceof Error ? e.message : "Couldn't ask. Try again." });
    }
  };
  // Asks a failed question again in its place, with the same conversation before it.
  const retry = (turn: Turn) => ask(turn.question, turns.slice(0, turns.indexOf(turn)));

  return (
    <aside
      aria-label="Ask Splee"
      onKeyDown={(e) => e.key === "Escape" && !lead && onClose()}
      className="fixed inset-y-0 right-0 z-40 flex w-full animate-fade-up flex-col border-l border-border bg-card shadow-pop sm:w-[420px]"
    >
      <div className="flex items-center gap-2 border-b border-border px-4 py-3">
        <Sparkles className="size-4 text-brand-600" />
        <div className="min-w-0 flex-1">
          <p className="font-medium">Ask Splee</p>
          <p className="text-xs text-muted-foreground">Reads your leads, campaigns, replies and deals. It can&apos;t change anything.</p>
        </div>
        {turns.length > 0 && (
          <button
            onClick={() => setTurns([])}
            disabled={busy}
            className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground disabled:opacity-40"
            aria-label="New conversation"
            title="New conversation"
          >
            <SquarePen className="size-4" />
          </button>
        )}
        <button onClick={onClose} className="rounded-md p-1.5 text-muted-foreground hover:bg-accent hover:text-foreground" aria-label="Close">
          <X className="size-4" />
        </button>
      </div>

      <div ref={listRef} className="flex flex-1 flex-col gap-5 overflow-y-auto px-4 py-5">
        {turns.length === 0 && (
          <div className="flex flex-col gap-2">
            <p className="text-sm text-muted-foreground">Ask anything about this project. Try:</p>
            {SUGGESTIONS.map((s) => (
              <button
                key={s}
                onClick={() => ask(s)}
                className="rounded-xl border border-border px-3 py-2 text-left text-sm transition-colors hover:border-border-strong hover:bg-accent"
              >
                {s}
              </button>
            ))}
          </div>
        )}
        {turns.map((t) => (
          <div key={t.id} className="flex flex-col gap-2">
            <p className="max-w-[85%] self-end rounded-2xl bg-primary px-3 py-2 text-sm whitespace-pre-wrap text-primary-foreground">{t.question}</p>
            {t.answer !== undefined ? (
              <AnswerText text={t.answer} projectId={projectId} openLead={setLead} />
            ) : t.error ? (
              <div className="flex items-start gap-2 text-sm text-danger">
                <CircleAlert className="mt-0.5 size-4 shrink-0" />
                <p className="min-w-0 flex-1">{t.error}</p>
                <button
                  onClick={() => retry(t)}
                  disabled={busy}
                  className="flex items-center gap-1 text-foreground hover:underline disabled:opacity-40"
                >
                  <RotateCcw className="size-3.5" /> Retry
                </button>
              </div>
            ) : (
              <Pending runId={t.run_id} onAnswer={(answer) => update(t.id, { answer })} onError={(error) => update(t.id, { error })} />
            )}
          </div>
        ))}
      </div>

      <form
        className="flex items-end gap-2 border-t border-border p-3"
        onSubmit={(e) => {
          e.preventDefault();
          ask(draft);
        }}
      >
        {/* Grows with the question up to five lines, then scrolls. 1.125rem is the padding plus border. */}
        <textarea
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              ask(draft);
            }
          }}
          rows={1}
          maxLength={2000}
          placeholder={turns.length ? "Ask a follow-up…" : "Ask about your pipeline…"}
          className="max-h-[calc(5lh+1.125rem)] min-h-9 flex-1 resize-none field-sizing-content rounded-lg border border-border-strong px-3 py-2 text-sm outline-none focus:border-foreground/40"
        />
        <button
          type="submit"
          disabled={!draft.trim() || busy}
          className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground disabled:opacity-40"
          aria-label="Ask"
        >
          <SendHorizontal className="size-4" />
        </button>
      </form>
      <LeadModal leadId={lead} onClose={() => setLead(null)} />
    </aside>
  );
}

/** Follows the question's run and shows what the agent is looking at until the answer comes. */
function Pending({ runId, onAnswer, onError }: { runId?: string; onAnswer: (answer: string) => void; onError: (error: string) => void }) {
  const { events } = useAgentEvents(runId ? `/api/runs/${runId}/events` : null, (e) => {
    if (e.kind === "done") onAnswer(typeof e.data.answer === "string" ? e.data.answer : "");
    if (e.kind === "error") onError(e.data.cancelled ? "Stopped." : "Splee couldn't answer that. Try again, or ask it another way.");
  });
  const line = events.findLast((e) => e.kind === "log")?.message;
  return (
    <p className="flex items-center gap-2 text-sm text-muted-foreground">
      <Loader2 className="size-3.5 animate-spin" />
      {line ? line.charAt(0).toUpperCase() + line.slice(1) : "Thinking…"}
    </p>
  );
}

// **bold**, or [text](target) where the target is lead:<id>, campaign:<id> or a web address.
const INLINE = /\*\*(.+?)\*\*|\[([^\]]+)\]\(([^)\s]+)\)/g;
const BULLET = /^\s*(?:[-*]|\d+\.)\s+/;

/** The answer's short paragraphs and lists, with leads opening in their window and campaigns linking to their page. */
function AnswerText({ text, projectId, openLead }: { text: string; projectId: string; openLead: (id: string) => void }) {
  const inline = (line: string) => {
    const parts: React.ReactNode[] = [];
    let last = 0;
    for (const m of line.matchAll(INLINE)) {
      parts.push(line.slice(last, m.index));
      last = m.index + m[0].length;
      const [, bold, label, target] = m;
      if (bold) parts.push(<strong key={m.index}>{bold}</strong>);
      else if (target.startsWith("lead:"))
        parts.push(
          <button key={m.index} onClick={() => openLead(target.slice(5))} className="font-medium text-brand-600 hover:underline">
            {label}
          </button>,
        );
      else if (target.startsWith("campaign:"))
        parts.push(
          <Link key={m.index} href={`/app/p/${projectId}/segments/${target.slice(9)}`} className="font-medium text-brand-600 hover:underline">
            {label}
          </Link>,
        );
      else if (/^https?:\/\//.test(target))
        parts.push(
          <a key={m.index} href={target} target="_blank" rel="noreferrer" className="text-brand-600 hover:underline">
            {label}
          </a>,
        );
      else parts.push(label);
    }
    parts.push(line.slice(last));
    return parts;
  };

  // Lines group into paragraphs and lists. A blank line ends a group, and so does a switch between text and list items,
  // because the model often writes "Three replied:" with the list right under it.
  const groups: { list: boolean; lines: string[] }[] = [];
  let open = false;
  for (const line of text.split("\n")) {
    if (!line.trim()) {
      open = false;
      continue;
    }
    const list = BULLET.test(line);
    const last = groups.at(-1);
    if (open && last?.list === list) last.lines.push(list ? line.replace(BULLET, "") : line);
    else groups.push({ list, lines: [list ? line.replace(BULLET, "") : line] });
    open = true;
  }
  return (
    <div className="flex flex-col gap-2 text-sm leading-relaxed text-pretty">
      {groups.map((g, i) =>
        g.list ? (
          <ul key={i} className="flex list-disc flex-col gap-1 pl-5">
            {g.lines.map((l, j) => (
              <li key={j}>{inline(l)}</li>
            ))}
          </ul>
        ) : (
          <p key={i}>
            {g.lines.map((l, j) => (
              <Fragment key={j}>
                {j > 0 && <br />}
                {inline(l)}
              </Fragment>
            ))}
          </p>
        ),
      )}
    </div>
  );
}
