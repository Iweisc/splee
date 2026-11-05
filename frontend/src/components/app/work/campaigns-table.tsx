"use client";

import { useRouter } from "next/navigation";
import type { SegmentWithStats } from "@/lib/types";
import { cn, money, num } from "@/lib/utils";
import { SegmentIcon, SegmentStatusDot } from "./status";
import { Td, Th, Tr } from "./states";

// Same columns as Explee's "Learns what works and doubles down" card. Cost/lead is spend per hot lead.
export function CampaignsTable({ projectId, segments }: { projectId: string; segments: SegmentWithStats[] }) {
  const router = useRouter();
  const order = { scaling: 0, working: 1, draft: 2, paused: 3 };
  const sorted = [...segments].sort((a, b) => order[a.status] - order[b.status] || a.position - b.position);

  return (
    <table className="w-full text-sm">
      <thead>
        <tr>
          <Th>Campaign</Th>
          <Th>Status</Th>
          <Th className="text-right">Sent</Th>
          <Th className="text-right">Replies</Th>
          <Th className="text-right">Hot</Th>
          <Th className="text-right">Cost/lead</Th>
        </tr>
      </thead>
      <tbody>
        {sorted.map((s) => (
          <Tr key={s.id} onClick={() => router.push(`/app/p/${projectId}/segments/${s.id}`)} className={cn(s.status === "paused" && "text-muted-foreground")}>
            <Td>
              <span className="flex items-center gap-2.5">
                <SegmentIcon name={s.icon} className="size-6" />
                <span className="truncate font-medium">{s.name}</span>
              </span>
            </Td>
            <Td>
              <SegmentStatusDot status={s.status} />
            </Td>
            <Td className="text-right tabular-nums">{num(s.sent)}</Td>
            <Td className="text-right tabular-nums">{num(s.replies)}</Td>
            <Td className="text-right tabular-nums">{num(s.hot)}</Td>
            <Td className="text-right tabular-nums">{s.cost_per_hot_micros === null ? "—" : money(s.cost_per_hot_micros)}</Td>
          </Tr>
        ))}
      </tbody>
    </table>
  );
}
