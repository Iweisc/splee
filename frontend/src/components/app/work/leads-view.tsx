"use client";

import { useState } from "react";
import { Download, Search, Trash2, Upload, Users, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button, buttonClass, LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Card, EmptyState, Modal } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { LeadRow, Paginated, SegmentWithStats } from "@/lib/types";
import { cn, num, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { LeadModal } from "./lead-modal";
import { SearchField } from "./search-field";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced } from "./states";
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
  // The leads ticked on this page. Changing a filter or the page clears it, so nothing off-screen is ever acted on.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const query = useDebounced(q);

  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const filters = { segment_id: segmentId ?? segment, status, q: query };
  const key = `/projects/${projectId}/leads${qs({ ...filters, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<LeadRow>>(key, fetcher, { keepPreviousData: true });
  const filtered = !!(query || segment || status);

  const filter = (set: (v: string) => void) => (e: React.ChangeEvent<HTMLInputElement | HTMLSelectElement>) => {
    set(e.target.value);
    setPage(1);
    setPicked(new Set());
  };
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const pageIds = data?.items.map((l) => l.id) ?? [];
  const allPicked = pageIds.length > 0 && pageIds.every((id) => picked.has(id));

  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <PickedBar
          projectId={projectId}
          ids={[...picked]}
          segments={segments ?? []}
          onDone={async () => {
            setPicked(new Set());
            // Emptying the last page would strand you past the end, where the empty state hides the pager.
            const fresh = await mutate();
            if (fresh && fresh.items.length === 0 && page > 1) setPage(Math.max(1, Math.ceil(fresh.total / PER_PAGE)));
          }}
          onClear={() => setPicked(new Set())}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          {/* The search takes a row's spare room, and never gets narrower than its placeholder. The filters only stretch
              to fill a row they wrapped onto. Status and Export wrap together, so Export never sits alone on a row. */}
          <SearchField
            value={q}
            onChange={(v) => {
              setQ(v);
              setPage(1);
              setPicked(new Set());
            }}
            placeholder="Search name, email or company"
            label="Search leads"
            className="min-w-64 flex-[999_1_0%] md:min-w-72"
          />
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
      )}

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
            Run &quot;Find people&quot; {segmentId ? "above" : "on a campaign"} and the agent adds decision makers here.
          </EmptyState>
        )
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                {/* Narrower screens drop the least useful columns instead of scrolling sideways. The lead's modal has all of them. */}
                <tr>
                  <Th className="w-[1%] pr-0">
                    <input
                      type="checkbox"
                      checked={allPicked}
                      onChange={() => setPicked(allPicked ? new Set() : new Set(pageIds))}
                      aria-label="Select every lead on this page"
                      className="block accent-black"
                    />
                  </Th>
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
                    {/* The whole cell ignores row clicks, so a near miss on the box doesn't open the lead. */}
                    <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
                      <input
                        type="checkbox"
                        checked={picked.has(l.id)}
                        onChange={() => toggle(l.id)}
                        aria-label={`Select ${l.full_name || l.email || "lead"}`}
                        className="block accent-black"
                      />
                    </Td>
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
          <Pager
            page={page}
            total={data.total}
            onChange={(p) => {
              setPage(p);
              setPicked(new Set());
            }}
          />
        </>
      )}
      <LeadModal leadId={openLead} onClose={() => setOpenLead(null)} />
    </Card>
  );
}

/** Replaces the filters while leads are ticked: move them into a campaign, or delete them. */
function PickedBar({
  projectId,
  ids,
  segments,
  onDone,
  onClear,
}: {
  projectId: string;
  ids: string[];
  segments: SegmentWithStats[];
  onDone: () => void;
  onClear: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const count = ids.length === 1 ? "1 lead" : `${num(ids.length)} leads`;

  const run = async (path: string, body: Record<string, unknown>, done: string) => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/leads/${path}`, {
        method: "POST",
        body: { ids, ...body },
      });
      toast.success(done);
      setConfirming(false);
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const move = (value: string) => {
    if (value === "") return;
    const segment = segments.find((s) => s.id === value);
    run("move", { segment_id: segment?.id ?? null }, segment ? `Moved ${count} to ${segment.name}` : `Took ${count} out of their campaign`);
  };

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
      <Button variant="ghost" size="icon" onClick={onClear} aria-label="Clear the selection" className="size-9">
        <X className="size-4" />
      </Button>
      <p className="mr-auto text-sm font-medium">{count} selected</p>
      {/* Delete comes before the campaign menu so that on a phone it shares the first row with the count and the menu
          fills the second. After the menu, it took a third row of its own. */}
      <Button variant="outline" size="sm" onClick={() => setConfirming(true)} disabled={busy} className="h-9 text-danger">
        <Trash2 className="size-3.5" /> Delete
      </Button>
      {/* It always shows "Move to campaign…", so it gets that label's width rather than the longest campaign name's,
          which pushed it onto a row of its own on a tablet. The open list still fits the names. */}
      <Select
        value=""
        onChange={(e) => move(e.target.value)}
        disabled={busy}
        className="h-9 w-auto flex-auto sm:w-52 sm:flex-none"
        aria-label="Move to campaign"
      >
        <option value="">Move to campaign…</option>
        {segments.map((s) => (
          <option key={s.id} value={s.id}>
            {s.name}
          </option>
        ))}
        <option value="none">No campaign</option>
      </Select>
      <Modal open={confirming} onClose={() => setConfirming(false)} title={`Delete ${count}?`} className="max-w-md">
        <p className="text-sm text-muted-foreground">Their emails, notes and webhook history go too. You can&apos;t undo this.</p>
        <div className="mt-5 flex justify-end gap-2">
          <Button variant="ghost" onClick={() => setConfirming(false)}>
            Cancel
          </Button>
          <Button variant="danger" onClick={() => run("delete", {}, `Deleted ${count}`)} loading={busy}>
            <Trash2 className="size-4" /> Delete
          </Button>
        </div>
      </Modal>
    </div>
  );
}
