"use client";

import { useState } from "react";
import { Building2 } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { Company, Paginated } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced } from "./states";
import { SearchField } from "./search-field";
import { FitScore } from "./status";

const STATUS_LABEL: Record<Company["status"], string> = {
  new: "New",
  people_found: "People found",
  no_people: "No people found",
  excluded: "Excluded",
};

// Columns that only show once the table is wide enough for them, least useful last.
const ACTION = "hidden @lg:table-cell";
const STATUS = "hidden @2xl:table-cell";
const REASON = "hidden @4xl:table-cell";
// Size and location wait for a wider table than the others: on a 1440px screen they squeezed the names and reasons.
const FIRMOGRAPHICS = "hidden @[80rem]:table-cell";
// The name and reason split whatever width the other columns leave, so a name only cuts off when the table is out of
// room. max-w-0 keeps a long name from widening the table. The rest are 1% wide so they shrink to their content: a
// column with no width would take the spare room instead.
const NARROW = "w-[1%]";

export function CompaniesTable({ projectId, segmentId }: { projectId: string; segmentId: string }) {
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);
  const query = useDebounced(q);
  const key = `/projects/${projectId}/companies${qs({ segment_id: segmentId, q: query, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<Company>>(key, fetcher, { keepPreviousData: true });

  const toggleExcluded = async (c: Company) => {
    try {
      const updated = await api<Company>(`/companies/${c.id}`, { method: "PATCH", body: { status: c.status === "excluded" ? "new" : "excluded" } });
      mutate((d) => d && { ...d, items: d.items.map((x) => (x.id === c.id ? updated : x)) }, { revalidate: false });
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <Card className="overflow-hidden">
      <div className="border-b border-border p-3">
        <SearchField
          value={q}
          onChange={(v) => {
            setQ(v);
            setPage(1);
          }}
          placeholder="Search companies"
          label="Search companies"
          className="max-w-sm"
        />
      </div>
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Building2 className="size-5" />} title={query ? "No companies match" : "No companies yet"}>
          {!query && "Use \"Find companies\" above and the agent researches matching companies on the web."}
        </EmptyState>
      ) : (
        <>
          <div className="@container overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <Th className="w-[50%]">Company</Th>
                  <Th className={cn(FIRMOGRAPHICS, NARROW)}>Size</Th>
                  <Th className={cn(FIRMOGRAPHICS, NARROW)}>Location</Th>
                  <Th className={NARROW}>Fit</Th>
                  <Th className={cn(REASON, "w-[50%]")}>Why it fits</Th>
                  <Th className={cn(STATUS, NARROW)}>Status</Th>
                  <Th className={cn(ACTION, NARROW)} />
                </tr>
              </thead>
              <tbody>
                {data.items.map((c) => (
                  <Tr key={c.id} className={cn(c.status === "excluded" && "text-muted-foreground")}>
                    <Td className="max-w-0">
                      <a href={`https://${c.domain}`} target="_blank" rel="noreferrer" className="flex items-center gap-2 hover:underline">
                        <Favicon domain={c.domain} />
                        <span className="min-w-0">
                          <span className="block truncate font-medium">{c.name || c.domain}</span>
                          <span className="block truncate text-xs text-muted-foreground">{c.domain}</span>
                        </span>
                      </a>
                    </Td>
                    <Td className={cn(FIRMOGRAPHICS, "whitespace-nowrap tabular-nums")}>{c.employees || "—"}</Td>
                    <Td className={cn(FIRMOGRAPHICS, "whitespace-nowrap")}>{[c.city, c.country_code].filter(Boolean).join(", ") || "—"}</Td>
                    {/* The reason doubles as a tooltip for when its own column is hidden. */}
                    <Td title={c.fit_reason}>
                      <FitScore value={c.fit_score} />
                    </Td>
                    <Td className={cn(REASON, "min-w-60")}>
                      <p className="line-clamp-2 text-xs text-muted-foreground">{c.fit_reason}</p>
                    </Td>
                    <Td className={STATUS}>
                      <Badge tone={c.status === "people_found" ? "green" : "neutral"}>{STATUS_LABEL[c.status]}</Badge>
                    </Td>
                    <Td className={cn(ACTION, "text-right")}>
                      <Button size="sm" variant="ghost" onClick={() => toggleExcluded(c)}>
                        {c.status === "excluded" ? "Include" : "Exclude"}
                      </Button>
                    </Td>
                  </Tr>
                ))}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={setPage} />
        </>
      )}
    </Card>
  );
}
