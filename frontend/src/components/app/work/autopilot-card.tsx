"use client";

import Link from "next/link";
import { ArrowRight, Loader2 } from "lucide-react";
import useSWR from "swr";
import { LinkButton } from "@/components/ui/button";
import { Badge, Card, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { AutopilotStatus } from "@/lib/types";
import { cn, money, num, timeAgo } from "@/lib/utils";
import { MicroLabel } from "./states";

const DOING: Partial<Record<string, string>> = {
  find_companies: "Finding companies for",
  find_people: "Finding people for",
  write_emails: "Writing emails for",
};

const DONE: Partial<Record<string, string>> = {
  find_companies: "Found companies for",
  find_people: "Found people for",
  write_emails: "Wrote emails for",
};

/** Autopilot at a glance: on or off, this month's spend and new leads against their limits, and what it's doing. */
export function AutopilotCard({ projectId }: { projectId: string }) {
  // Polled, since the worker moves on by itself every 10 minutes and its runs finish while the page is open.
  const { data } = useSWR<AutopilotStatus>(`/projects/${projectId}/autopilot`, fetcher, { refreshInterval: 30_000 });
  const settings = `/app/p/${projectId}/autopilot`;

  if (!data) return <Skeleton className="h-40 rounded-2xl" />;

  if (!data.enabled) {
    return (
      <Card className="p-5">
        <div className="mb-2 flex items-center justify-between gap-2">
          <MicroLabel>Autopilot</MicroLabel>
          <Badge>Off</Badge>
        </div>
        <p className="text-sm text-muted-foreground">
          Keeps your sending campaigns stocked with new leads and emails, within a monthly budget you set.
        </p>
        <LinkButton href={settings} variant="outline" size="sm" className="mt-3 w-full">
          Set up autopilot
        </LinkButton>
      </Card>
    );
  }

  const run = data.run;
  const going = run && (run.status === "queued" || run.status === "running");
  const campaign = run?.campaign ?? "a deleted campaign";
  const now = going ? `${DOING[run.kind] ?? "Working on"} ${campaign}` : data.note || "Waiting for its next check.";

  return (
    <Card className="p-5">
      <div className="mb-3 flex items-center justify-between gap-2">
        <MicroLabel>Autopilot</MicroLabel>
        <div className="flex items-center gap-3">
          <Link href={settings} className="text-xs font-medium hover:underline">
            Settings
          </Link>
          <Badge tone="green">On</Badge>
        </div>
      </div>
      <p className="flex items-start gap-2 text-sm">
        {going && <Loader2 className="mt-0.5 size-4 shrink-0 animate-spin text-muted-foreground" />}
        <span className={cn("min-w-0", going && "font-medium")}>{now}</span>
      </p>
      <div className="mt-4 flex flex-col gap-3">
        <Meter label="Spend this month" used={data.spent_micros} limit={data.budget_micros} format={money} />
        <Meter label="New leads this month" used={data.contacted} limit={data.lead_cap} format={num} />
      </div>
      {!data.auto_send && data.awaiting_approval > 0 && (
        <Link href={`/app/p/${projectId}/emails`} className="mt-4 flex items-center gap-1 text-sm font-medium hover:underline">
          {data.awaiting_approval === 1 ? "1 email waits" : `${num(data.awaiting_approval)} emails wait`} for your approval
          <ArrowRight className="size-3.5 shrink-0" />
        </Link>
      )}
      <p className="mt-4 border-t border-border pt-3 text-xs text-muted-foreground">
        {!run ? "No runs yet" : going ? `Started ${timeAgo(run.created_at)}` : lastRun(run)}
      </p>
    </Card>
  );
}

function lastRun(run: NonNullable<AutopilotStatus["run"]>) {
  const campaign = run.campaign ?? "a deleted campaign";
  const when = timeAgo(run.finished_at ?? run.created_at);
  if (run.status === "succeeded") return `${DONE[run.kind] ?? "Ran for"} ${campaign} ${when}`;
  return `${DOING[run.kind] ?? "Working on"} ${campaign} ${run.status === "failed" ? "failed" : "was cancelled"} ${when}`;
}

function Meter({ label, used, limit, format }: { label: string; used: number; limit: number; format: (n: number) => string }) {
  const share = limit > 0 ? Math.min(used / limit, 1) : 1;
  return (
    <div>
      <div className="mb-1 flex items-baseline justify-between gap-2 text-sm">
        <span className="text-muted-foreground">{label}</span>
        <span className="tabular-nums">
          <span className="font-medium">{format(used)}</span> <span className="text-muted-foreground">of {format(limit)}</span>
        </span>
      </div>
      <div
        className="h-1.5 overflow-hidden rounded-full bg-black/[0.06]"
        role="meter"
        aria-label={label}
        aria-valuemin={0}
        aria-valuemax={limit}
        aria-valuenow={Math.min(used, limit)}
      >
        <div className={cn("h-full rounded-full", share >= 1 ? "bg-amber-500" : "bg-brand-500")} style={{ width: `${share * 100}%` }} />
      </div>
    </div>
  );
}
