import { Fragment } from "react";
import { ArrowRight, Check, CircleAlert, CreditCard } from "lucide-react";
import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { STEPS } from "./steps";

/** Finished steps are black dots you can click, the viewed step is a pill, future steps are outlined numbers. */
export function Stepper({
  finished,
  viewed,
  running,
  failed,
  onSelect,
}: {
  finished: Set<number>;
  viewed: number;
  running: boolean;
  failed: boolean;
  onSelect: (step: number) => void;
}) {
  return (
    <ol className="flex items-center px-5 pt-[42px] pb-6 md:px-9">
      {STEPS.map((s, i) => (
        <Fragment key={s.n}>
          {i > 0 && <li aria-hidden className={cn("mx-2 h-px min-w-3 flex-1", finished.has(s.n - 1) ? "bg-foreground/70" : "bg-border-strong")} />}
          <li className="shrink-0">
            {s.n === viewed ? (
              <button
                type="button"
                onClick={() => onSelect(s.n)}
                aria-current="step"
                className="inline-flex h-[26px] items-center gap-2 rounded-full border border-foreground/80 bg-card px-3 text-sm font-semibold whitespace-nowrap"
              >
                {failed ? (
                  <CircleAlert className="size-3.5 text-danger" />
                ) : running ? (
                  <span className="size-1.5 animate-pulse rounded-full bg-brand-500" />
                ) : (
                  <Check className="size-3.5 text-brand-600" strokeWidth={3} />
                )}
                {s.n}
                <span className="hidden sm:inline">{s.title}</span>
              </button>
            ) : finished.has(s.n) ? (
              <button
                type="button"
                onClick={() => onSelect(s.n)}
                aria-label={`Show step ${s.n}: ${s.title}`}
                className="flex size-[22px] items-center justify-center rounded-full bg-foreground text-xs font-semibold text-primary-foreground transition-transform hover:scale-110"
              >
                {s.n}
              </button>
            ) : (
              <span className="flex size-[22px] items-center justify-center rounded-full border-[1.5px] border-foreground/45 text-xs font-semibold text-muted-foreground">
                {s.n}
              </span>
            )}
          </li>
        </Fragment>
      ))}
    </ol>
  );
}

export function CreditsBanner({ onStart, loading }: { onStart: () => void; loading: boolean }) {
  return (
    <div className="sticky top-0 z-20 flex flex-wrap items-center gap-4 border-b border-border bg-muted px-5 py-3.5">
      <span className="flex size-10 shrink-0 items-center justify-center rounded-lg border border-border-strong bg-card">
        <CreditCard className="size-5 text-muted-foreground" />
      </span>
      <div className="min-w-0 flex-1">
        <p className="text-[17px] font-semibold">$30 free credits — get new clients on autopilot</p>
        <p className="text-sm text-muted-foreground">No upfront charge · 750 emails included · Start outbound in 60s</p>
      </div>
      <Button variant="brand" onClick={onStart} loading={loading} className="h-11 rounded-lg px-5 font-semibold">
        Start outreach for free
        <ArrowRight className="size-4" />
      </Button>
    </div>
  );
}
