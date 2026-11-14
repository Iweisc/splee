"use client";

import { ExternalLink, PanelsTopLeft } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { LeadPage, SegmentWithStats } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";
import { MicroLabel } from "./states";

/** Whether the campaign's first emails come with a personal page for each lead. Saved as soon as it's switched. */
export function LeadPagesCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const save = (on: boolean) =>
    run("lead_pages", () =>
      mutate(
        `/segments/${segment.id}`,
        api<SegmentWithStats>(`/segments/${segment.id}`, {
          method: "PATCH",
          body: { lead_pages: on },
        }),
        { revalidate: false },
      ),
    );

  return (
    <Card className="flex items-start gap-4 p-5">
      <div className="min-w-0 flex-1">
        <MicroLabel className="flex items-center gap-1.5">
          <PanelsTopLeft className="size-3.5" /> Personal pages
        </MicroLabel>
        <p className="mt-1 text-sm font-medium">A page written for each lead</p>
        <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
          The writer adds a short page to each first email: a headline about their company, three ways you&apos;d help them, proof from your materials
          and your booking link. When a lead opens it, their score goes up and your chats hear about it. Applies to emails written from now on.
        </p>
      </div>
      <Switch checked={segment.lead_pages} onChange={save} disabled={pending === "lead_pages"} label="A page written for each lead" />
    </Card>
  );
}

/** The lead's page on the lead's window: its link and whether they opened it. Nothing when they have none. */
export function LeadPageLink({ leadId }: { leadId: string }) {
  const { data: page } = useSWR<LeadPage | null>(`/leads/${leadId}/page`, fetcher);
  if (!page) return null;
  return (
    <div>
      <MicroLabel className="mb-2">Personal page</MicroLabel>
      <div className="flex min-w-0 items-center gap-2 text-sm">
        <PanelsTopLeft className="size-3.5 shrink-0 text-muted-foreground" />
        <a href={page.url} target="_blank" rel="noreferrer" className="inline-flex min-w-0 items-center gap-1 hover:underline">
          <span className="truncate">{page.headline}</span>
          <ExternalLink className="size-3 shrink-0" />
        </a>
        <span className="ml-auto shrink-0 text-xs text-muted-foreground">
          {page.views > 0 && page.last_viewed_at
            ? `Opened ${page.views === 1 ? "once" : plural(page.views, "time")}, last ${timeAgo(page.last_viewed_at)}`
            : "Not opened yet"}
        </span>
      </div>
    </div>
  );
}
