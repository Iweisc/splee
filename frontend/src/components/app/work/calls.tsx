"use client";

import { useState } from "react";
import { Phone, PhoneMissed, Sparkles } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Textarea } from "@/components/ui/form";
import { Badge, type BadgeTone } from "@/components/ui/misc";
import type { Call, CallOutcome } from "@/lib/types";
import { cn, dateTime } from "@/lib/utils";

export const OUTCOMES: Record<CallOutcome, { label: string; tone: BadgeTone }> = {
  connected: { label: "Connected", tone: "green" },
  meeting_booked: { label: "Meeting booked", tone: "green" },
  voicemail: { label: "Left a voicemail", tone: "blue" },
  no_answer: { label: "No answer", tone: "neutral" },
  not_interested: { label: "Not interested", tone: "amber" },
  wrong_number: { label: "Wrong number", tone: "red" },
};

/** "4:05", or "1:02:09" past an hour. */
export function clock(secs: number) {
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = String(secs % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${s}` : `${m}:${s}`;
}

/** What the call ended as on Twilio's side, as the outcome it most likely was. */
export function likelyOutcome(callStatus: string): CallOutcome | null {
  if (callStatus === "no-answer" || callStatus === "busy") return "no_answer";
  return null;
}

/** The outcome and notes someone logs after a call. Wrong number clears the lead's phone, and a booking or a no ends the sequence. */
export function OutcomeForm({
  initial,
  onSave,
  onCancel,
  cancelLabel = "Not now",
}: {
  initial?: CallOutcome | null;
  onSave: (outcome: CallOutcome, notes: string) => Promise<void>;
  onCancel?: () => void;
  cancelLabel?: string;
}) {
  const [outcome, setOutcome] = useState<CallOutcome | null>(initial ?? null);
  const [notes, setNotes] = useState("");
  const [busy, setBusy] = useState(false);
  const save = async () => {
    if (!outcome) return;
    setBusy(true);
    try {
      await onSave(outcome, notes.trim());
    } finally {
      setBusy(false);
    }
  };
  const hint: Partial<Record<CallOutcome, string>> = {
    meeting_booked: "Moves them to Meeting booked and stops the rest of the sequence.",
    not_interested: "Marks them not interested and stops the rest of the sequence.",
    wrong_number: "Clears their number, so no more calls or texts go to it.",
  };
  return (
    <div className="flex flex-col gap-3">
      <div role="radiogroup" aria-label="What happened" className="grid grid-cols-2 gap-2">
        {(Object.keys(OUTCOMES) as CallOutcome[]).map((o) => (
          <button
            key={o}
            type="button"
            role="radio"
            aria-checked={outcome === o}
            onClick={() => setOutcome(o)}
            className={cn(
              "rounded-lg border px-3 py-2 text-left text-sm transition-colors",
              outcome === o ? "border-primary bg-primary text-primary-foreground" : "border-border bg-card hover:bg-muted",
            )}
          >
            {OUTCOMES[o].label}
          </button>
        ))}
      </div>
      {outcome && hint[outcome] && <p className="text-xs text-pretty text-muted-foreground">{hint[outcome]}</p>}
      <Textarea
        value={notes}
        onChange={(e) => setNotes(e.target.value)}
        placeholder="Notes: what they said, and what you agreed"
        className="min-h-[calc(3lh+1.125rem)] field-sizing-content"
        aria-label="Notes"
      />
      <div className="flex justify-end gap-2">
        {onCancel && (
          <Button variant="ghost" onClick={onCancel} disabled={busy}>
            {cancelLabel}
          </Button>
        )}
        <Button onClick={save} loading={busy} disabled={!outcome}>
          Save
        </Button>
      </div>
    </div>
  );
}

/** A call in a lead's timeline: what happened, how long, the recording, the notes and the agent's summary. */
export function CallCard({ call: c, className }: { call: Call; className?: string }) {
  const outcome = c.outcome ? OUTCOMES[c.outcome] : null;
  const missed = c.status === "skipped" || c.status === "cancelled";
  return (
    <div className={cn("flex flex-col gap-2 rounded-xl border border-border p-3", missed ? "bg-muted/60" : "bg-card", className)}>
      <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          {missed ? <PhoneMissed className="size-3.5" /> : <Phone className="size-3.5" />}
          {c.kind === "sequence" ? `Call step ${c.step}` : "Call"}
          {c.duration_secs ? ` · ${clock(c.duration_secs)}` : ""}
        </span>
        <span className="flex items-center gap-2">
          {outcome ? (
            <Badge tone={outcome.tone}>{outcome.label}</Badge>
          ) : (
            <Badge tone={c.status === "open" ? "blue" : "neutral"}>{c.status === "open" ? "To call" : c.status === "skipped" ? "Skipped" : "Cancelled"}</Badge>
          )}
          {dateTime(c.called_at ?? c.closed_at ?? c.due_at)}
        </span>
      </div>
      {missed && c.cancel_reason && <p className="text-sm text-muted-foreground">{c.cancel_reason}</p>}
      {c.status === "open" && c.instructions && <p className="text-sm text-muted-foreground">{c.instructions}</p>}
      {c.notes && <p className="text-sm whitespace-pre-wrap">{c.notes}</p>}
      {c.summary && (
        <div className="rounded-lg bg-brand-50/70 px-3 py-2 text-sm">
          <p className="mb-1 flex items-center gap-1.5 text-xs font-medium text-brand-700">
            <Sparkles className="size-3.5" /> Summary
          </p>
          <p className="text-pretty">{c.summary}</p>
          {c.next_step && (
            <p className="mt-1.5 text-pretty">
              <span className="font-medium">Next: </span>
              {c.next_step}
            </p>
          )}
        </div>
      )}
      {c.recording_url && (
        <audio controls preload="none" src={`/api/calls/${c.id}/recording`} className="h-9 w-full" />
      )}
    </div>
  );
}
