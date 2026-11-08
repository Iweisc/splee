"use client";

import { use } from "react";
import { Bot } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ListSkeleton, LoadError, Td, Th, Tr } from "@/components/app/work/states";
import { Badge, Card, EmptyState, type BadgeTone } from "@/components/ui/misc";
import type { AgentRun, RunKind, SegmentWithStats } from "@/lib/types";
import { dateTime, money, timeAgo } from "@/lib/utils";
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

  return (
    <>
      <PageHeader
        title="Agent runs"
        description="What the agent did for this project and what each run cost. Costs are what Claude Code reports, so on a Claude subscription they're the equivalent API price. Shows the latest 50."
      />
      <PageBody>
        {error ? (
          <LoadError error={error} retry={() => mutate()} />
        ) : !runs ? (
          <ListSkeleton />
        ) : runs.length === 0 ? (
          <EmptyState icon={<Bot className="size-6" />} title="No runs yet">
            Research, finding companies and people, and writing emails all show up here.
          </EmptyState>
        ) : (
          <Card className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <Th>Run</Th>
                  <Th>Campaign</Th>
                  <Th>Status</Th>
                  <Th>Started</Th>
                  <Th className="text-right">Took</Th>
                  <Th className="text-right">Cost</Th>
                </tr>
              </thead>
              <tbody>
                {runs.map((r) => (
                  <Tr key={r.id}>
                    <Td>
                      <div className="font-medium whitespace-nowrap">{TITLES[r.kind]}</div>
                      {r.status === "failed" && r.error && (
                        <p className="mt-0.5 line-clamp-2 max-w-80 text-xs text-red-700" title={r.error}>
                          {r.error}
                        </p>
                      )}
                    </Td>
                    <Td className="max-w-64 truncate text-muted-foreground">{segmentName(r) ?? "—"}</Td>
                    <Td>
                      <Badge tone={STATUSES[r.status].tone}>{STATUSES[r.status].label}</Badge>
                    </Td>
                    <Td className="whitespace-nowrap text-muted-foreground" title={r.started_at ? dateTime(r.started_at) : undefined}>
                      {r.started_at ? timeAgo(r.started_at) : "—"}
                    </Td>
                    <Td className="text-right whitespace-nowrap tabular-nums">{duration(r)}</Td>
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
