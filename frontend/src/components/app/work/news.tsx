"use client";

import { useState } from "react";
import { ExternalLink, Newspaper } from "lucide-react";
import useSWR from "swr";
import { Select, Switch } from "@/components/ui/form";
import { Badge, type BadgeTone, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { CompanySignal, SegmentWithStats, SignalBrief, SignalKind, SignalsOverview } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { AddToCampaignBar } from "./hiring";
import { ListSkeleton, LoadError, Td, Th, toastError, Tr } from "./states";

export const KIND_LABEL: Record<SignalKind, string> = {
  funding_filing: "SEC filing",
  funding_post: "Funding",
  launch: "Launch",
  acquisition: "Acquisition",
  expansion: "Expansion",
  leadership: "New leader",
};

const KIND_TONE: Record<SignalKind, BadgeTone> = {
  funding_filing: "green",
  funding_post: "green",
  launch: "blue",
  acquisition: "amber",
  expansion: "blue",
  leadership: "neutral",
};

const KINDS = { "": "All news", funding_filing: "SEC filings", funding_post: "Funding posts", launch: "Launches", acquisition: "Acquisitions", expansion: "Expansion", leadership: "New leaders" } as const;
type KindFilter = keyof typeof KINDS;

const PERIODS = { "30": "Last 30 days", "90": "Last 90 days", "": "Everything kept" } as const;
type Period = keyof typeof PERIODS;

/** "$12M", "$1.5M", "$750K". */
export function dollars(n: number): string {
  for (const [size, unit] of [[1e9, "B"], [1e6, "M"], [1e3, "K"]] as const) {
    if (n >= size) return `$${Number((n / size).toFixed(1))}${unit}`;
  }
  return `$${n}`;
}

/** "Oct 3", with the year when it isn't this year. A date without a time, so it's read as written, not in the viewer's timezone. */
export function signalDate(on: string): string {
  const [y, m, d] = on.split("-").map(Number);
  const date = new Date(y, m - 1, d);
  return date.toLocaleDateString("en-US", { month: "short", day: "numeric", year: y === new Date().getFullYear() ? undefined : "numeric" });
}

/** The leading signal as a badge and its headline, for company rows. Two lines at most, like the Hiring badge. */
export function NewsBadge({ news, className }: { news: SignalBrief | null; className?: string }) {
  if (!news) return null;
  return (
    <p className={cn("line-clamp-2 text-xs leading-5 text-muted-foreground", className)} title={news.headline}>
      <Badge tone={KIND_TONE[news.kind]} className="mr-1.5 align-middle">
        <Newspaper className="size-3" /> News
      </Badge>
      {news.headline}
    </p>
  );
}

/** Funding filings and company news at the project's companies, and "Add to campaign" for companies with no leads yet. */
export function NewsSignals({ projectId }: { projectId: string }) {
  const [kind, setKind] = useState<KindFilter>("");
  const [period, setPeriod] = useState<Period>("30");
  // Company ids, since a company with two signals has two rows that tick together.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const key = `/projects/${projectId}/signals${qs({ kind: kind || null, days: period || null })}`;
  const { data, error, mutate } = useSWR<SignalsOverview>(key, fetcher, { keepPreviousData: true });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const signals = data?.signals;

  const addable = (s: CompanySignal) => !!s.company_id && s.leads === 0;
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const companies = new Set(signals?.filter(addable).map((s) => s.company_id!) ?? []);
  const allPicked = companies.size > 0 && [...companies].every((id) => picked.has(id));
  const companyCount = new Set(signals?.map((s) => s.domain)).size;

  const setAlerts = async (funding_alerts: boolean) => {
    if (!data) return;
    mutate({ ...data, funding_alerts }, { revalidate: false });
    try {
      await api(`/projects/${projectId}/signals/settings`, { method: "PATCH", body: { funding_alerts } });
    } catch (e) {
      toastError(e);
      mutate();
    }
  };

  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <AddToCampaignBar
          projectId={projectId}
          ids={[...picked]}
          segments={segments ?? []}
          onClear={() => setPicked(new Set())}
          onDone={() => {
            setPicked(new Set());
            mutate();
          }}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          <Select
            value={kind}
            onChange={(e) => {
              setKind(e.target.value as KindFilter);
              setPicked(new Set());
            }}
            className="h-9 w-auto flex-auto sm:w-40 sm:flex-none"
            aria-label="Kind of news"
          >
            {Object.entries(KINDS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <Select
            value={period}
            onChange={(e) => {
              setPeriod(e.target.value as Period);
              setPicked(new Set());
            }}
            className="h-9 w-auto flex-auto sm:w-40 sm:flex-none"
            aria-label="Dated"
          >
            {Object.entries(PERIODS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <p className="text-sm text-muted-foreground tabular-nums sm:mr-auto">
            {signals && signals.length > 0 && `${plural(signals.length, "signal")} at ${plural(companyCount, "company", "companies")}`}
          </p>
          {data && data.checked > 0 && data.last_checked_at && (
            <p className="w-full text-xs text-muted-foreground sm:w-auto">
              {plural(data.checked, "company", "companies")} checked, last {timeAgo(data.last_checked_at)}
            </p>
          )}
        </div>
      )}

      {data && (
        <div className="flex items-center gap-3 border-b border-border px-3 py-2.5 text-sm">
          {data.sec ? (
            <>
              <Switch checked={data.funding_alerts} onChange={setAlerts} label="Tell my chats about new SEC filings" />
              <span className="text-pretty">Tell your chats when a company in a running campaign files to raise money</span>
            </>
          ) : (
            <span className="text-pretty text-muted-foreground">
              SEC filings are off on this server until it sets SEC_USER_AGENT. Company blogs are still read.
            </span>
          )}
        </div>
      )}

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="company news" />
      ) : !signals ? (
        <ListSkeleton />
      ) : signals.length === 0 ? (
        <EmptyState icon={<Newspaper className="size-5" />} title={period ? `No news in the ${PERIODS[period].toLowerCase()}` : "No news yet"}>
          Each week Splee looks up your US companies&apos; Form D filings with the SEC and reads every company&apos;s own blog or newsroom for
          funding, launches, acquisitions, new offices and new leaders.
        </EmptyState>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr>
                <Th className="w-[1%] pr-0">
                  <input
                    type="checkbox"
                    checked={allPicked}
                    ref={(box) => {
                      if (box) box.indeterminate = picked.size > 0 && !allPicked;
                    }}
                    onChange={() => setPicked(allPicked ? new Set() : companies)}
                    disabled={companies.size === 0}
                    aria-label="Select every company without leads"
                    className="block accent-black"
                  />
                </Th>
                <Th>News</Th>
                <Th className="hidden sm:table-cell">Company</Th>
                <Th className="hidden w-[1%] md:table-cell">Leads</Th>
                <Th className="w-[1%] text-right whitespace-nowrap">Date</Th>
              </tr>
            </thead>
            <tbody>
              {signals.map((s) => (
                <Row key={s.id} signal={s} picked={!!s.company_id && picked.has(s.company_id)} onToggle={addable(s) ? () => toggle(s.company_id!) : undefined} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

/** What the row's first line says: the amount for a filing, the post's title otherwise. */
function what(s: CompanySignal): string {
  if (s.kind !== "funding_filing") return s.title;
  if (s.amount) return `Raised ${dollars(s.amount)}`;
  if (s.offered) return `Raising ${dollars(s.offered)}`;
  return "Filed to raise money";
}

function Row({ signal: s, picked, onToggle }: { signal: CompanySignal; picked: boolean; onToggle?: () => void }) {
  const filing = s.kind === "funding_filing";
  // A filing names the legal entity and the offering, a post only needs its kind.
  const about = filing ? [s.title, s.industry, s.offered && s.amount ? `${dollars(s.offered)} offered` : ""].filter(Boolean).join(" · ") : "";
  return (
    <Tr onClick={onToggle}>
      <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
        {onToggle && <input type="checkbox" checked={picked} onChange={onToggle} aria-label={`Select ${s.company_name}`} className="block accent-black" />}
      </Td>
      <Td className="w-[60%] max-w-0">
        {/* The badge sits inline so a long title wraps under it, not in a narrow column beside it. */}
        <p className="line-clamp-3 font-medium sm:line-clamp-2">
          <Badge tone={KIND_TONE[s.kind]} className="mr-1.5 align-middle">
            {KIND_LABEL[s.kind]}
          </Badge>
          <a href={s.url} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()} className="hover:underline">
            {what(s)}
          </a>
        </p>
        {/* The company column only shows from sm up. */}
        <span className="block truncate text-xs text-muted-foreground sm:hidden">{s.company_name}</span>
        {about && <span className="hidden truncate text-xs text-muted-foreground sm:block">{about}</span>}
      </Td>
      <Td className="hidden w-[40%] max-w-0 sm:table-cell">
        <span className="flex items-center gap-2">
          <Favicon domain={s.domain} />
          <span className="min-w-0">
            <span className="block truncate font-medium">{s.company_name}</span>
            <span className="block truncate text-xs text-muted-foreground">
              {s.domain}
              {s.checked_at && ` · checked ${timeAgo(s.checked_at)}`}
            </span>
          </span>
        </span>
      </Td>
      <Td className="hidden whitespace-nowrap text-muted-foreground tabular-nums md:table-cell">{s.leads > 0 ? plural(s.leads, "lead") : "None yet"}</Td>
      <Td className="text-right whitespace-nowrap text-muted-foreground" title={`Found ${timeAgo(s.first_seen)}`}>
        {signalDate(s.happened_on)}
      </Td>
    </Tr>
  );
}

/** The lead's company's news on the lead's page, newest first. */
export function LeadNews({ news }: { news: CompanySignal[] }) {
  if (!news.length) return null;
  return (
    <ul className="flex flex-col gap-1.5 text-sm">
      {news.map((s) => (
        <li key={s.id} className="flex min-w-0 items-center gap-2">
          <Badge tone={KIND_TONE[s.kind]} className="shrink-0">
            {KIND_LABEL[s.kind]}
          </Badge>
          <a href={s.url} target="_blank" rel="noreferrer" className="inline-flex min-w-0 items-center gap-1 hover:underline">
            <span className="truncate">{s.kind === "funding_filing" ? what(s) : s.title}</span> <ExternalLink className="size-3 shrink-0" />
          </a>
          <span className="ml-auto shrink-0 text-xs text-muted-foreground">{signalDate(s.happened_on)}</span>
        </li>
      ))}
    </ul>
  );
}
