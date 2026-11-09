"use client";

import { use, useState } from "react";
import { Bot } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ListSkeleton, LoadError, Td, Th, Tr, toastError } from "@/components/app/work/states";
import { Badge, Card, EmptyState, type BadgeTone } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { AgentRun, RunKind, SegmentWithStats } from "@/lib/types";
import { cn, dateTime, money, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

const TITLES: Record<RunKind, string> = {
  explore: "Explore your website",
  research_company: "Research a company",
  find_companies: "Find companies",
  find_people: "Find decision makers",
  write_emails: "Write emails",
  classify_reply: "Sort a reply",
  draft_reply: "Draft a reply",
  linkedin_sequence: "Write LinkedIn messages",
  explorer: "Segments Explorer",
  segment_from_prompt: "Draft a campaign",
};

// Narrower cards drop columns instead of scrolling sideways, which hid the cost on anything short of a wide screen.
// The campaign and start time move under the run's name, and on phones its status does too.
const WIDE = "hidden @3xl:table-cell";
const STATUS = "hidden @md:table-cell";

const STATUSES: Record<AgentRun["status"], { label: string; tone: BadgeTone }> = {
  queued: { label: "Queued", tone: "neutral" },
  running: { label: "Running", tone: "blue" },
  succeeded: { label: "Done", tone: "green" },
  failed: { label: "Failed", tone: "red" },
  cancelled: { label: "Cancelled", tone: "neutral" },
};

export default function RunsPage({ params }: PageProps<"/app/p/[projectId]/runs">) {
  const { projectId } = use(params);
  // Refresh while something is going, so a running row's duration and cost catch up.
  const { data: runs, error, mutate } = useSWR<AgentRun[]>(`/projects/${projectId}/runs`, fetcher, {
    refreshInterval: (latest) => (latest?.some((r) => r.status === "queued" || r.status === "running") ? 5000 : 0),
  });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const segmentName = (run: AgentRun) => segments?.find((s) => s.id === run.input.segment_id)?.name;
  // Runs already queued again, so a second click doesn't queue a duplicate.
  const [retried, setRetried] = useState<string[]>([]);
  // After a reload that list is empty, so a run also counts as queued again once a newer one has the same input.
  const rerun = (run: AgentRun) =>
    retried.includes(run.id) ||
    !!runs?.some((r) => r.kind === run.kind && r.created_at > run.created_at && JSON.stringify(r.input) === JSON.stringify(run.input));

  const retry = async (run: AgentRun) => {
    setRetried((ids) => [...ids, run.id]);
    try {
      await api(`/runs/${run.id}/retry`, { method: "POST" });
      await mutate();
    } catch (e) {
      setRetried((ids) => ids.filter((id) => id !== run.id));
      toastError(e);
    }
  };

  return (
    <>
      <PageHeader
        title="Agent runs"
        description="What the agent did for this project and what each run cost. Costs are what Claude Code reports, so on a Claude subscription they're the equivalent API price. Shows the latest 50."
      />
      <PageBody>
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !runs ? (
          <ListSkeleton />
        ) : runs.length === 0 ? (
          <Card>
            <EmptyState icon={<Bot className="size-6" />} title="No runs yet">
              Research, finding companies and people, and writing emails all show up here.
            </EmptyState>
          </Card>
        ) : (
          <Card className="@container overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <Th>Run</Th>
                  <Th className={WIDE}>Campaign</Th>
                  <Th className={STATUS}>Status</Th>
                  <Th className={WIDE}>Started</Th>
                  <Th className={cn(WIDE, "text-right")}>Took</Th>
                  <Th className="text-right">Cost</Th>
                </tr>
              </thead>
              <tbody>
                {runs.map((r) => (
                  <Tr key={r.id}>
                    <Td>
                      <div className="font-medium @md:whitespace-nowrap">{TITLES[r.kind]}</div>
                      <Badge tone={STATUSES[r.status].tone} className="mt-1 @md:hidden">
                        {STATUSES[r.status].label}
                      </Badge>
                      {(segmentName(r) || r.started_at) && (
                        <p className="mt-0.5 text-xs text-muted-foreground @3xl:hidden">
                          {[segmentName(r), r.started_at && timeAgo(r.started_at)].filter(Boolean).join(" · ")}
                        </p>
                      )}
                      {r.status === "failed" && r.error && (
                        <p className="mt-0.5 line-clamp-2 max-w-80 text-xs text-red-700" title={r.error}>
                          {r.error}
                        </p>
                      )}
                      {(r.status === "failed" || r.status === "cancelled") && !rerun(r) && (
                        <button onClick={() => retry(r)} className="mt-1 text-xs font-medium text-foreground/75 underline-offset-2 hover:text-foreground hover:underline">
                          Run again
                        </button>
                      )}
                    </Td>
                    <Td className={cn(WIDE, "max-w-64 truncate text-muted-foreground")}>{segmentName(r) ?? "—"}</Td>
                    <Td className={STATUS}>
                      <Badge tone={STATUSES[r.status].tone}>{STATUSES[r.status].label}</Badge>
                    </Td>
                    <Td className={cn(WIDE, "whitespace-nowrap text-muted-foreground")} title={r.started_at ? dateTime(r.started_at) : undefined}>
                      {r.started_at ? timeAgo(r.started_at) : "—"}
                    </Td>
                    <Td className={cn(WIDE, "text-right whitespace-nowrap tabular-nums")}>{duration(r)}</Td>
                    <Td className="text-right tabular-nums">{cost(r.cost_usd)}</Td>
                  </Tr>
                ))}
              </tbody>
            </table>
          </Card>
        )}
      </PageBody>
    </>
  );
}

function duration(run: AgentRun): string {
  if (!run.started_at) return "—";
  const end = run.finished_at ? new Date(run.finished_at).getTime() : Date.now();
  const s = Math.max(0, Math.round((end - new Date(run.started_at).getTime()) / 1000));
  return s < 60 ? `${s}s` : `${Math.floor(s / 60)}m ${s % 60}s`;
}

// Reply sorting costs a fraction of a cent, which would round to a misleading $0.00.
function cost(usd: number): string {
  if (usd <= 0) return "—";
  return usd < 0.01 ? "<$0.01" : money(usd * 1_000_000);
}
