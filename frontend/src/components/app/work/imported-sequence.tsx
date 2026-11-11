"use client";

import useSWR from "swr";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { SegmentSteps } from "@/lib/types";
import { plural } from "@/lib/utils";
import { MicroLabel } from "./states";

/** Splee's fields like {first_name}, and a tool's variable with no match, like {{icebreaker}}, which stays as it was. */
const TOKEN = /(\{\{[^{}]*\}\}|\{[a-z_]+\})/g;

function Highlighted({ text }: { text: string }) {
  return (
    <>
      {text.split(TOKEN).map((part, i) =>
        part.startsWith("{{") ? (
          <mark key={i} className="rounded bg-amber-100 px-0.5 text-amber-900">
            {part}
          </mark>
        ) : /^\{[a-z_]+\}$/.test(part) ? (
          <span key={i} className="rounded bg-brand-50 px-0.5 font-medium text-brand-700">
            {part}
          </span>
        ) : (
          part
        ),
      )}
    </>
  );
}

/** The emails a campaign brought over from another tool keeps. A campaign made in Splee has none and shows nothing. */
export function ImportedSequence({ segmentId }: { segmentId: string }) {
  const { data } = useSWR<SegmentSteps>(`/segments/${segmentId}/steps`, fetcher);
  if (!data || data.steps.length === 0) return null;
  const unmatched = data.steps.some((s) => /\{\{[^{}]*\}\}/.test(s.subject + s.body));
  return (
    <Card className="flex flex-col gap-4 p-5">
      <div>
        <MicroLabel>Sequence from {data.imported_from ?? "another tool"}</MicroLabel>
        <p className="mt-1 text-sm text-muted-foreground">
          Splee sends these as written, with each lead&apos;s details in place of fields like {"{first_name}"}.
          {unmatched && " Splee has no field for the ones in double braces, so the writer fills those in for each lead."}
        </p>
      </div>
      <ol className="flex flex-col gap-3">
        {data.steps.map((s) => (
          <li key={s.position} className="rounded-xl border border-border p-4">
            <p className="text-xs font-medium text-muted-foreground">
              {s.position === 0 ? "Email 1" : `Follow-up ${s.position}, ${plural(s.delay_days, "day")} later`}
            </p>
            {s.subject && (
              <p className="mt-1.5 font-medium wrap-anywhere">
                <Highlighted text={s.subject} />
              </p>
            )}
            {!s.subject && s.position > 0 && <p className="mt-1.5 text-sm text-muted-foreground">Sent as a reply in the same thread</p>}
            <p className="mt-2 text-sm whitespace-pre-wrap wrap-anywhere">
              <Highlighted text={s.body} />
            </p>
          </li>
        ))}
      </ol>
    </Card>
  );
}
