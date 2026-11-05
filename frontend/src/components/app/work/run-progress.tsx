"use client";

import { Check, CircleAlert, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { AgentEvent, AgentRun, RunKind } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { toastError } from "./states";

export const EXPLORE_STEPS = [
  "Research your company",
  "Explore competitors",
  "Define campaigns",
  "Find potential customers",
  "Find decision makers",
  "Write personal emails",
];

const RUN_TITLES: Partial<Record<RunKind, string>> = {
  find_companies: "Find companies",
  find_people: "Find decision makers",
  write_emails: "Write personal emails",
  segment_from_prompt: "Draft campaign",
};

// Reply classification and LinkedIn runs are short background jobs. Showing them as progress would be noise.
const VISIBLE_KINDS = new Set<RunKind>(["explore", "find_companies", "find_people", "write_emails", "segment_from_prompt"]);

export function isActive(run: AgentRun) {
  return run.status === "queued" || run.status === "running";
}

/** The project's agent runs plus the first visible one that is still going. */
export function useProjectRuns(projectId: string) {
  const { data, mutate } = useSWR<AgentRun[]>(`/projects/${projectId}/runs`, fetcher);
  const active = data?.find((r) => VISIBLE_KINDS.has(r.kind) && isActive(r)) ?? null;
  return { runs: data, active, mutate };
}

type StepState = "pending" | "active" | "done" | "failed";

/**
 * Live view of an agent run, styled after the Explee demo sidebar.
 * Every finished step revalidates all SWR data so partial results show up while the run continues.
 */
export function RunProgress({ run, className }: { run: AgentRun; className?: string }) {
  const { mutate } = useSWRConfig();
  const { events, finished } = useAgentEvents(`/api/runs/${run.id}/events`, (e) => {
    if ((e.kind === "phase" && e.data.status === "finished") || e.kind === "done" || e.kind === "error") {
      mutate(() => true);
    }
  });

  const titles = run.kind === "explore" ? EXPLORE_STEPS : [RUN_TITLES[run.kind] ?? run.kind];
  const stepOf = (e: AgentEvent) => (run.kind === "explore" ? Math.max(1, e.step) : 1);

  const error = events.find((e) => e.kind === "error");
  const done = events.some((e) => e.kind === "done") || (!error && run.status === "succeeded");
  const failed = !!error || run.status === "failed" || run.status === "cancelled";
  const ended = finished || done || failed;
  const current = Math.max(run.kind === "explore" ? run.step : 0, ...events.map(stepOf), 1);

  const summaries = new Map<number, string>();
  for (const e of events) {
    if (e.kind === "phase" && e.data.status === "finished" && e.message) summaries.set(stepOf(e), e.message);
  }

  const stateOf = (step: number): StepState => {
    if (done || summaries.has(step) || step < current) return "done";
    if (step === current) return failed ? "failed" : "active";
    return "pending";
  };

  const logs = events.filter((e) => (e.kind === "log" || e.kind === "tool") && stepOf(e) === current).slice(-5);

  const cancel = async () => {
    try {
      await api(`/runs/${run.id}/cancel`, { method: "POST" });
      mutate(() => true);
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <Card className={cn("p-5", className)}>
      <div className="mb-4 flex items-center justify-between gap-3">
        <p className="flex items-center gap-2 font-mono text-xs text-muted-foreground">
          {ended ? (
            failed ? (
              <CircleAlert className="size-3.5 text-danger" />
            ) : (
              <Check className="size-3.5 text-brand-600" />
            )
          ) : (
            <span className="relative flex size-2">
              <span className="absolute inline-flex size-full animate-ping rounded-full bg-brand-500 opacity-60" />
              <span className="relative inline-flex size-2 rounded-full bg-brand-500" />
            </span>
          )}
          {ended
            ? failed
              ? "Splee agent stopped"
              : "Splee agent finished"
            : run.status === "queued" && events.length === 0
              ? "Splee agent is queued"
              : titles.length > 1
                ? `Splee agent is working on step ${current} of ${titles.length}`
                : "Splee agent is working"}
        </p>
        {!ended && (
          <Button variant="ghost" size="sm" onClick={cancel}>
            <X className="size-3.5" /> Cancel
          </Button>
        )}
      </div>

      <ol className="flex flex-col">
        {titles.map((title, i) => {
          const step = i + 1;
          const state = stateOf(step);
          return (
            <li key={step} className={cn("border-t border-border py-2.5 first:border-t-0 first:pt-0", state === "pending" && "opacity-45")}>
              <p className="flex items-center gap-2 font-mono text-xs">
                <StepMark state={state} step={step} />
                <span className={state === "active" ? "text-foreground" : "text-muted-foreground"}>
                  {titles.length > 1 && <>step {step} · </>}
                  {title}
                </span>
              </p>
              {state === "done" && summaries.get(step) && <p className="mt-1 pl-6 text-sm text-foreground/80">{summaries.get(step)}</p>}
              {(state === "active" || state === "failed") && logs.length > 0 && (
                <ul className="mt-2 flex flex-col gap-1 pl-6 font-mono text-xs text-muted-foreground">
                  {logs.map((e, j) => (
                    <li key={e.id} className={cn("truncate", j === logs.length - 1 && state === "active" && "text-foreground")}>
                      {j === logs.length - 1 ? "› " : "✓ "}
                      {e.message || String(e.data.name ?? "")}
                      {j === logs.length - 1 && state === "active" && (
                        <span className="ml-1 inline-block h-3 w-1.5 translate-y-0.5 animate-blink bg-foreground" />
                      )}
                    </li>
                  ))}
                </ul>
              )}
            </li>
          );
        })}
      </ol>

      {failed && <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700">{error?.message || run.error || "The run was stopped."}</p>}
    </Card>
  );
}

function StepMark({ state, step }: { state: StepState; step: number }) {
  if (state === "done") return <Check className="size-4 text-brand-600" />;
  if (state === "failed") return <CircleAlert className="size-4 text-danger" />;
  return (
    <span
      className={cn(
        "inline-flex size-4 items-center justify-center rounded-full text-[10px]",
        state === "active" ? "bg-primary text-primary-foreground" : "border border-border-strong",
      )}
    >
      {step}
    </span>
  );
}
