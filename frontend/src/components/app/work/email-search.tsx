import { Loader2 } from "lucide-react";
import { Badge, type BadgeTone } from "@/components/ui/misc";
import type { EmailSearchProgress, Lead } from "@/lib/types";
import { cn, plural } from "@/lib/utils";

const RESULT: Record<Exclude<Lead["email_search"], "">, { label: (l: Lead) => string; tone: BadgeTone }> = {
  queued: { label: () => "Finding email", tone: "neutral" },
  found: { label: (l) => `Found via ${l.email_source}`, tone: "green" },
  guessed: { label: () => "Guessed and verified", tone: "green" },
  catch_all: { label: () => "Catch-all domain", tone: "amber" },
  not_found: { label: () => "No email found", tone: "neutral" },
  suppressed: { label: () => "Do not contact", tone: "red" },
  duplicate: { label: () => "Already a lead", tone: "amber" },
  failed: { label: () => "Search failed", tone: "red" },
};

/** How finding the lead's email went, with why in the tooltip. Nothing for a lead that was never searched. */
export function EmailSearchBadge({ lead, className }: { lead: Lead; className?: string }) {
  if (!lead.email_search) return null;
  const r = RESULT[lead.email_search];
  return (
    <Badge tone={r.tone} title={lead.email_search_note || undefined} className={cn("max-w-full", className)}>
      {lead.email_search === "queued" && <Loader2 className="size-3 animate-spin" />}
      <span className="truncate">{r.label(lead)}</span>
    </Badge>
  );
}

/** "2 lookups, 1 credit charged.", or "" before any lookup. Only some services say what they charged. */
export function searchCost(lead: Lead): string {
  if (!lead.email_search_lookups) return "";
  const credits = lead.email_search_credits === null ? "" : `, ${plural(lead.email_search_credits, "credit")} charged`;
  return `${plural(lead.email_search_lookups, "lookup")}${credits}.`;
}

/** The leads waiting for an email search. `total` is how many this page queued, 0 when it doesn't know, like after a reload. */
export function EmailSearchStrip({ progress, total }: { progress: EmailSearchProgress; total: number }) {
  const known = total >= progress.queued && total > 0;
  const done = known ? total - progress.queued : 0;
  return (
    <div className="border-b border-border px-4 py-3 text-sm" role="status">
      <p className="flex items-center gap-2 font-medium">
        <Loader2 className="size-3.5 shrink-0 animate-spin text-muted-foreground" />
        {known ? `Finding emails: ${done} of ${plural(total, "lead")} done` : `Finding emails for ${plural(progress.queued, "lead")}`}
      </p>
      {known && (
        <div className="mt-2 h-1.5 overflow-hidden rounded-full bg-black/5">
          <div className="h-full rounded-full bg-brand-500 transition-[width]" style={{ width: `${Math.max(4, (done / total) * 100)}%` }} />
        </div>
      )}
      {progress.queued > progress.room && (
        <p className="mt-2 text-xs text-muted-foreground">
          Splee searches up to {progress.hourly} leads an hour, so some wait for the next hour.
        </p>
      )}
    </div>
  );
}
