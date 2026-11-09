"use client";

import { useRouter } from "next/navigation";
import { use, useState } from "react";
import { Clock, Layers, MailWarning, Pause, Play, Plus, TriangleAlert } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CampaignsTable } from "@/components/app/work/campaigns-table";
import { CompanyProfile } from "@/components/app/work/company-profile";
import { DailyChart } from "@/components/app/work/daily-chart";
import { type Kpi, KpiCard, replyRate } from "@/components/app/work/kpi-card";
import { RunProgress, useProjectRuns } from "@/components/app/work/run-progress";
import { SetupChecklist } from "@/components/app/work/setup-checklist";
import { ListSkeleton, LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { ProjectStatusBadge } from "@/components/app/work/status";
import { Button, LinkButton } from "@/components/ui/button";
import { Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import { api, ApiError } from "@/lib/api";
import { bouncing, wholePercent } from "@/lib/mailbox";
import type { Mailbox, MailboxStats, ProjectDetail, ProjectSettings, ProjectStats } from "@/lib/types";
import { money, num, timeAgo } from "@/lib/utils";
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
          {/* Stands in for the domain under the title, so the page doesn't drop 24px when the project loads. */}
          <Skeleton className="mt-1 h-5 w-40" />
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
        {sending && <MailboxNotice />}
        <BounceNotice />
        {sending && <SendWindowNotice projectId={projectId} settings={settings} />}
        <Stats projectId={projectId} />
        <div className="grid grid-cols-[minmax(0,1fr)] items-start gap-4 lg:grid-cols-[minmax(0,1fr)_340px]">
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

/** A mailbox that stops connecting stops sending or reading replies, and otherwise only the Mailboxes page says so. */
function MailboxNotice() {
  // Polled like the reply poller (every minute), so a mailbox that breaks while this page is open shows up.
  const { data } = useSWR<Mailbox[]>("/mailboxes", fetcher, { refreshInterval: 60_000 });
  // The sender marks a mailbox "error" when SMTP fails. The reply poller only records last_error, and clears it on its next
  // good poll. Without IMAP, an active mailbox's last_error is a stale SMTP error from before it was resumed.
  const broken = data?.filter((m) => m.status === "error" || (m.status === "active" && m.imap_host && m.last_error)) ?? [];
  if (!data || broken.length === 0) return null;
  const sendingStopped = !data.some((m) => m.status === "active");
  // The sender tests a mailbox with recheck_at again itself. The rest wait for someone to fix them.
  const needsFixing = broken.some((m) => m.status !== "error" || !m.recheck_at);
  return (
    <Card className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <MailWarning className="size-4 text-red-600" />
      {/* The basis pushes the button onto its own row on phones, so the addresses aren't squeezed into a narrow column. */}
      <div className="min-w-0 flex-1 basis-64 space-y-1">
        {broken.map((m) => (
          <p key={m.id}>
            <span className="font-medium">{m.email}</span>{" "}
            {m.status !== "error" ? "can't read replies" : m.recheck_at ? "can't send, and Splee tests it again on its own" : "can't send"}:{" "}
            <span className="text-muted-foreground">{m.last_error}</span>
          </p>
        ))}
        {sendingStopped && <p className="font-medium text-red-700">Nothing is going out until a mailbox works again.</p>}
      </div>
      <LinkButton href="/app/domains" variant="outline" size="sm" className="max-sm:grow">
        {needsFixing ? "Fix mailbox" : "View mailboxes"}
      </LinkButton>
    </Card>
  );
}

/** The Mailboxes page's bounce warning, here because this is the page people check every day. */
function BounceNotice() {
  const { data } = useSWR<MailboxStats[]>("/mailboxes/stats", fetcher);
  const bad = data?.filter((s) => bouncing(s.emailed, s.bounced)) ?? [];
  if (bad.length === 0) return null;
  return (
    <Card className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <TriangleAlert className="size-4 text-amber-600" />
      <div className="min-w-0 flex-1 basis-64 space-y-1">
        {bad.map((s) => (
          <p key={s.mailbox_id}>
            <span className="font-medium">{s.email}</span> bounced {wholePercent(s.bounced, s.emailed)} of the {num(s.emailed)} leads it
            emailed in the last 30 days.
          </p>
        ))}
        <p className="text-muted-foreground">
          Past 5%, providers start sending its mail to spam. Each campaign shows its bounce rate.
        </p>
      </div>
      <LinkButton href="/app/domains" variant="outline" size="sm" className="max-sm:grow">
        View mailboxes
      </LinkButton>
    </Card>
  );
}

/** Explains why nothing is going out right after "Start sending" outside the configured hours. */
function SendWindowNotice({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  // Read the clock once per mount. "in N min" goes stale if the page sits open, which is fine for a hint.
  const [now] = useState(() => new Date());
  const next = nextSendWindow(settings, now);
  if (!next) return null;
  const yours = next.toLocaleString("en-US", { weekday: "short", hour: "numeric", minute: "2-digit" });
  const theirs = next.toLocaleString("en-US", { timeZone: settings.timezone, hour: "2-digit", minute: "2-digit", hourCycle: "h23" });
  return (
    <Card className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      <Clock className="size-4 text-muted-foreground" />
      <p className="min-w-0 flex-1 basis-64">
        Outside sending hours. The next emails go out {yours} your time ({theirs} {settings.timezone}), {timeAgo(next.toISOString())}.
      </p>
      <LinkButton href={`/app/p/${projectId}/schedule`} variant="outline" size="sm" className="max-sm:grow">
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
  if (error)
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} />
      </Card>
    );

  const t = data?.totals;
  const kpis: Kpi[] = [
    { label: "Leads", value: t && num(t.leads) },
    { label: "Sent", value: t && num(t.sent) },
    {
      label: "Replies",
      value: t && num(t.replies),
      title: "Leads who wrote back, out of the leads you've emailed. Bounces don't count.",
      note: t && replyRate(t.replies, t.contacted),
    },
    { label: "Hot leads", value: t && num(t.hot) },
    { label: "Meetings", value: t && num(t.meetings) },
    {
      label: "Spend",
      value: t && money(t.spend_micros),
      // On a Claude subscription nothing is billed per run, so make clear what the AI figure means.
      title:
        t &&
        `AI ${money(t.ai_cost_micros)} · Email ${money(t.email_cost_micros)}\n\nAI is what Claude Code reports for the agent's runs. On a Claude subscription that's the equivalent API price, not a bill. Email is the cost per email you set on each mailbox.`,
    },
    { label: "Cost/hot lead", title: "Spend divided by hot leads.", value: t && (t.cost_per_hot_micros === null ? "—" : money(t.cost_per_hot_micros)) },
  ];

  return (
    <>
      <KpiCard kpis={kpis} className="grid-cols-2 sm:grid-cols-4 xl:grid-cols-7 xl:divide-x xl:divide-border" />
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
