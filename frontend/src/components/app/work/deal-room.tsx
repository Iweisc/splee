"use client";

import { useState } from "react";
import { DoorOpen, ExternalLink, Loader2, MessageCircleQuestion, PenLine, RefreshCw, Trash2 } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/form";
import { CopyButton } from "@/components/app/settings/copy-button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, DealRoom, DealRoomQuestion } from "@/lib/types";
import { timeAgo } from "@/lib/utils";
import { toastError } from "./states";

/** In an open deal's window: the buyer's deal room, and their questions with the agent's draft answers to publish. */
export function DealRoomSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/room`;
  const { data: room, mutate } = useSWR<DealRoom | null>(deal.status === "open" ? key : null, fetcher, {
    // Slowly otherwise, so a question the buyer asks while the window is open shows up.
    refreshInterval: (r) => (r?.status === "writing" || r?.questions.some((q) => q.drafting) ? 2500 : 30000),
  });
  const [busy, setBusy] = useState<string | null>(null);
  if (room === undefined || deal.status !== "open") return null;
  const them = deal.company_name || deal.lead_name || "them";

  const act = async (what: string, call: () => Promise<DealRoom | null>) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => api<DealRoom | null>(key, { method: "POST" }));
  const writing = room?.status === "writing";

  if (!room?.room) {
    return (
      <section className="flex items-start justify-between gap-3 rounded-xl border border-dashed border-border-strong p-4">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <DoorOpen className="size-3.5" /> Deal room
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? "The agent is writing the welcome and picking the case studies closest to them…"
              : room?.status === "failed"
                ? "Couldn't write the room. Try again."
                : `One page for ${them} with a welcome, the close plan and proposal, the case studies from Materials closest to them, and a place to ask anything. The agent drafts an answer to each question for you to publish.`}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
            <PenLine className="size-3.5" /> Make a deal room
          </Button>
        )}
      </section>
    );
  }

  const waiting = room.questions.filter((q) => !q.answer).length;
  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <DoorOpen className="size-3.5" /> Deal room
        </p>
        {waiting > 0 && <span className="rounded-full bg-amber-100 px-2 py-0.5 text-xs font-medium text-amber-800">{waiting} to answer</span>}
        {/* The three don't fit one line of a phone's dialog, so they wrap rather than run past the card. */}
        <div className="ml-auto flex flex-wrap items-center justify-end gap-1">
          {writing ? (
            <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Writing…
            </span>
          ) : (
            <Button type="button" size="sm" variant="ghost" onClick={write} loading={busy === "write"} title="Keeps the link and the questions">
              <RefreshCw className="size-3.5" /> Write again
            </Button>
          )}
          <a
            href={`${room.url}?preview=1`}
            target="_blank"
            rel="noreferrer"
            className="inline-flex h-8 items-center gap-1.5 rounded-lg px-3 text-sm font-medium hover:bg-accent"
          >
            <ExternalLink className="size-3.5" /> Open
          </a>
          <CopyButton value={room.url} label="Copy link" />
        </div>
      </div>
      <p className="mt-2 text-sm font-medium text-pretty">{room.room.headline}</p>
      <p className="mt-0.5 text-sm text-pretty text-muted-foreground">{room.room.intro}</p>
      {room.proof.length > 0 && (
        <ul className="mt-2 flex flex-wrap gap-1.5">
          {room.proof.map((p) => (
            <li key={p.material_id} title={p.why} className="rounded-full border border-border px-2 py-0.5 text-xs">
              {p.title}
            </li>
          ))}
        </ul>
      )}
      <p className="mt-2 text-xs text-muted-foreground">
        {room.last_viewed_at
          ? `${deal.lead_name || them} last looked ${timeAgo(room.last_viewed_at)}, ${room.views === 1 ? "once" : `${room.views} times`}.`
          : `Not opened yet. Send ${deal.lead_name || them} the link.`}
      </p>
      {room.questions.length > 0 && (
        <ol className="mt-3 flex flex-col gap-3 border-t border-border pt-3">
          {room.questions.map((q) => (
            <QuestionRow key={q.id} q={q} busy={busy} onAct={act} />
          ))}
        </ol>
      )}
    </section>
  );
}

function QuestionRow({
  q,
  busy,
  onAct,
}: {
  q: DealRoomQuestion;
  busy: string | null;
  onAct: (what: string, call: () => Promise<DealRoom | null>) => Promise<void>;
}) {
  const [text, setText] = useState(q.answer ?? q.draft);
  // The draft arrives after the question does. Take it unless someone already started typing.
  const [seenDraft, setSeenDraft] = useState(q.draft);
  if (q.draft !== seenDraft) {
    setSeenDraft(q.draft);
    if (!text.trim()) setText(q.draft);
  }
  const save = (answer: string) => onAct(q.id, () => api<DealRoom | null>(`/room-questions/${q.id}`, { body: { answer } }));
  const edited = text.trim() !== (q.answer ?? "").trim();
  return (
    <li className="flex flex-col gap-2">
      <div className="flex items-start gap-2">
        <MessageCircleQuestion className="mt-0.5 size-3.5 shrink-0 text-muted-foreground" />
        <p className="min-w-0 flex-1 text-sm text-pretty">
          <span className="font-medium">{q.name}</span> <span className="text-muted-foreground">asked {timeAgo(q.created_at)}:</span> {q.question}
        </p>
        <Button
          type="button"
          size="icon"
          variant="ghost"
          className="size-7"
          aria-label="Delete the question"
          onClick={() => onAct(q.id, () => api<DealRoom | null>(`/room-questions/${q.id}`, { method: "DELETE" }))}
        >
          <Trash2 className="size-3.5" />
        </Button>
      </div>
      {q.drafting && !text ? (
        <p className="flex items-center gap-1.5 pl-5.5 text-xs text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" /> The agent is drafting an answer…
        </p>
      ) : (
        <div className="flex flex-col gap-2 pl-5.5">
          <Textarea
            value={text}
            onChange={(e) => setText(e.target.value)}
            maxLength={4000}
            rows={2}
            aria-label={`Answer to ${q.name}`}
            className="min-h-[calc(2lh+1.125rem)] resize-none text-sm field-sizing-content"
          />
          <div className="flex flex-wrap items-center justify-end gap-2">
            {q.answer && <span className="mr-auto text-xs text-muted-foreground">On the page {q.answered_at ? timeAgo(q.answered_at) : ""}</span>}
            {q.answer && (
              <Button type="button" size="sm" variant="ghost" onClick={() => save("")} disabled={busy === q.id}>
                Take it down
              </Button>
            )}
            {(!q.answer || edited) && (
              <Button type="button" size="sm" onClick={() => save(text)} loading={busy === q.id} disabled={!text.trim()}>
                {q.answer ? "Update the answer" : "Publish the answer"}
              </Button>
            )}
          </div>
        </div>
      )}
    </li>
  );
}
