"use client";

import { Breakdown, type Column, Heatmap, RateBar, rateCell, rateText } from "@/components/app/work/analytics";
import { DailyChart, formatDay } from "@/components/app/work/daily-chart";
import { type Change, type Kpi, KpiCard } from "@/components/app/work/kpi-card";
import { ListSkeleton, MicroLabel } from "@/components/app/work/states";
import { Badge, Card, Skeleton } from "@/components/ui/misc";
import { bouncing } from "@/lib/mailbox";
import type { Analytics, AnalyticsTotals } from "@/lib/types";
import { cn, money, num, percent } from "@/lib/utils";

/** "Oct 14 – Nov 12, 2026", with both years when the range crosses one. */
export function rangeText(from: string, to: string) {
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

/**
 * What a client sees on a shared report: results, and spend only when the link shows it. Unsubscribes and the cost split
 * stay on the team's page, which also keeps the tiles to full rows: six without spend, eight with it.
 */
const CLIENT_KPIS = ["Sent", "Replies", "Hot leads", "Meetings", "Deals won", "Bounce rate"];
const CLIENT_SPEND_KPIS = [...CLIENT_KPIS, "Spend", "Cost/meeting"];
const SPEND_COLUMNS = ["Spend", "Cost/hot"];

/**
 * The Analytics page's numbers, chart and breakdowns. `shared` is a client report's view of them, where spend and mailboxes
 * only show when the link says so. The backend leaves them out of the data too.
 */
export function AnalyticsView({ data, validating, shared }: { data?: Analytics; validating?: boolean; shared?: { spend: boolean; mailboxes: boolean } }) {
  const slug = data ? `${data.from}-to-${data.to}` : "";
  const all = kpis(data?.totals, data?.previous);
  const picked = shared ? (shared.spend ? CLIENT_SPEND_KPIS : CLIENT_KPIS) : null;
  const tiles = picked ? all.filter((k) => picked.includes(k.label)) : all;
  const grid = !shared ? "grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-6" : shared.spend ? "grid-cols-2 sm:grid-cols-4" : "grid-cols-2 sm:grid-cols-3 xl:grid-cols-6";
  const campaignColumns = shared && !shared.spend ? CAMPAIGN_COLUMNS.filter((c) => !SPEND_COLUMNS.includes(c.label)) : CAMPAIGN_COLUMNS;

  return (
    <div className={cn("flex flex-col gap-4 transition-opacity", validating && data && "opacity-60")}>
      <KpiCard kpis={tiles} className={grid} />
      <Card className="p-5 print:break-inside-avoid">
        <MicroLabel className="mb-3">{data?.interval === "week" ? "Per week" : "Per day"}</MicroLabel>
        {data ? <DailyChart daily={data.series} weekly={data.interval === "week"} /> : <Skeleton className="h-52" />}
      </Card>
      {data ? (
        <>
          <Breakdown title="Campaigns" rows={data.campaigns} columns={campaignColumns} file={`splee-campaigns-${slug}.csv`} rowKey={(r) => r.segment_id ?? "none"} />
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
          {(!shared || shared.mailboxes) && (
            <Breakdown title="Mailboxes" rows={data.mailboxes} columns={MAILBOX_COLUMNS} file={`splee-mailboxes-${slug}.csv`} rowKey={(r) => r.mailbox_id ?? "none"} />
          )}
          <Heatmap cells={data.heatmap} timezone={data.timezone} />
        </>
      ) : (
        <Card>
          <ListSkeleton rows={4} />
        </Card>
      )}
    </div>
  );
}
