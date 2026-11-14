"use client";

import { useEffect, useRef, useState } from "react";
import { Headset, Loader2, RotateCcw, Send, Square } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/form";
import { Badge, type BadgeTone, Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, PracticeAssignment, PracticeSession } from "@/lib/types";
import { cn, timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

const SCENARIOS: { value: PracticeSession["scenario"]; label: string }[] = [
  { value: "next_call", label: "The next call" },
  { value: "pricing", label: "Price pushback" },
  { value: "competitor", label: "A competitor" },
];
export const scenarioLabel = (s: PracticeSession["scenario"]) => SCENARIOS.find((x) => x.value === s)?.label ?? s;
const busy = (s: PracticeSession) => s.status === "preparing" || s.status === "thinking" || s.status === "scoring";
export const tone = (score: number): BadgeTone => (score >= 75 ? "green" : score >= 50 ? "amber" : "red");
const firstName = (s: PracticeSession) => s.persona?.name.split(" ")[0] ?? "The buyer";

/** In an open deal's window: what you were assigned to practice on it, starting a practice call with the agent playing the
 * buyer, and the deal's past ones. */
export function PracticeSection({ deal }: { deal: Deal }) {
  const key = deal.status === "open" ? `/deals/${deal.id}/practice` : null;
  const { data: sessions, mutate } = useSWR<PracticeSession[]>(key, fetcher);
  const { data: assigned, mutate: mutateAssigned } = useSWR<PracticeAssignment[]>(key && `${key}/assigned`, fetcher);
  const [opened, setOpened] = useState<string | null>(null);
  const [starting, setStarting] = useState<string | null>(null);
  if (deal.status !== "open" || !sessions) return null;

  const begin = async (scenario: PracticeSession["scenario"], assignment?: PracticeAssignment) => {
    setStarting(assignment?.id ?? scenario);
    try {
      const s = await api<PracticeSession>(`/deals/${deal.id}/practice`, { body: { scenario, assignment_id: assignment?.id } });
      await Promise.all([mutate(), assignment && mutateAssigned()]);
      setOpened(s.id);
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(null);
    }
  };

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div>
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <Headset className="size-3.5" /> Practice the next call
        </p>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          The agent plays {deal.lead_name.split(" ")[0]} from what Splee knows about this deal: what they care about, what&apos;s still open and the
          competitors in it. Then it scores you.
        </p>
      </div>
      {assigned?.map((a) => (
        <div key={a.id} className="flex flex-col gap-2 rounded-lg bg-brand-50 p-3 text-sm text-pretty sm:flex-row sm:items-center">
          <div className="min-w-0">
            <p className="font-medium text-brand-800">
              {a.assigned_by ? `${a.assigned_by.split(" ")[0]} asked you to practice` : "Assigned to you"}: {scenarioLabel(a.scenario).toLowerCase()}
            </p>
            <p className="text-brand-800/80">
              {a.skill}. {a.drill}
            </p>
          </div>
          {a.session_id ? (
            <Button type="button" size="sm" variant="outline" className="shrink-0 sm:ml-auto" onClick={() => setOpened(a.session_id)}>
              Carry on
            </Button>
          ) : (
            <Button
              type="button"
              size="sm"
              className="shrink-0 sm:ml-auto"
              onClick={() => begin(a.scenario, a)}
              loading={starting === a.id}
              disabled={!!starting}
            >
              Start it
            </Button>
          )}
        </div>
      ))}
      <div className="flex flex-wrap gap-2">
        {SCENARIOS.map((s) => (
          <Button
            key={s.value}
            type="button"
            size="sm"
            variant="outline"
            onClick={() => begin(s.value)}
            loading={starting === s.value}
            disabled={!!starting}
          >
            {s.label}
          </Button>
        ))}
      </div>
      {sessions.length > 0 && (
        <ul className="flex flex-col divide-y divide-border text-sm">
          {sessions.slice(0, 5).map((s) => (
            <li key={s.id}>
              <button type="button" onClick={() => setOpened(s.id)} className="flex w-full items-center gap-2 py-2 text-left hover:underline">
                {s.score ? (
                  <Badge tone={tone(s.score.score)}>{s.score.score}</Badge>
                ) : (
                  <Badge>{s.status === "failed" ? "Failed" : "Not scored"}</Badge>
                )}
                <span>{scenarioLabel(s.scenario)}</span>
                <span className="ml-auto text-xs text-muted-foreground">{[s.user_name, timeAgo(s.created_at)].filter(Boolean).join(" · ")}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {opened && (
        <PracticeRoom
          id={opened}
          onClose={() => {
            setOpened(null);
            mutate();
            mutateAssigned();
          }}
        />
      )}
    </section>
  );
}

/** The call itself: the buyer's lines and the rep's, the box to answer in, and the score at the end. */
function PracticeRoom({ id, onClose }: { id: string; onClose: () => void }) {
  const { mutate: mutateAll } = useSWRConfig();
  const { data: s, mutate } = useSWR<PracticeSession>(`/practice/${id}`, fetcher, {
    refreshInterval: (s) => (s && busy(s) ? 1200 : 0),
  });
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const log = useRef<HTMLDivElement>(null);
  const turns = s?.turns.length ?? 0;
  const status = s?.status;
  // Keeps the newest line in view. Scrolling only the transcript, so the window around it stays put.
  useEffect(() => {
    if (log.current) log.current.scrollTop = log.current.scrollHeight;
  }, [turns, status]);

  const act = async (path: string, body?: { text: string }) => {
    setSending(true);
    try {
      const next = await api<PracticeSession>(`/practice/${id}/${path}`, { body: body ?? {} });
      await mutate(next, { revalidate: false });
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setSending(false);
      mutateAll((k) => typeof k === "string" && k.endsWith("/practice"));
    }
  };
  const say = async () => {
    const line = text.trim();
    if (!line) return;
    if (await act("turns", { text: line })) setText("");
  };

  const name = s?.persona?.name ?? "The buyer";
  return (
    <Modal open onClose={onClose} className="max-w-2xl" title={s ? `Practice: ${scenarioLabel(s.scenario)}` : "Practice"}>
      {!s ? (
        <Loader2 className="mx-auto size-5 animate-spin text-muted-foreground" />
      ) : (
        <div className="flex flex-col gap-4">
          {s.persona && (
            <p className="text-sm text-muted-foreground">
              <span className="font-medium text-foreground">{name}</span>
              {[s.persona.title, s.persona.company].filter(Boolean).length > 0 &&
                `, ${[s.persona.title, s.persona.company].filter(Boolean).join(" at ")}`}
            </p>
          )}
          <div ref={log} className="flex max-h-[45vh] min-h-40 flex-col gap-2 overflow-y-auto rounded-xl bg-muted/50 p-3">
            {s.turns.map((t, i) => (
              <p
                key={i}
                className={cn(
                  "max-w-[85%] rounded-2xl px-3 py-2 text-sm text-pretty whitespace-pre-wrap",
                  t.who === "rep" ? "self-end bg-primary text-primary-foreground" : "self-start border border-border bg-card",
                )}
              >
                {t.text}
              </p>
            ))}
            {busy(s) && (
              <p className="flex items-center gap-1.5 self-start text-xs text-muted-foreground">
                <Loader2 className="size-3.5 animate-spin" />
                {s.status === "preparing"
                  ? "Getting the buyer ready…"
                  : s.status === "scoring"
                    ? "Scoring the call…"
                    : `${firstName(s)} is thinking…`}
              </p>
            )}
            {s.status === "ended" && <p className="self-center text-xs text-muted-foreground">{firstName(s)} ended the call.</p>}
          </div>

          {s.status === "failed" ? (
            <div className="flex items-center justify-between gap-3">
              <p className="text-sm text-danger">{s.error || "The agent couldn't finish."}</p>
              <Button type="button" size="sm" variant="outline" onClick={() => act("retry")} loading={sending}>
                <RotateCcw className="size-3.5" /> Try again
              </Button>
            </div>
          ) : s.score ? (
            <Scorecard s={s} />
          ) : (
            <div className="flex flex-col gap-2">
              {s.status !== "ended" && (
                <Textarea
                  value={text}
                  onChange={(e) => setText(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && !e.shiftKey) {
                      e.preventDefault();
                      if (s.status === "live" && !sending) say();
                    }
                  }}
                  placeholder={s.status === "preparing" ? "" : `Answer ${firstName(s)}…`}
                  aria-label="Your line"
                  className="min-h-16"
                  disabled={s.status === "preparing" || s.status === "scoring"}
                />
              )}
              <div className="flex flex-wrap items-center justify-end gap-2">
                <Button
                  type="button"
                  size="sm"
                  variant="ghost"
                  onClick={() => act("score")}
                  loading={sending && s.status !== "live"}
                  disabled={busy(s) || !s.turns.some((t) => t.who === "rep")}
                >
                  <Square className="size-3.5" /> End and score
                </Button>
                {s.status !== "ended" && (
                  <Button
                    type="button"
                    size="sm"
                    onClick={say}
                    loading={sending && s.status === "live"}
                    disabled={s.status !== "live" || !text.trim()}
                  >
                    <Send className="size-3.5" /> Say it
                  </Button>
                )}
              </div>
            </div>
          )}
        </div>
      )}
    </Modal>
  );
}

function List({ title, items }: { title: string; items: string[] }) {
  if (!items.length) return null;
  return (
    <div>
      <MicroLabel>{title}</MicroLabel>
      <ul className="mt-1 flex list-disc flex-col gap-1 pl-5 text-sm text-pretty">
        {items.map((i) => (
          <li key={i}>{i}</li>
        ))}
      </ul>
    </div>
  );
}

/** How the rep did, and what the buyer was thinking all along. */
function Scorecard({ s }: { s: PracticeSession }) {
  const score = s.score!;
  const p = s.persona;
  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-start gap-4">
        <p
          className={cn(
            "shrink-0 text-4xl font-semibold tabular-nums",
            score.score >= 75 ? "text-brand-700" : score.score >= 50 ? "text-amber-700" : "text-red-700",
          )}
        >
          {score.score}
          <span className="text-base font-normal text-muted-foreground">/100</span>
        </p>
        <p className="text-sm text-pretty">{score.summary}</p>
      </div>
      {score.skill && (
        <div className="rounded-xl bg-brand-50 p-3 text-sm text-pretty text-brand-800">
          <MicroLabel>On the skill you were assigned</MicroLabel>
          <p className="mt-1">{score.skill}</p>
        </div>
      )}
      <div className="grid gap-4 sm:grid-cols-2">
        <List title="Keep doing" items={score.went_well} />
        <List title="Try next time" items={score.try_next} />
      </div>
      {score.moments.length > 0 && (
        <div>
          <MicroLabel>Say it differently</MicroLabel>
          <ul className="mt-1 flex flex-col gap-2">
            {score.moments.map((m) => (
              <li key={m.you_said} className="rounded-xl border border-border p-3 text-sm text-pretty">
                <p className="text-muted-foreground">You said &ldquo;{m.you_said}&rdquo;</p>
                <p className="mt-1">
                  <span className="font-medium">Try:</span> {m.try}
                </p>
              </li>
            ))}
          </ul>
        </div>
      )}
      {p?.situation && (
        <div className="rounded-xl bg-muted/60 p-3">
          <MicroLabel>What {firstName(s)} was thinking</MicroLabel>
          <p className="mt-1 text-sm text-pretty">{p.situation}</p>
          <div className="mt-2 grid gap-3 sm:grid-cols-2">
            <List title="Cared about" items={p.cares_about ?? []} />
            <List title="Objections" items={p.objections ?? []} />
          </div>
          {p.agrees_if && (
            <p className="mt-2 text-sm text-pretty">
              <span className="font-medium">Would agree to a next step if:</span> {p.agrees_if}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
