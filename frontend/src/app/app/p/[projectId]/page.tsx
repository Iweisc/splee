"use client";

import { useRouter } from "next/navigation";
import { use, useState } from "react";
import { Clock, Layers, Pause, Play, Plus } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CampaignsTable } from "@/components/app/work/campaigns-table";
import { CompanyProfile } from "@/components/app/work/company-profile";
import { DailyChart } from "@/components/app/work/daily-chart";
import { RunProgress, useProjectRuns } from "@/components/app/work/run-progress";
import { SetupChecklist } from "@/components/app/work/setup-checklist";
import { ListSkeleton, LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { ProjectStatusBadge } from "@/components/app/work/status";
import { Button, LinkButton } from "@/components/ui/button";
import { Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import { api, ApiError } from "@/lib/api";
import type { ProjectDetail, ProjectSettings, ProjectStats } from "@/lib/types";
import { money, num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

export default function OverviewPage({ params }: PageProps<"/app/p/[projectId]">) {
  const { projectId } = use(params);
  const { data: detail, error, mutate } = useSWR<ProjectDetail>(`/projects/${projectId}`, fetcher);
  const { active } = useProjectRuns(projectId);

  if (error) return <LoadError error={error} retry={() => mutate()} className="min-h-[60vh]" />;
  if (!detail) {
    return (
      <>
        <div className="border-b border-border px-4 pt-6 pb-5 sm:px-8 sm:pt-8">
          <Skeleton className="h-8 w-64" />
        </div>
        <PageBody className="grid gap-4 md:grid-cols-4">
          {Array.from({ length: 4 }, (_, i) => (
            <Skeleton key={i} className="h-24 rounded-2xl" />
          ))}
        </PageBody>
      </>
    );
  }

  const { project, settings } = detail;
  const sending = !settings.paused;

  return (
    <>
      <PageHeader
        title={
          <span className="flex items-center gap-3">
            {project.domain && <Favicon domain={project.domain} size={24} />}
            {project.name || project.domain || "Untitled project"}
            <ProjectStatusBadge status={project.status} />
          </span>
        }
        description={project.domain ?? project.business_description ?? undefined}
        actions={<SendingToggle projectId={projectId} sending={sending} />}
      />
      <PageBody className="flex flex-col gap-4">
        {active && <RunProgress key={active.id} run={active} />}
        {sending && <SendWindowNotice projectId={projectId} settings={settings} />}
        <Stats projectId={projectId} />
        <div className="grid items-start gap-4 lg:grid-cols-[minmax(0,1fr)_340px]">
          <Campaigns projectId={projectId} />
          <div className="flex flex-col gap-4">
            {!sending && <SetupChecklist projectId={projectId} settings={settings} />}
            <CompanyProfile project={project} />
          </div>
        </div>
      </PageBody>
    </>
  );
}

function SendingToggle({ projectId, sending }: { projectId: string; sending: boolean }) {
  const router = useRouter();
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);

  const toggle = async () => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/${sending ? "pause" : "start"}`, { method: "POST" });
      await mutate(() => true);
      toast.success(sending ? "Sending paused" : "Sending started");
    } catch (e) {
      // The backend answers 400 when no mailbox is active.
      if (e instanceof ApiError && e.status === 400) {
        toast.error(e.message, { action: { label: "Connect mailbox", onClick: () => router.push("/app/domains") } });
      } else {
        toastError(e);
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <Button onClick={toggle} loading={busy} variant={sending ? "outline" : "primary"}>
      {!busy && (sending ? <Pause className="size-4" /> : <Play className="size-4" />)}
      {sending ? "Pause sending" : "Start sending"}
    </Button>
  );
}

/** Explains why nothing is going out right after "Start sending" outside the configured hours. */
function SendWindowNotice({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  // Read the clock once per mount. "in N min" goes stale if the page sits open, which is fine for a hint.
  const [now] = useState(() => new Date());
  const next = nextSendWindow(settings, now);
  if (!next) return null;
  const mins = Math.round((next.getTime() - now.getTime()) / 60_000);
  const wait = mins < 60 ? `${mins} min` : `${Math.floor(mins / 60)} h ${mins % 60} min`;
  const yours = next.toLocaleString("en-US", { weekday: "short", hour: "numeric", minute: "2-digit" });
  const theirs = next.toLocaleString("en-US", { timeZone: settings.timezone, hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  return (
    <Card className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <Clock className="size-4 text-muted-foreground" />
      <p className="flex-1">
        Outside sending hours. The next emails go out {yours} your time ({theirs} {settings.timezone}), in {wait}.
      </p>
      <LinkButton href={`/app/p/${projectId}/schedule`} variant="outline" size="sm">
        Change hours
      </LinkButton>
    </Card>
  );
}

const WEEKDAYS = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

/** Start of the next sending window, or null while inside one. Mirrors in_window in backend/src/mail/sender.rs. */
function nextSendWindow(s: ProjectSettings, now: Date): Date | null {
  const fmt = new Intl.DateTimeFormat("en-US", { timeZone: s.timezone, weekday: "short", hour: "numeric", hourCycle: "h23" });
  const inWindow = (t: Date) => {
    const parts = Object.fromEntries(fmt.formatToParts(t).map((p) => [p.type, p.value]));
    const day = WEEKDAYS.indexOf(parts.weekday ?? "") + 1;
    const hour = Number(parts.hour);
    return s.send_days.includes(day) && hour >= s.send_start_hour && hour < s.send_end_hour;
  };
  if (inWindow(now)) return null;
  // Quarter-hour steps, because some timezones are offset by 30 or 45 minutes.
  const t = new Date(now);
  t.setUTCMinutes(Math.floor(t.getUTCMinutes() / 15) * 15, 0, 0);
  for (let i = 0; i < 8 * 24 * 4; i++) {
    t.setUTCMinutes(t.getUTCMinutes() + 15);
    if (inWindow(t)) return t;
  }
  return null;
}

function Stats({ projectId }: { projectId: string }) {
  const { data, error, mutate } = useSWR<ProjectStats>(`/projects/${projectId}/stats?days=30`, fetcher);
  if (error) return <LoadError error={error} retry={() => mutate()} />;

  const t = data?.totals;
  const kpis: { label: string; value?: string; detail?: string[]; title?: string }[] = [
    { label: "Leads", value: t && num(t.leads) },
    { label: "Sent", value: t && num(t.sent) },
    { label: "Replies", value: t && num(t.replies) },
    { label: "Hot leads", value: t && num(t.hot) },
    { label: "Meetings", value: t && num(t.meetings) },
    {
      label: "Spend",
      value: t && money(t.spend_micros),
      detail: t && [`AI ${money(t.ai_cost_micros)}`, `Email ${money(t.email_cost_micros)}`],
      // On a Claude subscription nothing is billed per run, so make clear what the AI figure means.
      title:
        "AI is what Claude Code reports for the agent's runs. On a Claude subscription that's the equivalent API price, not a bill. Email is the cost per email you set on each mailbox.",
    },
    { label: "Cost/hot lead", title: "Spend divided by hot leads.", value: t && (t.cost_per_hot_micros === null ? "—" : money(t.cost_per_hot_micros)) },
  ];

  return (
    <>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-4 xl:grid-cols-7">
        {kpis.map((k) => (
          <Card key={k.label} className="px-4 py-3.5" title={k.title}>
            <MicroLabel>{k.label}</MicroLabel>
            {k.value === undefined ? <Skeleton className="mt-2 h-6 w-14" /> : <p className="mt-1 text-xl font-semibold tabular-nums">{k.value}</p>}
            {k.detail && (
              <div className="mt-0.5 text-[11px] text-muted-foreground tabular-nums">
                {k.detail.map((line) => (
                  <p key={line}>{line}</p>
                ))}
              </div>
            )}
          </Card>
        ))}
      </div>
      <Card className="p-5">
        <MicroLabel className="mb-3">Last 30 days</MicroLabel>
        {data ? <DailyChart daily={data.daily} /> : <Skeleton className="h-48" />}
      </Card>
    </>
  );
}

function Campaigns({ projectId }: { projectId: string }) {
  const { data, error, mutate } = useSWR<ProjectStats>(`/projects/${projectId}/stats?days=30`, fetcher);
  return (
    <Card className="overflow-hidden">
      <div className="flex items-center justify-between px-5 pt-4 pb-2">
        <MicroLabel>Campaigns</MicroLabel>
        <LinkButton href={`/app/p/${projectId}/new`} variant="ghost" size="sm">
          <Plus className="size-3.5" /> New campaign
        </LinkButton>
      </div>
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !data ? (
        <ListSkeleton rows={4} />
      ) : data.segments.length === 0 ? (
        <EmptyState
          icon={<Layers className="size-5" />}
          title="No campaigns yet"
          action={
            <LinkButton href={`/app/p/${projectId}/new`} size="sm">
              Create a campaign
            </LinkButton>
          }
        >
          The research run defines campaigns for you, or describe one yourself.
        </EmptyState>
      ) : (
        <CampaignsTable projectId={projectId} segments={data.segments} />
      )}
    </Card>
  );
}
