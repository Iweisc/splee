"use client";

import useSWR from "swr";
import { Field, Select } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import type { SegmentWithStats } from "@/lib/types";

/** Picks the campaign new leads join, for imports and the form webhook. */
export function CampaignField({
  projectId,
  value,
  onChange,
  className = "w-72 max-w-full",
}: {
  projectId: string;
  value: string;
  onChange: (segmentId: string) => void;
  className?: string;
}) {
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  return (
    <Field label="Add to campaign" className={className} hint="Optional.">
      <Select value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">No campaign</option>
        {segments?.map((s) => (
          <option key={s.id} value={s.id}>
            {s.name}
          </option>
        ))}
      </Select>
    </Field>
  );
}
