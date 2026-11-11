"use client";

import { use, useState } from "react";
import { ChartLine } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { Breakdown, type Column, Heatmap, RateBar, rateCell, rateText } from "@/components/app/work/analytics";
import { DailyChart, formatDay } from "@/components/app/work/daily-chart";
import { type Change, type Kpi, KpiCard } from "@/components/app/work/kpi-card";
import { ListSkeleton, LoadError, MicroLabel } from "@/components/app/work/states";
import { Input, Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { bouncing } from "@/lib/mailbox";
import type { Analytics, AnalyticsTotals, Mailbox, ProjectDetail, SegmentWithStats } from "@/lib/types";
import { cn, money, num, percent } from "@/lib/utils";

const PRESETS = [
  { days: 7, label: "7 days" },
  { days: 30, label: "30 days" },
  { days: 90, label: "90 days" },
  { days: 365, label: "1 year" },
];
const MAX_DAYS = 365;

type Range = { days: number } | { from: string; to: string };

/** Today in the project's time zone, as YYYY-MM-DD, which is how the backend reads ranges. */
function today(timezone: string) {
  return new Intl.DateTimeFormat("en-CA", { timeZone: timezone, year: "numeric", month: "2-digit", day: "2-digit" }).format(new Date());
}

function addDays(date: string, n: number) {
  const d = new Date(`${date}T00:00:00Z`);
  d.setUTCDate(d.getUTCDate() + n);
  return d.toISOString().slice(0, 10);
}

function span(from: string, to: string) {
  return Math.round((Date.parse(`${to}T00:00:00Z`) - Date.parse(`${from}T00:00:00Z`)) / 86_400_000) + 1;
}

function rangeText(from: string, to: string) {
  const year = (d: string) => d.slice(0, 4);
  const withYear = (d: string) => `${formatDay(d)}, ${year(d)}`;
  return year(from) === year(to) ? `${formatDay(from)} – ${withYear(to)}` : `${withYear(from)} – ${withYear(to)}`;
}

/** Percent change of a count. `upIsGood` is null for numbers that are neither, like spend. */
function countChange(cur: number, prev: number, upIsGood: boolean | null): Change | undefined {
  if (prev === 0) return cur === 0 ? undefined : { text: "from 0", up: true, good: upIsGood };
  const ratio = (cur - prev) / prev;
  if (Math.abs(ratio) < 0.005) return { text: "0%", up: null, good: null };
  return { text: percent(Math.abs(ratio)), up: ratio > 0, good: upIsGood === null ? null : ratio > 0 === upIsGood };
}

/** A rate moves in percentage points, since "up 50%" on a 2% reply rate reads like far more than it is. */
function rateChange(cur: number | null, prev: number | null, upIsGood: boolean): Change | undefined {
  if (cur === null || prev === null) return undefined;
  const pts = (cur - prev) * 100;
  if (Math.abs(pts) < 0.05) return { text: "0 pts", up: null, good: null };
  const size = Math.abs(pts);
  return { text: `${size < 10 ? size.toFixed(1) : Math.round(size)} pts`, up: pts > 0, good: pts > 0 === upIsGood };
}

/** Whole dollars past $100, so a tile's value and its change fit on one line on a phone. */
function cost(micros: number) {
  return money(micros, { cents: micros < 100_000_000 });
}

function dealValue(v: AnalyticsTotals["won_value"][number]) {
  return (v.value_cents / 100).toLocaleString("en-US", { style: "currency", currency: v.currency, maximumFractionDigits: 0 });
}

function kpis(t?: AnalyticsTotals, p?: AnalyticsTotals): Kpi[] {
  const change = (f: (t: AnalyticsTotals, p: AnalyticsTotals) => Change | undefined) => (t && p ? f(t, p) : undefined);
  const perUnit = (v: number | null | undefined) => (v === undefined ? undefined : v === null ? "—" : cost(v));
  return [
    { label: "Sent", value: t && num(t.sent), note: t && `${num(t.delivered)} delivered`, change: change((t, p) => countChange(t.sent, p.sent, true)) },
    {
      label: "Replies",
      title: "Leads whose first reply came in these dates, out of the leads emailed. Bounces don't count.",
      value: t && num(t.replies),
      note: t?.reply_rate != null ? `${percent(t.reply_rate)} reply rate` : undefined,
      change: change((t, p) => countChange(t.replies, p.replies, true)),
    },
    {
      label: "Hot leads",
      title: "Leads who replied in these dates and are now Hot or Meeting booked.",
      value: t && num(t.hot),
      note: t?.hot_rate != null ? `${percent(t.hot_rate)} of leads` : undefined,
      change: change((t, p) => countChange(t.hot, p.hot, true)),
    },
    { label: "Meetings", title: "Leads booked in these dates.", value: t && num(t.meetings), change: change((t, p) => countChange(t.meetings, p.meetings, true)) },
    {
      label: "Deals won",
      value: t && num(t.deals_won),
      title: t && t.won_value.length > 1 ? t.won_value.map(dealValue).join(" + ") : undefined,
      note: t?.won_value[0] && `${dealValue(t.won_value[0])}${t.won_value.length > 1 ? " and more" : ""} won`,
      change: change((t, p) => countChange(t.deals_won, p.deals_won, true)),
    },
    {
      label: "Bounce rate",
      title: "Leads whose email bounced, out of the leads emailed. Past 5%, providers start sending your mail to spam.",
      value: t && rateText(t.bounce_rate),
      note: t && `${num(t.bounced)} bounced`,
      warn: t && bouncing(t.contacted, t.bounced),
      change: change((t, p) => rateChange(t.bounce_rate, p.bounce_rate, false)),
    },
    { label: "Unsubscribes", value: t && num(t.unsubscribes), change: change((t, p) => countChange(t.unsubscribes, p.unsubscribes, false)) },
    { label: "Spend", title: "Claude cost plus email cost.", value: t && cost(t.spend_micros), change: change((t, p) => countChange(t.spend_micros, p.spend_micros, null)) },
    {
      label: "Claude cost",
      // On a Claude subscription nothing is billed per run, so make clear what the figure means.
      title: "What Claude Code reports for the agent runs started in these dates. On a Claude subscription that's the equivalent API price, not a bill. A mailbox filter leaves it out, since runs don't belong to a mailbox.",
      value: t && cost(t.claude_cost_micros),
      change: change((t, p) => countChange(t.claude_cost_micros, p.claude_cost_micros, null)),
    },
    {
      label: "Email cost",
      title: "Emails sent times the cost per email you set on each mailbox.",
      value: t && cost(t.email_cost_micros),
      change: change((t, p) => countChange(t.email_cost_micros, p.email_cost_micros, null)),
    },
    {
      label: "Cost/hot lead",
      title: "Spend divided by hot leads.",
      value: perUnit(t?.cost_per_hot_micros),
      change: change((t, p) => (t.cost_per_hot_micros !== null && p.cost_per_hot_micros !== null ? countChange(t.cost_per_hot_micros, p.cost_per_hot_micros, false) : undefined)),
    },
    {
      label: "Cost/meeting",
      title: "Spend divided by meetings.",
      value: perUnit(t?.cost_per_meeting_micros),
      change: change((t, p) =>
        t.cost_per_meeting_micros !== null && p.cost_per_meeting_micros !== null ? countChange(t.cost_per_meeting_micros, p.cost_per_meeting_micros, false) : undefined,
      ),
    },
  ];
}

type CampaignRow = Analytics["campaigns"][number];
type MailboxRow = Analytics["mailboxes"][number];
type StepRow = Analytics["steps"][number];
type ChannelRow = Analytics["channels"][number];

const WIDE = "hidden sm:table-cell";
const WIDER = "hidden lg:table-cell";

const CAMPAIGN_COLUMNS: Column<CampaignRow>[] = [
  { label: "Campaign", value: (r) => r.name },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDE },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true },
  { label: "Hot", value: (r) => r.hot, numeric: true, className: WIDE },
  { label: "Meetings", value: (r) => r.meetings, numeric: true, className: WIDER },
  { label: "Won", value: (r) => r.deals_won, numeric: true, className: WIDER },
  { label: "Bounce rate", value: (r) => rateCell(r.bounce_rate), render: (r) => rateText(r.bounce_rate), numeric: true, className: WIDER },
  { label: "Spend", value: (r) => r.spend_micros / 1e6, render: (r) => money(r.spend_micros), numeric: true, className: WIDE },
  {
    label: "Cost/hot",
    value: (r) => (r.cost_per_hot_micros === null ? null : r.cost_per_hot_micros / 1e6),
    render: (r) => (r.cost_per_hot_micros === null ? "—" : money(r.cost_per_hot_micros)),
    numeric: true,
    className: WIDE,
  },
];

const MAILBOX_COLUMNS: Column<MailboxRow>[] = [
  { label: "Mailbox", value: (r) => r.email, render: (r) => <span className="block max-w-40 truncate sm:max-w-none">{r.email}</span> },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDE },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true, className: WIDE },
  { label: "Hot", value: (r) => r.hot, numeric: true, className: WIDER },
  { label: "Bounced", value: (r) => r.bounced, numeric: true, className: WIDER },
  {
    label: "Bounce rate",
    value: (r) => rateCell(r.bounce_rate),
    render: (r) => (
      // The rate rounds, so 5.04% shows as 5.0%. The title says why it's flagged. Phones only have room for the color.
      <span className={cn("inline-flex items-center gap-2", r.bouncing && "font-medium text-amber-700")} title={r.bouncing ? "Over 5% bounced" : undefined}>
        {r.bouncing && (
          <Badge tone="amber" className="hidden sm:inline-flex">
            High
          </Badge>
        )}
        {rateText(r.bounce_rate)}
      </span>
    ),
    numeric: true,
  },
];

const stepName = (step: number) => (step === 0 ? "First email" : `Follow-up ${step}`);

function stepColumns(steps: StepRow[]): Column<StepRow>[] {
  const best = Math.max(0, ...steps.map((s) => s.reply_rate ?? 0));
  return [
    { label: "Email", value: (r) => r.step, render: (r) => stepName(r.step) },
    { label: "Sent", value: (r) => r.sent, numeric: true },
    { label: "Replies", value: (r) => r.replies, numeric: true },
    { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => <RateBar rate={r.reply_rate} best={best} />, numeric: true },
  ];
}

const CHANNEL_NAMES: Record<ChannelRow["channel"], string> = { email: "Email", linkedin: "LinkedIn", sms: "SMS", whatsapp: "WhatsApp", social: "Social posts" };

const CHANNEL_COLUMNS: Column<ChannelRow>[] = [
  { label: "Channel", value: (r) => CHANNEL_NAMES[r.channel] },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  { label: "Leads", value: (r) => r.leads, numeric: true, className: WIDE },
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDE },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true },
];

export default function AnalyticsPage({ params }: PageProps<"/app/p/[projectId]/analytics">) {
  const { projectId } = use(params);
  const { data: detail } = useSWR<ProjectDetail>(`/projects/${projectId}`, fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: mailboxList } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const [range, setRange] = useState<Range>({ days: 30 });
  const [segmentId, setSegmentId] = useState("");
  const [mailboxId, setMailboxId] = useState("");

  const timezone = detail?.settings.timezone;
  const end = timezone && today(timezone);
  const [from, to] = "days" in range ? (end ? [addDays(end, 1 - range.days), end] : []) : [range.from, range.to];
  const invalid =
    from && to && (from > to ? "Pick an end date on or after the start." : span(from, to) > MAX_DAYS ? `Pick at most ${MAX_DAYS} days.` : null);
  const query = new URLSearchParams({ ...(from && { from }), ...(to && { to }), ...(segmentId && { segment_id: segmentId }), ...(mailboxId && { mailbox_id: mailboxId }) });
  const key = from && to && !invalid ? `/projects/${projectId}/analytics?${query}` : null;
  const { data, error, mutate, isValidating } = useSWR<Analytics>(key, fetcher, { keepPreviousData: true });

  const filtered = Boolean(segmentId || mailboxId);
  const t = data?.totals;
  const empty = data && t && t.sent === 0 && t.replies === 0 && t.meetings === 0 && t.deals_won === 0 && data.channels.every((c) => c.sent === 0);
  const slug = data ? `${data.from}-to-${data.to}` : "";

  return (
    <>
      <PageHeader title="Analytics" description="Which campaigns, mailboxes, follow-ups and send times get replies, next to the period before." />
      <PageBody className="flex flex-col gap-4">
        <div className="flex flex-wrap items-end gap-3">
          <div className="grid w-full grid-cols-5 gap-0.5 rounded-lg bg-black/[0.05] p-0.5 sm:flex sm:w-auto" role="group" aria-label="Date range">
            {PRESETS.map((p) => (
              <RangeButton key={p.days} active={"days" in range && range.days === p.days} onClick={() => setRange({ days: p.days })}>
                {p.label}
              </RangeButton>
            ))}
            <RangeButton active={"from" in range} onClick={() => from && to && setRange({ from, to })}>
              Custom
            </RangeButton>
          </div>
          {"from" in range && (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <Input type="date" aria-label="From" value={range.from} max={range.to} onChange={(e) => e.target.value && setRange({ ...range, from: e.target.value })} className="h-9 w-auto" />
              <span className="text-muted-foreground">to</span>
              <Input type="date" aria-label="To" value={range.to} min={range.from} onChange={(e) => e.target.value && setRange({ ...range, to: e.target.value })} className="h-9 w-auto" />
            </div>
          )}
          <div className="flex grow flex-wrap gap-2 sm:grow-0">
            <Select aria-label="Campaign" value={segmentId} onChange={(e) => setSegmentId(e.target.value)} className="h-9 min-w-0 flex-1 sm:w-auto sm:flex-none">
              <option value="">All campaigns</option>
              {segments?.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </Select>
            <Select aria-label="Mailbox" value={mailboxId} onChange={(e) => setMailboxId(e.target.value)} className="h-9 min-w-0 flex-1 sm:w-auto sm:flex-none">
              <option value="">All mailboxes</option>
              {mailboxList?.map((m) => (
                <option key={m.id} value={m.id}>
                  {m.email}
                </option>
              ))}
            </Select>
          </div>
        </div>
        {invalid ? (
          <p className="text-sm font-medium text-red-700">{invalid}</p>
        ) : (
          data && (
            <p className="-mt-1 text-xs text-muted-foreground">
              {rangeText(data.from, data.to)}, compared with {rangeText(data.previous_from, data.previous_to)}. Days are in {data.timezone}.
            </p>
          )
        )}

        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : empty ? (
          <Card>
            <EmptyState icon={<ChartLine className="size-5" />} title={filtered ? "Nothing matches these filters" : "Nothing sent in these dates"}>
              {filtered
                ? "Try all campaigns and mailboxes, or a longer range."
                : "Once emails go out, this page shows which campaigns, mailboxes, follow-ups and send times get replies."}
            </EmptyState>
          </Card>
        ) : (
          <div className={cn("flex flex-col gap-4 transition-opacity", isValidating && data && "opacity-60")}>
            <KpiCard kpis={kpis(t, data?.previous)} className="grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-6" />
            <Card className="p-5">
              <MicroLabel className="mb-3">{data?.interval === "week" ? "Per week" : "Per day"}</MicroLabel>
              {data ? <DailyChart daily={data.series} weekly={data.interval === "week"} /> : <Skeleton className="h-52" />}
            </Card>
            {data ? (
              <>
                <Breakdown
                  title="Campaigns"
                  rows={data.campaigns}
                  columns={CAMPAIGN_COLUMNS}
                  file={`splee-campaigns-${slug}.csv`}
                  rowKey={(r) => r.segment_id ?? "none"}
                />
                <div className="grid gap-4 lg:grid-cols-2">
                  <Breakdown
                    title="Follow-ups"
                    hint="A reply counts for the last email the lead got before it, so you can see whether each follow-up earns its place."
                    rows={data.steps}
                    columns={stepColumns(data.steps)}
                    sortBy={0}
                    file={`splee-steps-${slug}.csv`}
                    rowKey={(r) => String(r.step)}
                  />
                  <Breakdown
                    title="Channels"
                    hint="LinkedIn replies only stop the sequence, so Splee doesn't count them."
                    rows={data.channels}
                    columns={CHANNEL_COLUMNS}
                    file={`splee-channels-${slug}.csv`}
                    rowKey={(r) => r.channel}
                    rowClass={(r) => (r.sent === 0 ? "text-muted-foreground" : undefined)}
                  />
                </div>
                <Breakdown
                  title="Mailboxes"
                  rows={data.mailboxes}
                  columns={MAILBOX_COLUMNS}
                  file={`splee-mailboxes-${slug}.csv`}
                  rowKey={(r) => r.mailbox_id ?? "none"}
                />
                <Heatmap cells={data.heatmap} timezone={data.timezone} />
              </>
            ) : (
              <Card>
                <ListSkeleton rows={4} />
              </Card>
            )}
          </div>
        )}
      </PageBody>
    </>
  );
}

function RangeButton({ active, onClick, children }: { active: boolean; onClick: () => void; children: React.ReactNode }) {
  return (
    <button
      onClick={onClick}
      aria-pressed={active}
      className={cn("h-8 rounded-md px-2 text-sm font-medium whitespace-nowrap transition-colors sm:px-3", active ? "bg-card shadow-sm" : "text-muted-foreground hover:text-foreground")}
    >
      {children}
    </button>
  );
}
