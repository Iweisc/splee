"use client";

import { useState } from "react";
import { ArrowRight, ExternalLink, Mail, Sparkles, Video } from "lucide-react";
import { Button, LinkButton } from "@/components/ui/button";
import { Badge } from "@/components/ui/misc";
import { api } from "@/lib/api";
import { MEETING_NAMES, type Meeting } from "@/lib/types";
import { cn, dateTime } from "@/lib/utils";
import { toastError } from "./states";

/** "45 min", or "1h 10m" past an hour. */
function length(secs: number) {
  const m = Math.round(secs / 60);
  return m < 60 ? `${m} min` : `${Math.floor(m / 60)}h ${m % 60}m`;
}

const FOLLOW_UP: Record<string, string> = {
  draft: "Follow-up drafted and waiting for your approval.",
  approved: "Follow-up approved and on its way.",
  scheduled: "Follow-up approved and on its way.",
  sending: "Follow-up approved and on its way.",
  sent: "Follow-up sent.",
  cancelled: "Follow-up skipped.",
};

/** A sales meeting in a lead's timeline: who was there, the agent's summary and next steps, the drafted follow-up, and the
 *  deal stage the agent suggests, which only moves the deal when someone accepts it. */
export function MeetingCard({ meeting: m, onChange, className }: { meeting: Meeting; onChange: () => unknown; className?: string }) {
  const [busy, setBusy] = useState<"accept" | "dismiss" | null>(null);
  const decide = async (accept: boolean) => {
    setBusy(accept ? "accept" : "dismiss");
    try {
      await api(`/meetings/${m.id}/stage`, { method: "POST", body: { accept } });
      await onChange();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const who = m.attendees.map((a) => a.name || a.email).filter(Boolean);
  const provider = MEETING_NAMES[m.provider];
  return (
    <div className={cn("flex flex-col gap-2 rounded-xl border border-border bg-card p-3", className)}>
      <div className="flex flex-wrap items-center justify-between gap-x-2 gap-y-1 text-xs text-muted-foreground">
        <span className="flex items-center gap-1.5">
          <Video className="size-3.5" />
          {provider} meeting{m.duration_secs ? ` · ${length(m.duration_secs)}` : ""}
          {m.booking_id && <Badge tone="blue">Booked in Splee</Badge>}
        </span>
        <span>{dateTime(m.started_at)}</span>
      </div>
      <div>
        <p className="text-sm font-medium text-pretty">{m.title || "Meeting"}</p>
        {who.length > 0 && <p className="text-xs text-pretty text-muted-foreground">With {who.join(", ")}</p>}
      </div>
      {m.summary ? (
        <div className="rounded-lg bg-brand-50/70 px-3 py-2 text-sm">
          <p className="mb-1 flex items-center gap-1.5 text-xs font-medium text-brand-700">
            <Sparkles className="size-3.5" /> Summary
          </p>
          <p className="text-pretty">{m.summary}</p>
          {m.next_steps.length > 0 && (
            <>
              <p className="mt-2 text-xs font-medium text-brand-700">Next steps</p>
              <ul className="mt-1 flex list-disc flex-col gap-0.5 pl-4">
                {m.next_steps.map((step) => (
                  <li key={step} className="text-pretty">
                    {step}
                  </li>
                ))}
              </ul>
            </>
          )}
        </div>
      ) : (
        !m.summarized_at && <p className="text-sm text-muted-foreground">The agent is writing notes on this meeting…</p>
      )}
      {m.stage_status === "pending" && m.suggested_stage && (
        <div className="flex flex-col gap-2 rounded-lg border border-border px-3 py-2 text-sm sm:flex-row sm:items-center">
          <p className="flex-1 text-pretty">
            <span className="font-medium">Move the deal to {m.suggested_stage}?</span> {m.stage_reason}
          </p>
          <div className="flex shrink-0 gap-2 max-sm:[&>*]:flex-1">
            <Button size="sm" variant="ghost" onClick={() => decide(false)} loading={busy === "dismiss"} disabled={!!busy}>
              Dismiss
            </Button>
            <Button size="sm" onClick={() => decide(true)} loading={busy === "accept"} disabled={!!busy}>
              <ArrowRight className="size-3.5" /> Move
            </Button>
          </div>
        </div>
      )}
      {m.stage_status === "accepted" && m.suggested_stage && <p className="text-xs text-muted-foreground">Deal moved to {m.suggested_stage}.</p>}
      {m.follow_up_status && (
        <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
          <span className="flex items-center gap-1.5 text-muted-foreground">
            <Mail className="size-3.5 shrink-0" /> {FOLLOW_UP[m.follow_up_status] ?? "Follow-up written."}
          </span>
          {m.follow_up_status === "draft" && (
            <LinkButton href={`/app/p/${m.project_id}/emails`} size="sm" variant="outline">
              Review
            </LinkButton>
          )}
        </div>
      )}
      {(m.provider_summary || m.recording_url) && (
        <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
          {m.provider_summary && (
            <details className="w-full text-muted-foreground">
              <summary className="cursor-pointer">{provider}&apos;s own notes</summary>
              <p className="mt-1 whitespace-pre-wrap text-foreground/80">{m.provider_summary}</p>
            </details>
          )}
          {m.recording_url && (
            <a href={m.recording_url} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-muted-foreground hover:underline">
              Open in {provider} <ExternalLink className="size-3" />
            </a>
          )}
        </div>
      )}
    </div>
  );
}
