"use client";

import Link from "next/link";
import { use } from "react";
import { Layers, Plus } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { SegmentToggle } from "@/components/app/work/segment-toggle";
import { LoadError, MicroLabel } from "@/components/app/work/states";
import { FitScore, SegmentIcon, SegmentStatusDot } from "@/components/app/work/status";
import { LinkButton } from "@/components/ui/button";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import type { SegmentWithStats } from "@/lib/types";
import { compact, money, num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

export default function SegmentsPage({ params }: PageProps<"/app/p/[projectId]/segments">) {
  const { projectId } = use(params);
  const { data: segments, error, mutate } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);

  return (
    <>
      <PageHeader
        title="Campaigns"
        description="Each campaign is one buyer segment with its own targeting, angle and emails."
        actions={
          <LinkButton href={`/app/p/${projectId}/new`}>
            <Plus className="size-4" /> New campaign
          </LinkButton>
        }
      />
      <PageBody>
        {error ? (
          <LoadError error={error} retry={() => mutate()} />
        ) : !segments ? (
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} className="h-52 rounded-2xl" />
            ))}
          </div>
        ) : segments.length === 0 ? (
          <Card>
            <EmptyState
              icon={<Layers className="size-5" />}
              title="No campaigns yet"
              action={
                <LinkButton href={`/app/p/${projectId}/new`} size="sm">
                  Create a campaign
                </LinkButton>
              }
            >
              Describe who you want to reach in plain words and the agent fills in the targeting.
            </EmptyState>
          </Card>
        ) : (
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            {[...segments]
              .sort((a, b) => a.position - b.position)
              .map((s) => (
                <SegmentCard key={s.id} projectId={projectId} segment={s} />
              ))}
          </div>
        )}
      </PageBody>
    </>
  );
}

function SegmentCard({ projectId, segment: s }: { projectId: string; segment: SegmentWithStats }) {
  const stats = [
    { label: "Companies", value: num(s.companies) },
    { label: "Leads", value: num(s.leads) },
    { label: "Sent", value: num(s.sent) },
    { label: "Replies", value: num(s.replies) },
    { label: "Hot", value: num(s.hot) },
    { label: "Cost/lead", value: s.cost_per_hot_micros === null ? "—" : money(s.cost_per_hot_micros) },
  ];
  return (
    <Link href={`/app/p/${projectId}/segments/${s.id}`} className="group min-w-0">
      <Card className="flex h-full flex-col p-5 transition-shadow group-hover:shadow-pop">
        <div className="flex items-start gap-3">
          <SegmentIcon name={s.icon} className="size-9" />
          <div className="min-w-0 flex-1">
            <p className="line-clamp-2 font-medium">{s.name}</p>
            <div className="mt-0.5 flex items-center gap-3">
              <SegmentStatusDot status={s.status} />
              <span className="text-xs whitespace-nowrap text-muted-foreground tabular-nums">~{compact(s.est_size)} companies</span>
            </div>
          </div>
        </div>
        {s.description && <p className="mt-3 line-clamp-2 text-sm text-muted-foreground">{s.description}</p>}
        {/* Start sits here, not beside the name: there it took a third of the row, so names wrapped on laptops, some cards
            in a row grew a line, and their fit scores no longer lined up. */}
        <div className="mt-3 mb-4 flex items-center gap-2">
          <MicroLabel>Fit</MicroLabel>
          <FitScore value={s.fit_score} />
          <div className="ml-auto">
            <SegmentToggle segment={s} />
          </div>
        </div>
        <dl className="mt-auto grid grid-cols-3 gap-x-3 gap-y-2 border-t border-border pt-3 text-sm">
          {stats.map((st) => (
            <div key={st.label}>
              <dt className="text-xs text-muted-foreground">{st.label}</dt>
              <dd className="font-medium tabular-nums">{st.value}</dd>
            </div>
          ))}
        </dl>
      </Card>
    </Link>
  );
}
