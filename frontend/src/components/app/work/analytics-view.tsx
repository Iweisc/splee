"use client";

import { Breakdown, type Column, Heatmap, RateBar, rateCell, rateText } from "@/components/app/work/analytics";
import { DailyChart, formatDay } from "@/components/app/work/daily-chart";
import { type Change, type Kpi, KpiCard } from "@/components/app/work/kpi-card";
import { ListSkeleton, MicroLabel } from "@/components/app/work/states";
import { Badge, Card, Skeleton } from "@/components/ui/misc";
import { bouncing } from "@/lib/mailbox";
import type { Analytics, AnalyticsTotals, Engagement } from "@/lib/types";
import { cn, money, num, percent, plural } from "@/lib/utils";

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

/** "12.4x", and whole numbers from 10 up. */
function times(ratio: number | null) {
  return ratio === null ? "—" : `${ratio < 10 ? ratio.toFixed(1) : num(Math.round(ratio))}x`;
}

/** The tiles for what leads paid, which only show once a payment provider is connected. */
function revenueKpis(currency: string, t?: AnalyticsTotals, p?: AnalyticsTotals): Kpi[] {
  const change = (f: (t: AnalyticsTotals, p: AnalyticsTotals) => Change | undefined) => (t && p ? f(t, p) : undefined);
  const paid = (cents: number) => dealValue({ currency, value_cents: cents });
  return [
    {
      label: "Revenue",
      title: "What leads, and people at their companies, paid through Stripe, Paddle or Lemon Squeezy in these dates, less refunds.",
      value: t && paid(t.revenue_cents),
      note: t && (t.refunded_cents > 0 ? `${paid(t.refunded_cents)} refunded` : `in ${currency}`),
      change: change((t, p) => countChange(t.revenue_cents, p.revenue_cents, true)),
    },
    {
      label: "Payments",
      value: t && num(t.payments),
      note: t && `from ${plural(t.paying_leads, "lead")}`,
      change: change((t, p) => countChange(t.payments, p.payments, true)),
    },
    {
      label: "Revenue/meeting",
      title: "Revenue divided by meetings.",
      value: t && (t.revenue_per_meeting_cents === null ? "—" : paid(t.revenue_per_meeting_cents)),
      change: change((t, p) =>
        t.revenue_per_meeting_cents !== null && p.revenue_per_meeting_cents !== null ? countChange(t.revenue_per_meeting_cents, p.revenue_per_meeting_cents, true) : undefined,
      ),
    },
    {
      label: "Return on spend",
      title: "Revenue divided by spend.",
      value: t && times(t.return_on_spend),
      change: change((t, p) => (t.return_on_spend !== null && p.return_on_spend !== null ? countChange(t.return_on_spend, p.return_on_spend, true) : undefined)),
    },
  ];
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
type AngleRow = NonNullable<Analytics["angles"]>[number];

const WIDE = "hidden sm:table-cell";
const WIDER = "hidden lg:table-cell";
const WIDEST = "hidden 2xl:table-cell";

/** Click and open rates, which a campaign that doesn't track them leaves as a dash. Opens get a ~, since they're estimates. */
function engagementColumns<T extends Engagement>(clickClass?: string, openClass?: string): Column<T>[] {
  return [
    { label: "Click rate", value: (r) => rateCell(r.click_rate), render: (r) => rateText(r.click_rate), numeric: true, className: clickClass },
    {
      label: "Open rate",
      value: (r) => rateCell(r.open_rate),
      render: (r) => (r.open_rate === null ? "—" : <span title="An estimate">~{percent(r.open_rate)}</span>),
      numeric: true,
      className: openClass,
    },
  ];
}

const OPENS_HINT = "Open rates are estimates, since Apple Mail opens every email itself and some people block images. A dash means opens or clicks aren't tracked.";
const tracksOpens = (rows: Engagement[]) => rows.some((r) => r.open_rate !== null);

const CAMPAIGN_COLUMNS: Column<CampaignRow>[] = [
  { label: "Campaign", value: (r) => r.name },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  // Replies and Won wait for the widest screens so the click and open rates fit at 1280. The tiles above have both totals.
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDEST },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true },
  ...engagementColumns<CampaignRow>(WIDE, WIDER),
  { label: "Hot", value: (r) => r.hot, numeric: true, className: WIDE },
  { label: "Meetings", value: (r) => r.meetings, numeric: true, className: WIDER },
  { label: "Won", value: (r) => r.deals_won, numeric: true, className: WIDEST },
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
  ...engagementColumns<MailboxRow>(WIDER, WIDER),
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
    ...engagementColumns<StepRow>(WIDE, WIDE),
  ];
}

const ANGLE_COLUMNS: Column<AngleRow>[] = [
  {
    label: "Angle",
    value: (r) => `${r.campaign}: ${r.name}`,
    render: (r) => (
      <span className="block max-w-40 truncate sm:max-w-none">
        {r.name} <span className="font-normal text-muted-foreground">· {r.campaign}</span>
      </span>
    ),
  },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDE },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true },
  ...engagementColumns<AngleRow>(WIDE, WIDE),
];

/** The campaign's revenue, in the organization's currency, and its return on spend. */
function revenueColumns(currency: string): Column<CampaignRow>[] {
  return [
    {
      label: "Revenue",
      value: (r) => r.revenue_cents / 100,
      render: (r) => dealValue({ currency, value_cents: r.revenue_cents }),
      numeric: true,
      className: WIDE,
    },
    // The table only has room for a twelfth column on the widest screens. The Return on spend tile has the total.
    { label: "Return", value: (r) => r.return_on_spend, render: (r) => times(r.return_on_spend), numeric: true, className: WIDEST },
  ];
}

const CHANNEL_NAMES: Record<ChannelRow["channel"], string> = { email: "Email", linkedin: "LinkedIn", sms: "SMS", whatsapp: "WhatsApp", call: "Calls", social: "Social posts" };

const CHANNEL_COLUMNS: Column<ChannelRow>[] = [
  { label: "Channel", value: (r) => CHANNEL_NAMES[r.channel] },
  { label: "Sent", value: (r) => r.sent, numeric: true },
  { label: "Leads", value: (r) => r.leads, numeric: true, className: WIDE },
  { label: "Replies", value: (r) => r.replies, numeric: true, className: WIDE },
  { label: "Reply rate", value: (r) => rateCell(r.reply_rate), render: (r) => rateText(r.reply_rate), numeric: true },
  { label: "Connect rate", value: (r) => rateCell(r.connect_rate), render: (r) => rateText(r.connect_rate), numeric: true, className: WIDE },
  { label: "Meetings", value: (r) => r.meetings, numeric: true, className: WIDE },
];

/**
 * What a client sees on a shared report: results, and spend only when the link shows it. Unsubscribes and the cost split
 * stay on the team's page, which also keeps the tiles to full rows: six without spend, eight with it.
 */
const CLIENT_KPIS = ["Sent", "Replies", "Hot leads", "Meetings", "Deals won", "Bounce rate"];
const CLIENT_SPEND_KPIS = [...CLIENT_KPIS, "Spend", "Cost/meeting"];
const SPEND_COLUMNS = ["Spend", "Cost/hot", "Return"];

/**
 * The Analytics page's numbers, chart and breakdowns. `shared` is a client report's view of them, where spend and mailboxes
 * only show when the link says so. The backend leaves them out of the data too.
 */
export function AnalyticsView({ data, validating, shared }: { data?: Analytics; validating?: boolean; shared?: { spend: boolean; mailboxes: boolean } }) {
  const slug = data ? `${data.from}-to-${data.to}` : "";
  const revenue = !!data?.revenue_tracked;
  const all = kpis(data?.totals, data?.previous);
  if (revenue && !shared) {
    // Revenue goes next to deals won, and what it's worth per meeting and per dollar next to the costs, which keeps four full rows.
    const extra = revenueKpis(data.currency, data.totals, data.previous);
    all.splice(all.findIndex((k) => k.label === "Deals won") + 1, 0, ...extra.slice(0, 2));
    all.splice(all.findIndex((k) => k.label === "Cost/meeting") + 1, 0, ...extra.slice(2));
  }
  const picked = shared ? (shared.spend ? CLIENT_SPEND_KPIS : CLIENT_KPIS) : null;
  const tiles = picked ? all.filter((k) => picked.includes(k.label)) : all;
  const grid = shared
    ? shared.spend
      ? "grid-cols-2 sm:grid-cols-4"
      : "grid-cols-2 sm:grid-cols-3 xl:grid-cols-6"
    : revenue
      ? "grid-cols-2 sm:grid-cols-4 2xl:grid-cols-8"
      : "grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 xl:grid-cols-6";
  const withRevenue = revenue && data ? [...CAMPAIGN_COLUMNS, ...revenueColumns(data.currency)] : CAMPAIGN_COLUMNS;
  const campaignColumns = shared && !shared.spend ? withRevenue.filter((c) => !SPEND_COLUMNS.includes(c.label)) : withRevenue;

  return (
    <div className={cn("flex flex-col gap-4 transition-opacity", validating && data && "opacity-60")}>
      <KpiCard kpis={tiles} className={grid} />
      <Card className="p-5 print:break-inside-avoid">
        <MicroLabel className="mb-3">{data?.interval === "week" ? "Per week" : "Per day"}</MicroLabel>
        {data ? <DailyChart daily={data.series} weekly={data.interval === "week"} /> : <Skeleton className="h-52" />}
      </Card>
      {data ? (
        <>
          <Breakdown
            title="Campaigns"
            hint={tracksOpens(data.campaigns) ? OPENS_HINT : undefined}
            hintClass="hidden lg:block"
            rows={data.campaigns}
            columns={campaignColumns}
            file={`splee-campaigns-${slug}.csv`}
            rowKey={(r) => r.segment_id ?? "none"}
          />
          {!shared && !!data.angles?.length && (
            <Breakdown
              title="A/B test angles"
              hint={
                <>
                  Each angle&apos;s leads and emails, follow-ups included. Reply rate is by lead
                  <span className="hidden sm:inline">, click and open rates by email</span>.
                </>
              }
              rows={data.angles}
              columns={ANGLE_COLUMNS}
              file={`splee-angles-${slug}.csv`}
              rowKey={(r) => r.variant_id}
            />
          )}
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
              hint="LinkedIn replies only stop the sequence, so Splee doesn't count them. For calls, sent is calls made, and connect rate is the share where someone picked up and talked."
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
