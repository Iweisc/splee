"use client";

import { useParams } from "next/navigation";
import useSWR from "swr";
import { fetcher } from "@/hooks/use-me";
import type { ReplyCounts } from "@/lib/types";
import { num, percent } from "@/lib/utils";
import { Section } from "./layout";

/**
 * Sent emails and the leads who answered them, one row per group from `/projects/:id/stats/<stat>`. Hidden until something
 * was sent. Kept apart from the settings above it, so changing them doesn't relabel what was already sent.
 */
export function RepliesSection<T extends ReplyCounts>({
  stat,
  title,
  column,
  label,
}: {
  stat: string;
  title: string;
  column: string;
  label: (row: T) => string;
}) {
  const { projectId } = useParams<{ projectId: string }>();
  const { data } = useSWR<T[]>(`/projects/${projectId}/stats/${stat}`, fetcher);
  if (!data?.length) return null;
  return (
    <Section title={title} description="Each lead's first reply counts for the last email they got before it.">
      {/* On a phone, at 14px with no padding, big counts ran into each other ("1,234 10.0%"). 13px leaves room for the
          gaps without wrapping "00:00–01:00". */}
      <table className="w-full text-sm tabular-nums max-sm:text-[13px]">
        <thead className="text-left text-muted-foreground">
          <tr>
            <th className="pb-2 font-normal whitespace-nowrap">{column}</th>
            <th className="pb-2 pl-2 text-right font-normal">Sent</th>
            <th className="pb-2 pl-2 text-right font-normal">Replied</th>
            <th className="pb-2 pl-2 text-right font-normal">Rate</th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {data.map((row) => (
            <tr key={label(row)}>
              <td className="py-2 font-medium whitespace-nowrap">{label(row)}</td>
              <td className="py-2 pl-2 text-right">{num(row.sent)}</td>
              <td className="py-2 pl-2 text-right">{num(row.replied)}</td>
              <td className="py-2 pl-2 text-right">{percent(row.replied / row.sent)}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Section>
  );
}
