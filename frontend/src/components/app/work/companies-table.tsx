"use client";

import { useState } from "react";
import { Building2, Search } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { Company, Paginated } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced } from "./states";
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
const FIRMOGRAPHICS = "hidden @[70rem]:table-cell";

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
        <div className="relative max-w-sm">
          <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input
            value={q}
            onChange={(e) => {
              setQ(e.target.value);
              setPage(1);
            }}
            placeholder="Search companies"
            className="h-9 pl-9"
          />
        </div>
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
                  <Th>Company</Th>
                  <Th className={FIRMOGRAPHICS}>Size</Th>
                  <Th className={FIRMOGRAPHICS}>Location</Th>
                  <Th>Fit</Th>
                  <Th className={REASON}>Why it fits</Th>
                  <Th className={STATUS}>Status</Th>
                  <Th className={ACTION} />
                </tr>
              </thead>
              <tbody>
                {data.items.map((c) => (
                  <Tr key={c.id} className={cn(c.status === "excluded" && "text-muted-foreground")}>
                    <Td>
                      <a href={`https://${c.domain}`} target="_blank" rel="noreferrer" className="flex items-center gap-2 hover:underline">
                        <Favicon domain={c.domain} />
                        <span className="max-w-40 min-w-0 @md:max-w-48">
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
                    <Td className={cn(REASON, "max-w-md min-w-60")}>
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
