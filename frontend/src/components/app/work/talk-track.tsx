"use client";

import { useState } from "react";
import { ChevronDown, GraduationCap, Loader2, RefreshCw, ScrollText } from "lucide-react";
import { Button } from "@/components/ui/button";
import { api } from "@/lib/api";
import type { CallPrep, CallRow } from "@/lib/types";
import { cn } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

/** What to say on the call. See backend/src/agent/call_prep.rs. */
export function TalkTrack({ prep: p, ringing }: { prep: CallPrep; ringing?: boolean }) {
  // While it rings the caller needs the opener and the voicemail. The rest is for once they pick up.
  const voicemail = p.voicemail && (
    <div>
      <MicroLabel className="mb-1">If it goes to voicemail</MicroLabel>
      <p className="text-sm text-pretty text-muted-foreground">{p.voicemail}</p>
    </div>
  );
  return (
    <div className="flex flex-col gap-3">
      {p.reminder && (
        <p className="flex items-start gap-2 rounded-lg bg-amber-50 px-3 py-2 text-sm text-pretty text-amber-900">
          <GraduationCap className="mt-0.5 size-3.5 shrink-0" /> {p.reminder}
        </p>
      )}
      <div>
        <MicroLabel className="mb-1">Open with</MicroLabel>
        <p className="text-sm font-medium text-pretty">&ldquo;{p.opener}&rdquo;</p>
      </div>
      {ringing && voicemail}
      {p.questions.length > 0 && (
        <div>
          <MicroLabel className="mb-1">Ask</MicroLabel>
          <ol className="flex list-decimal flex-col gap-1 pl-5 text-sm text-pretty">
            {p.questions.map((q) => (
              <li key={q}>{q}</li>
            ))}
          </ol>
        </div>
      )}
      {p.objections.length > 0 && (
        <div>
          <MicroLabel className="mb-1">If they say</MicroLabel>
          <ul className="flex flex-col gap-2">
            {p.objections.map((o) => (
              <li key={o.objection} className="text-sm text-pretty">
                <p className="font-medium">&ldquo;{o.objection}&rdquo;</p>
                <p className="text-muted-foreground">{o.answer}</p>
              </li>
            ))}
          </ul>
        </div>
      )}
      {!ringing && voicemail}
    </div>
  );
}

/** On an open call's row: the talk track behind a toggle, or a button to write it. */
export function TalkTrackRow({ row, onChanged }: { row: CallRow; onChanged: () => Promise<unknown> }) {
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const write = async () => {
    setBusy(true);
    try {
      await api(`/calls/${row.id}/prep`, { method: "POST" });
      await onChanged();
      setOpen(true);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const status = row.prep?.status;
  const prep = row.prep?.prep;

  if (status === "writing") {
    return (
      <p className="flex items-center gap-2 text-sm text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" /> Writing the talk track…
      </p>
    );
  }
  if (!prep) {
    return (
      <div className="flex items-center gap-3">
        <Button size="sm" variant="ghost" onClick={write} loading={busy} className="-ml-2 px-2">
          <ScrollText className="size-3.5" /> Write a talk track
        </Button>
        {status === "failed" && <span className="text-sm text-danger">The agent couldn&apos;t write it. Try again.</span>}
      </div>
    );
  }
  return (
    <div className="rounded-lg border border-border">
      <div className="flex items-center gap-2 px-3 py-2">
        <button
          type="button"
          onClick={() => setOpen(!open)}
          aria-expanded={open}
          className="flex min-w-0 flex-1 items-center gap-2 text-left text-sm"
        >
          <ScrollText className="size-3.5 shrink-0 text-brand-700" />
          <span className={cn("min-w-0 flex-1", !open && "truncate")}>
            <span className="font-medium">Talk track</span>
            {!open && <span className="text-muted-foreground"> · &ldquo;{prep.opener}&rdquo;</span>}
          </span>
          <ChevronDown className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
        </button>
        {open && (
          <Button size="sm" variant="ghost" onClick={write} loading={busy} className="px-2">
            <RefreshCw className="size-3.5" /> Write again
          </Button>
        )}
      </div>
      {open && (
        <div className="border-t border-border px-3 py-3">
          <TalkTrack prep={prep} />
        </div>
      )}
    </div>
  );
}
