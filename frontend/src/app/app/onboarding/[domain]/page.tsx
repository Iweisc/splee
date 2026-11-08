"use client";

import { use } from "react";
import { ArrowRight } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CompanyProfile } from "@/components/app/work/company-profile";
import { RunProgress } from "@/components/app/work/run-progress";
import { SetupChecklist } from "@/components/app/work/setup-checklist";
import { ListSkeleton, LoadError, MicroLabel } from "@/components/app/work/states";
import { SegmentIcon } from "@/components/app/work/status";
import { LinkButton } from "@/components/ui/button";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import type { ProjectDetail, ProjectSummary, SegmentWithStats } from "@/lib/types";
import { compact } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

// The slug is the domain, or `p-<project id>` for projects without a website (same convention as the public demo).
export default function OnboardingRunPage({ params }: PageProps<"/app/onboarding/[domain]">) {
  const { domain } = use(params);
  const slug = decodeURIComponent(domain);
  const byId = slug.startsWith("p-") ? slug.slice(2) : null;

  const { data: projects, error: listError } = useSWR<ProjectSummary[]>(byId ? null : "/projects", fetcher);
  const projectId =
    byId ??
    projects
      ?.filter((p) => p.domain === slug)
      .sort((a, b) => b.created_at.localeCompare(a.created_at))[0]?.id;

  const { data: detail, error, mutate } = useSWR<ProjectDetail>(projectId ? `/projects/${projectId}` : null, fetcher);

  if (listError || error) return <LoadError error={listError ?? error} retry={() => mutate()} className="min-h-[60vh]" />;
  if (projects && !projectId) {
    return (
      <EmptyState
        className="min-h-[60vh]"
        title="Project not found"
        action={
          <LinkButton href="/app/onboarding" variant="outline">
            Create a project
          </LinkButton>
        }
      >
        There is no project for {slug} in this organization.
      </EmptyState>
    );
  }
  if (!detail) {
    return (
      <PageBody className="grid gap-4 pt-10 lg:grid-cols-[minmax(0,1fr)_360px]">
        <Skeleton className="h-96 rounded-2xl" />
        <Skeleton className="h-64 rounded-2xl" />
      </PageBody>
    );
  }

  const { project, settings, latest_run: run } = detail;
  // A run that stopped after step 1 leaves the project `ready` so research can be re-run, but onboarding isn't done.
  const failed = project.status === "failed" || run?.status === "failed" || run?.status === "cancelled";
  const done = !failed && project.status !== "researching";

  return (
    <>
      <PageHeader
        title={
          failed
            ? "Research didn't finish"
            : done
              ? `${project.name || project.domain} is ready`
              : `Setting up ${project.domain ?? "your project"}`
        }
        description={
          failed
            ? "Re-run the research from the company profile."
            : done
              ? "Review the campaigns, connect a mailbox and start sending."
              : "Splee is researching your company and market. You can leave this page, the agent keeps working."
        }
        actions={
          <LinkButton href={`/app/p/${project.id}`} variant={done ? "primary" : "outline"}>
            Project overview <ArrowRight className="size-4" />
          </LinkButton>
        }
      />
      <PageBody className="grid grid-cols-[minmax(0,1fr)] items-start gap-4 lg:grid-cols-[minmax(0,1fr)_360px]">
        <div className="flex flex-col gap-4">
          {run && <RunProgress key={run.id} run={run} />}
          <Campaigns projectId={project.id} />
        </div>
        <div className="flex flex-col gap-4">
          {done && <SetupChecklist projectId={project.id} settings={settings} />}
          <CompanyProfile project={project} />
        </div>
      </PageBody>
    </>
  );
}

function Campaigns({ projectId }: { projectId: string }) {
  const { data: segments, error, mutate } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  if (error) return <LoadError error={error} retry={() => mutate()} />;
  if (!segments) return <ListSkeleton rows={3} />;
  if (segments.length === 0) return null;
  return (
    <Card className="p-5">
      <MicroLabel className="mb-3">Campaigns {segments.length}</MicroLabel>
      <ul className="flex flex-col gap-2">
        {segments.map((s) => (
          <li key={s.id} className="flex items-center gap-3 rounded-xl border border-border px-3 py-2">
            <SegmentIcon name={s.icon} />
            <div className="min-w-0 flex-1">
              <p className="truncate text-sm font-medium">{s.name}</p>
              <p className="truncate text-xs text-muted-foreground">{s.description}</p>
            </div>
            <span className="text-sm tabular-nums">{compact(s.est_size)}</span>
          </li>
        ))}
      </ul>
    </Card>
  );
}
