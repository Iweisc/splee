"use client";

import { useState } from "react";
import { Search, Upload, Users } from "lucide-react";
import useSWR from "swr";
import { LinkButton } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { LeadRow, Paginated, SegmentWithStats } from "@/lib/types";
import { timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { LeadModal } from "./lead-modal";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, Tr, useDebounced } from "./states";
import { LEAD_STATUS, LeadStatusBadge } from "./status";

/** Leads table with filters. Pass `segmentId` to pin it to one campaign (hides the campaign filter). */
export function LeadsView({ projectId, segmentId }: { projectId: string; segmentId?: string }) {
  const [q, setQ] = useState("");
  const [segment, setSegment] = useState("");
  const [status, setStatus] = useState("");
  const [page, setPage] = useState(1);
  const [openLead, setOpenLead] = useState<string | null>(null);
  const query = useDebounced(q);

  const { data: segments } = useSWR<SegmentWithStats[]>(segmentId ? null : `/projects/${projectId}/segments`, fetcher);
  const key = `/projects/${projectId}/leads${qs({ segment_id: segmentId ?? segment, status, q: query, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<LeadRow>>(key, fetcher, { keepPreviousData: true });
  const filtered = !!(query || segment || status);

  const filter = (set: (v: string) => void) => (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
    set(e.target.value);
    setPage(1);
  };

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
        <div className="relative min-w-56 flex-1">
          <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input value={q} onChange={filter(setQ)} placeholder="Search name, email or company" className="h-9 pl-9" />
        </div>
        {!segmentId && (
          <Select value={segment} onChange={filter(setSegment)} className="h-9 w-auto">
            <option value="">All campaigns</option>
            {segments?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        )}
        <Select value={status} onChange={filter(setStatus)} className="h-9 w-auto">
          <option value="">Any status</option>
          {Object.entries(LEAD_STATUS).map(([value, s]) => (
            <option key={value} value={value}>
              {s.label}
            </option>
          ))}
        </Select>
      </div>

      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        filtered ? (
          <EmptyState icon={<Search className="size-5" />} title="No leads match these filters" />
        ) : (
          <EmptyState
            icon={<Users className="size-5" />}
            title="No leads yet"
            action={
              <LinkButton href={`/app/p/${projectId}/import`} variant="outline" size="sm">
                <Upload className="size-3.5" /> Import a CSV
              </LinkButton>
            }
          >
            Run &quot;Find people&quot; on a campaign and the agent adds decision makers here.
          </EmptyState>
        )
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <Th>Name</Th>
                  <Th>Company</Th>
                  <Th>Email</Th>
                  {!segmentId && <Th>Campaign</Th>}
                  <Th>Status</Th>
                  <Th className="text-right">Last contacted</Th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((l) => (
                  <Tr key={l.id} onClick={() => setOpenLead(l.id)}>
                    <Td>
                      <p className="font-medium">{l.full_name || "—"}</p>
                      <p className="max-w-56 truncate text-xs text-muted-foreground">{l.title}</p>
                    </Td>
                    <Td className="max-w-48 truncate">{l.company_name}</Td>
                    <Td className="max-w-56 truncate text-muted-foreground">{l.email ?? "—"}</Td>
                    {!segmentId && <Td className="max-w-40 truncate text-muted-foreground">{l.segment_name ?? "—"}</Td>}
                    <Td>
                      <LeadStatusBadge status={l.status} />
                    </Td>
                    <Td className="text-right whitespace-nowrap text-muted-foreground">{l.last_contacted_at ? timeAgo(l.last_contacted_at) : "—"}</Td>
                  </Tr>
                ))}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={setPage} />
        </>
      )}
      <LeadModal leadId={openLead} onClose={() => setOpenLead(null)} />
    </Card>
  );
}
