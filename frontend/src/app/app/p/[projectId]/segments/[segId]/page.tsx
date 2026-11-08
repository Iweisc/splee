"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { use, useState } from "react";
import { ArrowLeft, Building2, Mail, Play, Trash2, Users } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CompaniesTable } from "@/components/app/work/companies-table";
import { EmailsView } from "@/components/app/work/emails-view";
import { LeadsView } from "@/components/app/work/leads-view";
import { isActive, RunProgress, useProjectRuns } from "@/components/app/work/run-progress";
import { SegmentFields, type SegmentDraft } from "@/components/app/work/segment-form";
import { SegmentToggle } from "@/components/app/work/segment-toggle";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { FitScore, SegmentIcon, SegmentStatusDot } from "@/components/app/work/status";
import { Button } from "@/components/ui/button";
import { NumberInput } from "@/components/ui/form";
import { Card, Skeleton, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { EmailRow, Paginated, SegmentWithStats } from "@/lib/types";
import { cn, compact, num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

type Tab = "details" | "companies" | "leads" | "emails";

export default function SegmentPage({ params }: PageProps<"/app/p/[projectId]/segments/[segId]">) {
  const { projectId, segId } = use(params);
  const router = useRouter();
  const { data: segment, error, mutate } = useSWR<SegmentWithStats>(`/segments/${segId}`, fetcher);
  const { runs } = useProjectRuns(projectId);
  const [tab, setTab] = useState<Tab>("details");

  const run = runs?.find((r) => r.input.segment_id === segId && isActive(r)) ?? null;

  if (error) return <LoadError error={error} retry={() => mutate()} className="min-h-[60vh]" />;
  if (!segment) {
    return (
      <PageBody className="flex flex-col gap-4 pt-8">
        <Skeleton className="h-8 w-72" />
        <Skeleton className="h-32 rounded-2xl" />
        <Skeleton className="h-96 rounded-2xl" />
      </PageBody>
    );
  }

  const remove = async () => {
    if (!confirm(`Delete the campaign "${segment.name}"?`)) return;
    try {
      await api(`/segments/${segId}`, { method: "DELETE" });
      toast.success("Campaign deleted");
      router.push(`/app/p/${projectId}/segments`);
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <>
      <PageHeader
        title={
          <span className="flex items-center gap-3">
            <Link href={`/app/p/${projectId}/segments`} className="text-muted-foreground hover:text-foreground" aria-label="All campaigns">
              <ArrowLeft className="size-5" />
            </Link>
            <SegmentIcon name={segment.icon} className="size-8" />
            {segment.name || "Untitled campaign"}
          </span>
        }
        description={
          <span className="flex flex-wrap items-center gap-4">
            <SegmentStatusDot status={segment.status} />
            <span className="tabular-nums">~{compact(segment.est_size)} companies</span>
            <span className="inline-flex items-center gap-2">
              Fit <FitScore value={segment.fit_score} />
            </span>
          </span>
        }
        actions={
          <>
            <Button variant="ghost" size="icon" onClick={remove} aria-label="Delete campaign">
              <Trash2 className="size-4" />
            </Button>
            <SegmentToggle segment={segment} size="md" />
          </>
        }
      />
      <PageBody className="flex flex-col gap-4">
        {run && <RunProgress key={run.id} run={run} />}
        <Pipeline segment={segment} busy={!!run} />
        <Tabs
          value={tab}
          onChange={setTab}
          options={[
            { value: "details", label: "Details" },
            { value: "companies", label: `Companies ${num(segment.companies)}` },
            { value: "leads", label: `Leads ${num(segment.leads)}` },
            { value: "emails", label: "Emails" },
          ]}
          className="self-start"
        />
        {/* Keyed on the content so the form picks up fields the agent fills in after "Describe it". */}
        {tab === "details" && <SegmentEditor key={editorKey(segment)} segment={segment} />}
        {tab === "companies" && <CompaniesTable projectId={projectId} segmentId={segId} />}
        {tab === "leads" && <LeadsView projectId={projectId} segmentId={segId} />}
        {tab === "emails" && <EmailsView projectId={projectId} segmentId={segId} />}
      </PageBody>
    </>
  );
}

function Pipeline({ segment, busy }: { segment: SegmentWithStats; busy: boolean }) {
  const { mutate } = useSWRConfig();
  const [count, setCount] = useState(25);
  const [starting, setStarting] = useState<string | null>(null);
  // A working campaign approves new emails right away, so counting only drafts would show 0.
  const { data: written } = useSWR<Paginated<EmailRow>>(
    `/emails${qs({ segment_id: segment.id, status: "draft,approved,scheduled,sending", per_page: 1 })}`,
    fetcher,
  );

  const start = async (action: string, body: Record<string, unknown>) => {
    setStarting(action);
    try {
      await api(`/segments/${segment.id}/${action}`, { body });
      await mutate(`/projects/${segment.project_id}/runs`);
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(null);
    }
  };

  const running = segment.status === "working" || segment.status === "scaling";
  const steps = [
    {
      title: "Find companies",
      icon: <Building2 className="size-4" />,
      stat: `${num(segment.companies)} found`,
      done: segment.companies > 0,
      action: (
        <div className="flex gap-1.5">
          <NumberInput
            min={1}
            max={200}
            value={count}
            onValueChange={(n) => setCount(Math.min(200, Math.max(1, Math.round(n))))}
            className="h-8 w-16 px-2 tabular-nums"
            aria-label="Number of companies"
          />
          <Button size="sm" variant="outline" disabled={busy} loading={starting === "find-companies"} onClick={() => start("find-companies", { count })}>
            Find
          </Button>
        </div>
      ),
    },
    {
      title: "Find people",
      icon: <Users className="size-4" />,
      stat: `${num(segment.leads)} leads`,
      done: segment.leads > 0,
      action: (
        <Button size="sm" variant="outline" disabled={busy || segment.companies === 0} loading={starting === "find-people"} onClick={() => start("find-people", {})}>
          Find people
        </Button>
      ),
    },
    {
      title: "Write emails",
      icon: <Mail className="size-4" />,
      stat: written ? `${num(written.total)} written` : "…",
      done: (written?.total ?? 0) > 0 || segment.sent > 0,
      action: (
        <Button size="sm" variant="outline" disabled={busy || segment.leads === 0} loading={starting === "write-emails"} onClick={() => start("write-emails", {})}>
          Write emails
        </Button>
      ),
    },
    {
      title: "Start",
      icon: <Play className="size-4" />,
      stat: running ? `${num(segment.sent)} sent` : "Approves drafts",
      done: running,
      action: <SegmentToggle segment={segment} />,
    },
  ];

  return (
    <Card className="grid divide-y divide-border md:grid-cols-4 md:divide-x md:divide-y-0">
      {steps.map((s, i) => (
        <div key={s.title} className="@container flex flex-col gap-3 p-4">
          <div className="flex items-center gap-2">
            <span
              className={cn(
                "inline-flex size-6 items-center justify-center rounded-full text-xs",
                s.done ? "bg-brand-500 text-white" : "border border-border-strong text-muted-foreground",
              )}
            >
              {i + 1}
            </span>
            <span className="text-sm font-medium">{s.title}</span>
            {/* The icon is decoration, so it gives way before a title would wrap in the four-across row on tablets. */}
            <span className="ml-auto hidden text-muted-foreground @[10rem]:block">{s.icon}</span>
          </div>
          <MicroLabel>{s.stat}</MicroLabel>
          <div>{s.action}</div>
        </div>
      ))}
    </Card>
  );
}

function toDraft(s: SegmentWithStats): SegmentDraft {
  return { name: s.name, description: s.description, icon: s.icon, angle: s.angle, pain_points: s.pain_points, targeting: s.targeting };
}

function editorKey(s: SegmentWithStats) {
  return `${s.id}:${JSON.stringify(toDraft(s))}`;
}

function SegmentEditor({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const initial = toDraft(segment);
  const [draft, setDraft] = useState(initial);
  const [saving, setSaving] = useState(false);
  const dirty = JSON.stringify(draft) !== JSON.stringify(initial);

  const save = async () => {
    setSaving(true);
    try {
      await mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: draft }), { revalidate: false });
      mutate((k) => typeof k === "string" && k.includes("/segments"));
      toast.success("Campaign saved");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card className="p-6">
      <SegmentFields value={draft} onChange={setDraft} />
      <div className="mt-5 flex justify-end gap-2">
        {dirty && (
          <Button variant="ghost" onClick={() => setDraft(initial)}>
            Discard
          </Button>
        )}
        <Button onClick={save} loading={saving} disabled={!dirty || !draft.name.trim()}>
          Save changes
        </Button>
      </div>
    </Card>
  );
}
