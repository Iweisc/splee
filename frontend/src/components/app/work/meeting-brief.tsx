"use client";

import { useState } from "react";
import { Loader2, NotebookText, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { MeetingBrief } from "@/lib/types";
import { dayTime, timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

/** The brief the agent writes before a lead's call: on its own a day ahead, or now from the button. */
export function MeetingBriefCard({ leadId, meetingAt }: { leadId: string; meetingAt: string | null }) {
  const key = `/leads/${leadId}/brief`;
  const { data, mutate } = useSWR<MeetingBrief | null>(key, fetcher, { refreshInterval: (b) => (b?.status === "writing" ? 3000 : 0) });
  const [asking, setAsking] = useState(false);
  if (data === undefined) return null;

  const write = async () => {
    setAsking(true);
    try {
      await mutate(api<MeetingBrief>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setAsking(false);
    }
  };
  const writing = data?.status === "writing";
  const brief = data?.status === "ready" ? data.brief : null;

  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <MicroLabel className="flex items-center gap-1.5">
            <NotebookText className="size-3.5" /> Meeting brief
          </MicroLabel>
          {!writing && (
            <p className="mt-1 text-xs text-muted-foreground">
              {brief && data?.written_at
                ? `Written ${timeAgo(data.written_at)}${data.meeting_at ? ` for the call on ${dayTime(data.meeting_at)}` : ""}`
                : meetingAt
                  ? `Written on its own a day before the call on ${dayTime(meetingAt)}, then posted to your chats.`
                  : "Who they are, what was said so far and what to ask, from what Splee knows."}
            </p>
          )}
        </div>
        {!writing && (
          <Button size="sm" variant={brief ? "ghost" : "outline"} onClick={write} loading={asking}>
            {brief ? <RefreshCw className="size-3.5" /> : <NotebookText className="size-3.5" />}
            {brief ? "Write again" : "Write it now"}
          </Button>
        )}
      </div>

      {writing && (
        <p className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" /> Writing the brief…
        </p>
      )}
      {data?.status === "failed" && <p className="mt-3 text-sm text-danger">The brief couldn&apos;t be written. Try again.</p>}

      {brief && (
        <div className="mt-3 flex flex-col gap-4">
          <p className="text-sm text-pretty">{brief.summary}</p>
          <div className="grid gap-x-6 gap-y-4 sm:grid-cols-2">
            <Part title="About them" items={brief.about} />
            <Part title="Lately" items={brief.signals} />
            {brief.so_far && (
              <div className="sm:col-span-2">
                <MicroLabel className="mb-1">So far</MicroLabel>
                <p className="text-sm text-pretty text-foreground/80">{brief.so_far}</p>
              </div>
            )}
            <Part title="Bring up" items={brief.talking_points} />
            <Part title="Ask" items={brief.questions} numbered />
            <Part title="Be ready for" items={brief.watch_outs} />
          </div>
        </div>
      )}
    </section>
  );
}

function Part({ title, items, numbered }: { title: string; items: string[]; numbered?: boolean }) {
  if (!items.length) return null;
  const List = numbered ? "ol" : "ul";
  return (
    <div>
      <MicroLabel className="mb-1">{title}</MicroLabel>
      <List className={`flex flex-col gap-1 pl-4 text-sm text-foreground/80 ${numbered ? "list-decimal" : "list-disc"}`}>
        {items.map((item) => (
          <li key={item} className="text-pretty">
            {item}
          </li>
        ))}
      </List>
    </div>
  );
}
