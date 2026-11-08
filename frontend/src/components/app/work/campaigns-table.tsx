"use client";

import { useRouter } from "next/navigation";
import type { SegmentWithStats } from "@/lib/types";
import { cn, money, num } from "@/lib/utils";
import { SegmentIcon, SegmentStatusDot } from "./status";
import { Td, Th, Tr } from "./states";

// Columns that only show once the table is wide enough for them. Narrower tables show the status under the name.
const STATUS = "hidden @md:table-cell";
const COUNTS = "hidden @lg:table-cell";
const COST = "hidden @xl:table-cell";

// Same columns as Explee's "Learns what works and doubles down" card. Cost/lead is spend per hot lead.
export function CampaignsTable({ projectId, segments }: { projectId: string; segments: SegmentWithStats[] }) {
  const router = useRouter();
  const order = { scaling: 0, working: 1, draft: 2, paused: 3 };
  const sorted = [...segments].sort((a, b) => order[a.status] - order[b.status] || a.position - b.position);

  return (
    // The overview puts this table in a narrow column on laptops, so columns follow the table's own width, not the screen's.
    <div className="@container overflow-x-auto">
      <table className="w-full text-sm">
        <thead>
          <tr>
            <Th>Campaign</Th>
            <Th className={STATUS}>Status</Th>
            <Th className={cn(COUNTS, "text-right")}>Sent</Th>
            <Th className={cn(COUNTS, "text-right")}>Replies</Th>
            <Th className="text-right">Hot</Th>
            <Th className={cn(COST, "text-right")}>Cost/lead</Th>
          </tr>
        </thead>
        <tbody>
          {sorted.map((s) => (
            <Tr key={s.id} onClick={() => router.push(`/app/p/${projectId}/segments/${s.id}`)} className={cn(s.status === "paused" && "text-muted-foreground")}>
              <Td>
                <span className="flex items-center gap-2.5">
                  <SegmentIcon name={s.icon} className="size-6" />
                  <span>
                    <span className="block font-medium">{s.name}</span>
                    <span className="@md:hidden">
                      <SegmentStatusDot status={s.status} />
                    </span>
                  </span>
                </span>
              </Td>
              <Td className={STATUS}>
                <SegmentStatusDot status={s.status} />
              </Td>
              <Td className={cn(COUNTS, "text-right tabular-nums")}>{num(s.sent)}</Td>
              <Td className={cn(COUNTS, "text-right tabular-nums")}>{num(s.replies)}</Td>
              <Td className="text-right tabular-nums">{num(s.hot)}</Td>
              <Td className={cn(COST, "text-right tabular-nums")}>{s.cost_per_hot_micros === null ? "—" : money(s.cost_per_hot_micros)}</Td>
            </Tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
