"use client";

import { Fragment, useState } from "react";
import { Building2 } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { Company, CompanyRow, Paginated } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced } from "./states";
import { HiringBadge } from "./hiring";
import { SearchField } from "./search-field";
import { TechBadges, useTech } from "./tech";
import { FitScore } from "./status";
import { ScoreButton, ScoreFilter, ScoreHeader, ScoreReasons } from "./score";

const STATUS_LABEL: Record<Company["status"], string> = {
  new: "New",
  people_found: "People found",
  no_people: "No people found",
  excluded: "Excluded",
};

// Columns that only show once the table is wide enough for them, least useful last.
const ACTION = "hidden @lg:table-cell";
// The agent's research fit leaves with the Exclude column, so a phone has room for the names beside the score.
const FIT = ACTION;
const STATUS = "hidden @2xl:table-cell";
const REASON = "hidden @4xl:table-cell";
// Size and location wait for a wider table than the others: on a 1440px screen they squeezed the names and reasons.
const FIRMOGRAPHICS = "hidden @[80rem]:table-cell";
// The name and reason split whatever width the other columns leave, so a name only cuts off when the table is out of
// room. max-w-0 keeps a long name from widening the table. The rest are 1% wide so they shrink to their content: a
// column with no width would take the spare room instead.
const NARROW = "w-[1%]";

/** A campaign's companies, or every company in the project without `segmentId`. */
export function CompaniesTable({ projectId, segmentId }: { projectId: string; segmentId?: string }) {
  const [q, setQ] = useState("");
  const [minScore, setMinScore] = useState("");
  // "score", or the best research fit first.
  const [sort, setSort] = useState("");
  const [page, setPage] = useState(1);
  // The company whose score reasons show under its row.
  const [why, setWhy] = useState<string | null>(null);
  const query = useDebounced(q);
  const key = `/projects/${projectId}/companies${qs({ segment_id: segmentId, q: query, min_score: minScore, sort, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<CompanyRow>>(key, fetcher, { keepPreviousData: true });
  const { data: tech } = useTech(projectId);

  const toggleExcluded = async (c: CompanyRow) => {
    try {
      const updated = await api<Company>(`/companies/${c.id}`, { method: "PATCH", body: { status: c.status === "excluded" ? "new" : "excluded" } });
      // The PATCH answers with the plain company, so the row keeps its hiring roles and tools.
      mutate((d) => d && { ...d, items: d.items.map((x) => (x.id === c.id ? { ...x, ...updated } : x)) }, { revalidate: false });
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap gap-2 border-b border-border p-3">
        <SearchField
          value={q}
          onChange={(v) => {
            setQ(v);
            setPage(1);
          }}
          placeholder="Search companies"
          label="Search companies"
          className="min-w-56 flex-[999_1_0%] sm:max-w-sm"
        />
        <ScoreFilter
          value={minScore}
          onChange={(v) => {
            setMinScore(v);
            setPage(1);
          }}
          className="h-9 w-auto flex-auto sm:flex-none"
        />
      </div>
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Building2 className="size-5" />} title={query || minScore ? "No companies match" : "No companies yet"}>
          {!query &&
            !minScore &&
            (segmentId
              ? "Use \"Find companies\" above and the agent researches matching companies on the web."
              : "Companies you add to a campaign show here with their scores.")}
        </EmptyState>
      ) : (
        <>
          <div className="@container overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  {/* The name gets the bigger share since the score came in: at 50% its Hiring and tool badges wrapped on a 1280px screen. */}
                  <Th className="w-[60%]">Company</Th>
                  <Th className={cn(FIRMOGRAPHICS, NARROW)}>Size</Th>
                  <Th className={cn(FIRMOGRAPHICS, NARROW)}>Location</Th>
                  <Th className={cn(NARROW, "@max-lg:px-2")}>
                    <ScoreHeader
                      sorted={sort === "score"}
                      onSort={() => {
                        setSort(sort ? "" : "score");
                        setPage(1);
                      }}
                    />
                  </Th>
                  {/* The agent's research fit, from before Splee knew any of the company's signals. */}
                  <Th className={cn(FIT, NARROW)}>Fit</Th>
                  <Th className={cn(REASON, "w-[40%]")}>Why it fits</Th>
                  <Th className={cn(STATUS, NARROW)}>Status</Th>
                  <Th className={cn(ACTION, NARROW)} />
                </tr>
              </thead>
              <tbody>
                {data.items.map((c) => {
                  const toggle = (className?: string) => (
                    <Button size="sm" variant="ghost" className={className} onClick={() => toggleExcluded(c)}>
                      {c.status === "excluded" ? "Include" : "Exclude"}
                    </Button>
                  );
                  return (
                    <Fragment key={c.id}>
                      <Tr className={cn(c.status === "excluded" && "text-muted-foreground", why === c.id && "bg-accent/60")}>
                        <Td className="max-w-0">
                          <a href={`https://${c.domain}`} target="_blank" rel="noreferrer" className="flex items-center gap-2 hover:underline">
                            <Favicon domain={c.domain} />
                            <span className="min-w-0">
                              <span className="block truncate font-medium">{c.name || c.domain}</span>
                              <span className="block truncate text-xs text-muted-foreground">{c.domain}</span>
                            </span>
                          </a>
                          <HiringBadge roles={c.hiring} className="mt-1 pl-6" />
                          <TechBadges techs={c.tech} picks={tech?.picks ?? []} className="mt-1 pl-6" />
                        </Td>
                        <Td className={cn(FIRMOGRAPHICS, "whitespace-nowrap tabular-nums")}>{c.employees || "—"}</Td>
                        <Td className={cn(FIRMOGRAPHICS, "whitespace-nowrap")}>{[c.city, c.country_code].filter(Boolean).join(", ") || "—"}</Td>
                        <Td className="@max-lg:px-2">
                          <ScoreButton item={c} open={why === c.id} onToggle={() => setWhy(why === c.id ? null : c.id)} />
                          {/* A narrow table has no room for the Exclude column, so its button sits under the score instead. */}
                          {toggle("-ml-3 flex @lg:hidden")}
                        </Td>
                        {/* The reason doubles as a tooltip for when its own column is hidden. */}
                        <Td title={c.fit_reason} className={FIT}>
                          <FitScore value={c.fit_score} />
                        </Td>
                        <Td className={cn(REASON, "min-w-48")}>
                          <p className="line-clamp-2 text-xs text-muted-foreground">{c.fit_reason}</p>
                        </Td>
                        <Td className={STATUS}>
                          <Badge tone={c.status === "people_found" ? "green" : "neutral"}>{STATUS_LABEL[c.status]}</Badge>
                        </Td>
                        <Td className={cn(ACTION, "text-right")}>{toggle()}</Td>
                      </Tr>
                      {why === c.id && (
                        // Spans every column, hidden ones included, so it's the table's full width at any size.
                        <tr className="bg-accent/60">
                          <td colSpan={8} className="px-4 pt-1 pb-3">
                            <ScoreReasons item={c} />
                          </td>
                        </tr>
                      )}
                    </Fragment>
                  );
                })}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={setPage} />
        </>
      )}
    </Card>
  );
}
