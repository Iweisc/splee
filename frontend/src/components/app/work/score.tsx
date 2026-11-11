"use client";

import { ArrowDown, ChevronDown, CircleCheck, CircleMinus, Flame } from "lucide-react";
import { Select } from "@/components/ui/form";
import type { ScoreReason, Scored } from "@/lib/types";
import { cn } from "@/lib/utils";

/** The choices of the "Score at least" filter. */
const MIN_SCORES = [20, 40, 60, 80];

const tooltip = (s: Scored) => `Score ${s.score} of 100: fit ${s.score_fit} of 50, intent ${s.score_intent} of 50`;

/** Fit then intent on one 0-100 track, so the bar's length is the score and its colors say where it came from. */
export function ScoreBar({ fit, intent, className }: { fit: number; intent: number; className?: string }) {
  return (
    <span className={cn("flex h-1.5 w-12 shrink-0 overflow-hidden rounded-full bg-black/10", className)} aria-hidden>
      <span className="h-full bg-brand-500" style={{ width: `${fit}%` }} />
      <span className="h-full bg-amber-400" style={{ width: `${intent}%` }} />
    </span>
  );
}

/** The score with its bar, as a button that opens the row's reasons. */
export function ScoreButton({ item, open, onToggle }: { item: Scored; open: boolean; onToggle: () => void }) {
  return (
    <button
      type="button"
      onClick={(e) => {
        // The row opens the lead, and this opens the reasons instead.
        e.stopPropagation();
        onToggle();
      }}
      aria-expanded={open}
      title={tooltip(item)}
      aria-label={`${tooltip(item)}. ${open ? "Hide" : "Show"} why`}
      className="-mx-1.5 flex items-center gap-1.5 rounded-md px-1.5 py-1 hover:bg-black/5 sm:gap-2"
    >
      {/* Wide enough for 100, so scores line up down the column. */}
      <span className="min-w-7 text-right text-sm font-medium whitespace-nowrap tabular-nums">{item.score}</span>
      {/* A phone has no room for the full bar and the chevron beside the names, and the button still opens the reasons. */}
      <ScoreBar fit={item.score_fit} intent={item.score_intent} className="w-8 sm:w-12" />
      <ChevronDown className={cn("size-3.5 text-muted-foreground transition-transform max-sm:hidden", open && "rotate-180")} />
    </button>
  );
}

const KIND_ICON: Record<ScoreReason["kind"], React.ReactNode> = {
  fit: <CircleCheck className="size-3.5 shrink-0 text-brand-600" />,
  intent: <Flame className="size-3.5 shrink-0 text-amber-600" />,
  against: <CircleMinus className="size-3.5 shrink-0 text-red-600" />,
};

export function ReasonList({ reasons, className }: { reasons: ScoreReason[]; className?: string }) {
  return (
    <ul className={cn("flex flex-wrap gap-x-5 gap-y-1.5 text-sm", className)}>
      {reasons.map((r) => (
        <li key={r.text} className="flex items-center gap-1.5">
          {KIND_ICON[r.kind]}
          {r.text}
        </li>
      ))}
    </ul>
  );
}

/** What an opened row shows under it: the two parts, then each reason. */
export function ScoreReasons({ item }: { item: Scored }) {
  return (
    <div className="flex flex-col gap-2 sm:flex-row sm:items-start sm:gap-6">
      <p className="flex shrink-0 gap-4 text-xs whitespace-nowrap text-muted-foreground tabular-nums sm:pt-0.5">
        <span className="flex items-center gap-1.5">
          <span className="size-2 rounded-full bg-brand-500" /> Fit {item.score_fit}/50
        </span>
        <span className="flex items-center gap-1.5">
          <span className="size-2 rounded-full bg-amber-400" /> Intent {item.score_intent}/50
        </span>
      </p>
      {item.score_reasons.length ? (
        <ReasonList reasons={item.score_reasons} />
      ) : (
        <p className="text-sm text-muted-foreground">
          {item.scored_at ? "Nothing in your targeting or signals points to them yet." : "Not scored yet. Scores update every few minutes."}
        </p>
      )}
    </div>
  );
}

/** A table header that sorts by score, highest first, and back to the list's own order on a second click. */
export function ScoreHeader({ sorted, onSort }: { sorted: boolean; onSort: () => void }) {
  return (
    <button
      type="button"
      onClick={onSort}
      aria-pressed={sorted}
      title={sorted ? "Back to the usual order" : "Highest score first"}
      className={cn("inline-flex items-center gap-1 uppercase hover:text-foreground", sorted && "text-foreground")}
    >
      Score
      <ArrowDown className={cn("size-3", !sorted && "opacity-40")} />
    </button>
  );
}

export function ScoreFilter({ value, onChange, className }: { value: string; onChange: (v: string) => void; className?: string }) {
  return (
    <Select value={value} onChange={(e) => onChange(e.target.value)} className={className} aria-label="Score at least">
      <option value="">Any score</option>
      {MIN_SCORES.map((n) => (
        <option key={n} value={n}>
          Score at least {n}
        </option>
      ))}
    </Select>
  );
}
