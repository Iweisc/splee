"use client";

import Link from "next/link";
import { use, useState } from "react";
import { ChevronDown, Layers, Plus } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ArchiveToggle, SegmentToggle } from "@/components/app/work/segment-toggle";
import { LoadError, MicroLabel } from "@/components/app/work/states";
import { FitScore, SegmentIcon, SegmentStatusDot } from "@/components/app/work/status";
import { Button, LinkButton } from "@/components/ui/button";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import type { SegmentWithStats } from "@/lib/types";
import { cn, compact, money, num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

export default function SegmentsPage({ params }: PageProps<"/app/p/[projectId]/segments">) {
  const { projectId } = use(params);
  const { data, error, mutate } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments?archived=true`, fetcher);
  const [showArchived, setShowArchived] = useState(false);
  const sorted = data && [...data].sort((a, b) => a.position - b.position);
  const segments = sorted?.filter((s) => !s.archived_at);
  const archived = sorted?.filter((s) => s.archived_at) ?? [];

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
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !segments ? (
          <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} className="h-52 rounded-2xl" />
            ))}
          </div>
        ) : segments.length === 0 && archived.length === 0 ? (
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
          <>
            {segments.length === 0 ? (
              <Card>
                <EmptyState
                  icon={<Layers className="size-5" />}
                  title="Every campaign is archived"
                  action={
                    <LinkButton href={`/app/p/${projectId}/new`} size="sm">
                      Create a campaign
                    </LinkButton>
                  }
                >
                  Start a new one, or unarchive one below to send again.
                </EmptyState>
              </Card>
            ) : (
              <div className="grid gap-4 md:grid-cols-2 xl:grid-cols-3">
                {segments.map((s) => (
                  <SegmentCard key={s.id} projectId={projectId} segment={s} />
                ))}
              </div>
            )}
            {archived.length > 0 && (
              <div className="mt-6">
                <Button variant="ghost" size="sm" onClick={() => setShowArchived(!showArchived)} aria-expanded={showArchived} className="-ml-3">
                  <ChevronDown className={cn("size-3.5 transition-transform", showArchived && "rotate-180")} />
                  Archived ({archived.length})
                </Button>
                {showArchived && (
                  <div className="mt-3 grid gap-4 md:grid-cols-2 xl:grid-cols-3">
                    {archived.map((s) => (
                      <SegmentCard key={s.id} projectId={projectId} segment={s} />
                    ))}
                  </div>
                )}
              </div>
            )}
          </>
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
              {/* Size and fit stay 0 until research estimates them, as on a campaign made by hand. A real 0 wouldn't tell anyone much. */}
              {s.est_size > 0 && <span className="text-xs whitespace-nowrap text-muted-foreground tabular-nums">~{compact(s.est_size)} companies</span>}
            </div>
          </div>
        </div>
        {s.description && <p className="mt-3 line-clamp-2 text-sm text-muted-foreground">{s.description}</p>}
        {/* Start sits here, not beside the name: there it took a third of the row, so names wrapped on laptops, some cards
            in a row grew a line, and their fit scores no longer lined up. */}
        <div className="mt-3 mb-4 flex items-center gap-2">
          {s.fit_score > 0 && (
            <>
              <MicroLabel>Fit</MicroLabel>
              <FitScore value={s.fit_score} />
            </>
          )}
          <div className="ml-auto">{s.archived_at ? <ArchiveToggle segment={s} /> : <SegmentToggle segment={s} />}</div>
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
