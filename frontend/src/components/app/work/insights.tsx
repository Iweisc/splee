"use client";

import { useState } from "react";
import { ArrowRight, BookmarkPlus, Check, Loader2, MessageSquareQuote, RefreshCw, Sparkles } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { CampaignInsightsView, SegmentWithStats } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

const FIELDS: Record<string, string> = { angle: "Angle", pain_points: "Pain points" };

/** What the agent read in the campaign's replies: what works, the objections and how often, and changes to the campaign. */
export function InsightsCard({ segment }: { segment: SegmentWithStats }) {
  const key = `/segments/${segment.id}/insights`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data, mutate } = useSWR<CampaignInsightsView>(key, fetcher, { refreshInterval: (v) => (v?.latest?.status === "writing" ? 3000 : 0) });
  const [busy, setBusy] = useState<string | null>(null);
  if (!data) return null;

  const act = async (what: string, call: () => Promise<unknown>) => {
    setBusy(what);
    try {
      await call();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const read = () => act("read", () => mutate(api<CampaignInsightsView>(key, { method: "POST" }), { revalidate: false }));
  const latest = data.latest;
  const insights = latest?.status === "ready" ? latest.insights : null;
  const writing = latest?.status === "writing";
  const enough = data.replies >= data.min_replies;
  const use = (index: number) =>
    act(`use-${index}`, async () => {
      await mutate(api<CampaignInsightsView>(`${key}/${latest!.id}/apply`, { method: "POST", body: { index } }), { revalidate: false });
      await mutateAll(`/segments/${segment.id}`);
      toast.success("The campaign's next emails will use it");
    });
  const save = (index: number, title: string, body: string) =>
    act(`save-${index}`, async () => {
      await api(`/projects/${segment.project_id}/saved-replies`, { method: "POST", body: { title: title.slice(0, 80), body } });
      toast.success("Added to your saved replies");
    });

  return (
    <Card className="p-5">
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <MicroLabel className="flex items-center gap-1.5">
            <MessageSquareQuote className="size-3.5" /> What replies say
          </MicroLabel>
          <p className="mt-1 text-sm text-pretty text-muted-foreground">
            {insights && latest
              ? `From ${plural(latest.replies, "reply", "replies")}, read ${timeAgo(latest.created_at)}.`
              : writing
                ? null
                : enough
                  ? "The agent reads the replies and tells you what works, what people object to and how to change the campaign."
                  : `Once ${data.min_replies} people reply, the agent can read them for you. ${plural(data.replies, "reply", "replies")} so far.`}
          </p>
        </div>
        {!writing && enough && (
          <Button size="sm" variant={insights ? "ghost" : "outline"} onClick={read} loading={busy === "read"}>
            {insights ? <RefreshCw className="size-3.5" /> : <Sparkles className="size-3.5" />}
            {insights ? "Read again" : `Read ${plural(data.replies, "reply", "replies")}`}
          </Button>
        )}
      </div>

      {writing && (
        <p className="mt-3 flex items-center gap-2 text-sm text-muted-foreground">
          <Loader2 className="size-3.5 animate-spin" /> Reading {plural(data.replies, "reply", "replies")}…
        </p>
      )}
      {latest?.status === "failed" && <p className="mt-3 text-sm text-danger">The agent couldn&apos;t read the replies. Try again.</p>}

      {insights && latest && (
        <div className="mt-4 flex flex-col gap-5">
          <p className="text-sm text-pretty">{insights.summary}</p>

          {(insights.working.length > 0 || insights.who.length > 0) && (
            <div className="grid gap-x-8 gap-y-4 sm:grid-cols-2">
              <Bullets title="What works" items={insights.working} />
              <Bullets title="Who answers" items={insights.who} />
            </div>
          )}

          {insights.objections.length > 0 && (
            <div>
              <MicroLabel className="mb-2">Objections</MicroLabel>
              <ol className="flex flex-col divide-y divide-border rounded-xl border border-border">
                {insights.objections.map((o, i) => (
                  <li key={o.theme} className="flex flex-col gap-2 p-3">
                    <div className="flex items-center gap-3">
                      <span className="min-w-0 flex-1 text-sm font-medium">{o.theme}</span>
                      <span className="h-1.5 w-20 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden>
                        <span
                          className="block h-full rounded-full bg-amber-500"
                          style={{ width: `${Math.min(100, (o.count / latest.replies) * 100)}%` }}
                        />
                      </span>
                      <span className="w-16 shrink-0 text-right text-xs text-muted-foreground tabular-nums">
                        {plural(o.count, "reply", "replies")}
                      </span>
                    </div>
                    {o.quote && (
                      <blockquote className="border-l-2 border-border pl-3 text-sm text-pretty text-muted-foreground italic">{o.quote}</blockquote>
                    )}
                    {o.answer && (
                      <div className="flex items-start gap-3">
                        <p className="min-w-0 flex-1 text-sm text-pretty text-foreground/80">
                          <span className="font-medium text-foreground">Answer: </span>
                          {o.answer}
                        </p>
                        <Button
                          size="sm"
                          variant="ghost"
                          onClick={() => save(i, o.theme, o.answer)}
                          loading={busy === `save-${i}`}
                          title="Add to saved replies"
                        >
                          <BookmarkPlus className="size-3.5" /> Save
                        </Button>
                      </div>
                    )}
                  </li>
                ))}
              </ol>
            </div>
          )}

          {insights.suggestions.length > 0 && (
            <div>
              <MicroLabel className="mb-2">Change the campaign</MicroLabel>
              <ul className="flex flex-col gap-3">
                {insights.suggestions.map((s, i) => {
                  const used = latest.applied.includes(i);
                  return (
                    <li key={i} className="rounded-xl bg-muted p-4">
                      <div className="flex items-start justify-between gap-3">
                        <p className="text-sm font-medium">{FIELDS[s.field] ?? s.field}</p>
                        <Button size="sm" variant={used ? "ghost" : "outline"} onClick={() => use(i)} loading={busy === `use-${i}`} disabled={used}>
                          {used ? <Check className="size-3.5" /> : <ArrowRight className="size-3.5" />}
                          {used ? "Used" : "Use this"}
                        </Button>
                      </div>
                      {s.current && <p className="mt-2 text-sm text-pretty text-muted-foreground line-through">{s.current}</p>}
                      <p className="mt-1 text-sm text-pretty">{s.value}</p>
                      <p className="mt-2 text-xs text-pretty text-muted-foreground">{s.why}</p>
                    </li>
                  );
                })}
              </ul>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

function Bullets({ title, items }: { title: string; items: string[] }) {
  if (!items.length) return null;
  return (
    <div>
      <MicroLabel className="mb-1">{title}</MicroLabel>
      <ul className="flex list-disc flex-col gap-1 pl-4 text-sm text-foreground/80">
        {items.map((item) => (
          <li key={item} className="text-pretty">
            {item}
          </li>
        ))}
      </ul>
    </div>
  );
}
