"use client";

import { use, useState } from "react";
import { ChartLine, Share2 } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { AnalyticsView, rangeText } from "@/components/app/work/analytics-view";
import { ShareReport } from "@/components/app/work/share-report";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { Analytics, Mailbox, ProjectDetail, SegmentWithStats } from "@/lib/types";
import { cn } from "@/lib/utils";

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

export default function AnalyticsPage({ params }: PageProps<"/app/p/[projectId]/analytics">) {
  const { projectId } = use(params);
  const { data: detail } = useSWR<ProjectDetail>(`/projects/${projectId}`, fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: mailboxList } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const [range, setRange] = useState<Range>({ days: 30 });
  const [segmentId, setSegmentId] = useState("");
  const [mailboxId, setMailboxId] = useState("");
  const [sharing, setSharing] = useState(false);

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

  return (
    <>
      <PageHeader
        title="Analytics"
        description="Which campaigns, mailboxes, follow-ups and send times get replies, next to the period before."
        actions={
          <Button variant="outline" onClick={() => setSharing(true)}>
            <Share2 className="size-4" /> Share with a client
          </Button>
        }
      />
      {detail && <ShareReport open={sharing} onClose={() => setSharing(false)} project={detail} />}
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
          <AnalyticsView data={data} validating={isValidating} />
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
