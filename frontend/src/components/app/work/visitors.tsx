"use client";

import { useState } from "react";
import Link from "next/link";
import { Code, Eye } from "lucide-react";
import useSWR from "swr";
import { LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { Paginated, SegmentWithStats, Visitor } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { AddToCampaignBar } from "./hiring";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, Tr, useLastPageGuard } from "./states";

const PERIODS = { "7": "Last 7 days", "30": "Last 30 days" } as const;
type Period = keyof typeof PERIODS;
const MIN_VIEWS = { "1": "Any pages read", "2": "2+ pages read", "3": "3+ pages read", "5": "5+ pages read", "10": "10+ pages read" } as const;
type MinViews = keyof typeof MIN_VIEWS;

const PAGES = "hidden sm:table-cell";
const SOURCE = "hidden lg:table-cell";
const STATUS = "hidden md:table-cell";

/** Where a company is: its campaign, its leads, or nowhere yet. */
function Status({ v }: { v: Visitor }) {
  if (v.campaign) return <Badge tone="green">In {v.campaign}</Badge>;
  if (v.leads > 0) return <Badge tone="blue">{plural(v.leads, "lead")}</Badge>;
  return <span className="text-muted-foreground">New</span>;
}

/** Companies that visited the project's site, the pages they read and how they first came, and "Add to campaign" for the
 * ones with no leads and no campaign yet. */
export function VisitorsView({ projectId }: { projectId: string }) {
  const [period, setPeriod] = useState<Period>("7");
  const [minViews, setMinViews] = useState<MinViews>("1");
  const [page, setPage] = useState(1);
  // Domains, since a visiting company may not be one of the project's companies yet.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const key = `/projects/${projectId}/visitors${qs({ days: period, min_views: minViews, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<Visitor>>(key, fetcher, { keepPreviousData: true });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const turnPage = (p: number) => {
    setPage(p);
    setPicked(new Set());
  };
  useLastPageGuard(data?.total, page, turnPage);

  const addable = data?.items.filter((v) => v.leads === 0 && !v.campaign).map((v) => v.domain) ?? [];
  const allPicked = addable.length > 0 && addable.every((d) => picked.has(d));
  const toggle = (domain: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(domain)) next.add(domain);
      return next;
    });
  const install = `/app/p/${projectId}/integrations#visitors`;
  const filtered = minViews !== "1";

  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <AddToCampaignBar
          projectId={projectId}
          ids={[...picked]}
          segments={segments ?? []}
          route={{ path: "visitors/add", field: "domains" }}
          onClear={() => setPicked(new Set())}
          onDone={() => {
            setPicked(new Set());
            mutate();
          }}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          <Select
            value={period}
            onChange={(e) => {
              setPeriod(e.target.value as Period);
              turnPage(1);
            }}
            className="h-9 w-auto flex-auto sm:w-40 sm:flex-none"
            aria-label="Visited in"
          >
            {Object.entries(PERIODS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <Select
            value={minViews}
            onChange={(e) => {
              setMinViews(e.target.value as MinViews);
              turnPage(1);
            }}
            className="h-9 w-auto flex-auto sm:w-44 sm:flex-none"
            aria-label="Pages read"
          >
            {Object.entries(MIN_VIEWS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <p className="mr-auto text-sm text-muted-foreground tabular-nums">{data && data.total > 0 && plural(data.total, "company", "companies")}</p>
          <LinkButton href={install} variant="outline" size="sm" className="h-9 flex-auto sm:flex-none">
            <Code className="size-3.5" /> Site snippet
          </LinkButton>
        </div>
      )}

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="visitors" />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Eye className="size-5" />} title={filtered ? "No companies read that many pages" : `No company visits in the ${PERIODS[period].toLowerCase()}`}>
          Add <Link href={install} className="underline">the snippet</Link> to your site and Splee shows the companies that browse it from their
          own networks. Home, mobile, cloud and VPN connections are left out.
        </EmptyState>
      ) : (
        <>
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
                      onChange={() => setPicked(allPicked ? new Set() : new Set(addable))}
                      disabled={addable.length === 0}
                      aria-label="Select every company not in a campaign"
                      className="block accent-black"
                    />
                  </Th>
                  <Th>Company</Th>
                  <Th className={PAGES}>Pages read</Th>
                  <Th className={cn(SOURCE, "w-[1%] whitespace-nowrap")}>First came from</Th>
                  <Th className={cn(STATUS, "w-[1%]")}>Status</Th>
                  <Th className="w-[1%] text-right whitespace-nowrap">Last visit</Th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((v) => {
                  const onToggle = v.leads === 0 && !v.campaign ? () => toggle(v.domain) : undefined;
                  const reads = `${plural(v.views, "page")} in ${plural(v.visits, "visit")}`;
                  return (
                    <Tr key={v.domain} onClick={onToggle}>
                      <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
                        {onToggle && (
                          <input type="checkbox" checked={picked.has(v.domain)} onChange={onToggle} aria-label={`Select ${v.name}`} className="block accent-black" />
                        )}
                      </Td>
                      <Td className="max-w-0 sm:w-[40%]">
                        <span className="flex items-center gap-2">
                          <Favicon domain={v.domain} />
                          <span className="min-w-0">
                            <span className="block truncate font-medium">{v.name}</span>
                            <span className="block truncate text-xs text-muted-foreground">{v.domain}</span>
                          </span>
                        </span>
                        {/* The pages and status columns only show from sm and md up. */}
                        <span className="mt-1 block truncate pl-6 text-xs text-muted-foreground sm:hidden" title={v.campaign ? `In ${v.campaign}` : undefined}>
                          {plural(v.views, "page")}
                          {v.campaign ? ` · In ${v.campaign}` : v.leads > 0 ? ` · ${plural(v.leads, "lead")}` : ""}
                        </span>
                      </Td>
                      <Td className={cn(PAGES, "max-w-0 w-[60%]")}>
                        <span className="block text-muted-foreground tabular-nums">{reads}</span>
                        {/* Each path on one line, wrapping between paths, so a long one doesn't hide the rest. */}
                        <span className="flex flex-wrap gap-x-3 font-mono text-xs">
                          {v.top_paths.map((p) => (
                            <span key={p} className="max-w-full truncate" title={p}>
                              {p}
                            </span>
                          ))}
                        </span>
                      </Td>
                      <Td className={cn(SOURCE, "whitespace-nowrap text-muted-foreground")}>
                        {v.source || "Direct"}
                      </Td>
                      <Td className={cn(STATUS, "whitespace-nowrap")}>
                        <Status v={v} />
                      </Td>
                      <Td className="text-right whitespace-nowrap text-muted-foreground">{timeAgo(v.last_visit)}</Td>
                    </Tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={turnPage} />
        </>
      )}
    </Card>
  );
}
