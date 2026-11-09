"use client";

import { useState } from "react";
import { Download, Search, Upload, Users } from "lucide-react";
import useSWR from "swr";
import { buttonClass, LinkButton } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { LeadRow, Paginated, SegmentWithStats } from "@/lib/types";
import { cn, num, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { LeadModal } from "./lead-modal";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, Tr, useDebounced } from "./states";
import { LEAD_STATUS, LeadStatusBadge } from "./status";

/** Leads table with filters. Pass `segmentId` to pin it to one campaign (hides the campaign filter). */
// Columns that only show once there is room for them.
const COMPANY = "hidden sm:table-cell";
const CONTACTED = "hidden md:table-cell";
const WIDE = "hidden xl:table-cell";
// The text columns split whatever width the status and date leave, by their percentages, so they only cut text off
// when the screen is short of room. max-w-0 keeps long text from widening the table. Status and date are 1% wide so
// they shrink to their content: a column with no width at all would take the spare room instead.
const FILL = "max-w-0";

export function LeadsView({ projectId, segmentId }: { projectId: string; segmentId?: string }) {
  const [q, setQ] = useState("");
  const [segment, setSegment] = useState("");
  const [status, setStatus] = useState("");
  const [page, setPage] = useState(1);
  const [openLead, setOpenLead] = useState<string | null>(null);
  const query = useDebounced(q);

  const { data: segments } = useSWR<SegmentWithStats[]>(segmentId ? null : `/projects/${projectId}/segments`, fetcher);
  const filters = { segment_id: segmentId ?? segment, status, q: query };
  const key = `/projects/${projectId}/leads${qs({ ...filters, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<LeadRow>>(key, fetcher, { keepPreviousData: true });
  const filtered = !!(query || segment || status);

  const filter = (set: (v: string) => void) => (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
    set(e.target.value);
    setPage(1);
  };

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
        {/* The search takes a row's spare room, and never gets narrower than its placeholder. The filters only stretch
            to fill a row they wrapped onto. Status and Export wrap together, so Export never sits alone on a row. */}
        <div className="relative min-w-64 flex-[999_1_0%]">
          <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
          <Input value={q} onChange={filter(setQ)} placeholder="Search name, email or company" className="h-9 pl-9" />
        </div>
        {!segmentId && (
          <Select value={segment} onChange={filter(setSegment)} className="h-9 w-auto flex-auto" aria-label="Campaign">
            <option value="">All campaigns</option>
            {segments?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        )}
        <div className="flex flex-auto flex-wrap gap-2">
          <Select value={status} onChange={filter(setStatus)} className="h-9 w-auto flex-auto" aria-label="Status">
            <option value="">Any status</option>
            {Object.entries(LEAD_STATUS).map(([value, s]) => (
              <option key={value} value={value}>
                {s.label}
              </option>
            ))}
          </Select>
          {!!data?.total && (
            // A plain link so the browser streams the file with the session cookie. It exports every match, not just this page.
            <a href={`/api/projects/${projectId}/leads/export${qs(filters)}`} download className={buttonClass("outline", "sm", "h-9 flex-auto")}>
              <Download className="size-3.5" /> Export {data.total === 1 ? "1 lead" : `${num(data.total)} leads`}
            </a>
          )}
        </div>
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
                {/* Narrower screens drop the least useful columns instead of scrolling sideways. The lead's modal has all of them. */}
                <tr>
                  <Th>Name</Th>
                  <Th className={COMPANY}>Company</Th>
                  <Th className={WIDE}>Email</Th>
                  {!segmentId && <Th className={WIDE}>Campaign</Th>}
                  <Th className="w-[1%]">Status</Th>
                  <Th className={cn(CONTACTED, "w-[1%] text-right whitespace-nowrap")}>Last contacted</Th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((l) => (
                  <Tr key={l.id} onClick={() => setOpenLead(l.id)}>
                    <Td className={cn(FILL, "w-[30%]")}>
                      {/* Its click bubbles to the row, which opens the lead. */}
                      <button className="block text-left font-medium">{l.full_name || "—"}</button>
                      <p className="truncate text-xs text-muted-foreground">{l.title}</p>
                    </Td>
                    <Td className={cn(COMPANY, FILL, "w-[25%] truncate")}>{l.company_name}</Td>
                    <Td className={cn(WIDE, FILL, "w-[25%] truncate text-muted-foreground")}>{l.email ?? "—"}</Td>
                    {!segmentId && <Td className={cn(WIDE, FILL, "w-[20%] truncate text-muted-foreground")}>{l.segment_name ?? "—"}</Td>}
                    <Td>
                      <LeadStatusBadge status={l.status} />
                    </Td>
                    <Td className={cn(CONTACTED, "text-right whitespace-nowrap text-muted-foreground")}>{l.last_contacted_at ? timeAgo(l.last_contacted_at) : "—"}</Td>
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
