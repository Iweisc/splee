"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Loader2, Megaphone, RefreshCw, Users } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { BuyerTrait, BuyersView, Segment } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { LoadError, MicroLabel, toastError } from "./states";

/** On the Win/loss tab: what the companies that buy have in common, and a campaign to find more. See backend/src/agent/buyers.rs. */
export function BuyersCard({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/buyers`;
  const router = useRouter();
  const { data, error, mutate } = useSWR<BuyersView>(key, fetcher, { refreshInterval: (v) => (v?.latest?.status === "writing" ? 3000 : 0) });
  const [busy, setBusy] = useState<"read" | "campaign" | null>(null);
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} />
      </Card>
    );
  }
  if (!data) return null;

  const read = async () => {
    setBusy("read");
    try {
      await mutate(api<BuyersView>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const latest = data.latest;
  const writing = latest?.status === "writing";
  const p = latest?.status === "ready" ? latest.profile : null;

  const createCampaign = async () => {
    if (!p) return;
    setBusy("campaign");
    try {
      const segment = await api<Segment>(`/projects/${projectId}/segments`, { body: { prompt: p.campaign } });
      router.push(`/app/p/${projectId}/segments/${segment.id}`);
    } catch (e) {
      toastError(e);
      setBusy(null);
    }
  };

  const enough = data.won >= data.min_won;
  return (
    <Card className="p-4">
      <div className="flex items-start gap-3">
        <Users className={cn("mt-0.5 size-4 shrink-0", p ? "text-brand-700" : "text-muted-foreground")} />
        <div className="min-w-0 flex-1">
          <p className="text-sm font-medium">Who buys from you</p>
          {p ? (
            <p className="mt-0.5 text-sm text-pretty text-foreground/80">{p.summary}</p>
          ) : (
            <p className="mt-0.5 text-sm text-muted-foreground">
              {writing
                ? "The agent is comparing your won deals with your lost ones…"
                : latest?.status === "failed"
                  ? "The agent couldn't finish comparing your deals. Try again."
                  : enough
                    ? "The agent compares your won deals with your lost ones, says what the buyers have in common and drafts a campaign to find more like them."
                    : `Once ${data.min_won} deals are won, the agent can say what your buyers have in common. ${plural(data.won, "deal")} won so far.`}
            </p>
          )}
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          enough && (
            <Button type="button" size="sm" variant="ghost" onClick={read} loading={busy === "read"}>
              <RefreshCw className="size-3.5" /> {latest ? "Read again" : "Find out"}
            </Button>
          )
        )}
      </div>
      {p && latest && (
        <div className="mt-4 grid gap-4 pl-7 lg:grid-cols-2">
          <Traits title="What the buyers share" traits={p.traits} total={latest.won} what="won deal" bar="bg-brand-500" />
          {p.avoid.length > 0 && <Traits title="What the ones you lose share" traits={p.avoid} what="lost deal" bar="bg-red-500" />}
          {p.campaign && (
            <div className="rounded-xl bg-muted/60 p-3 lg:col-span-2">
              <MicroLabel className="mb-1">Find more like them</MicroLabel>
              <p className="text-sm text-pretty">{p.campaign}</p>
              <div className="mt-2 flex items-center justify-between gap-3">
                <p className="text-xs text-muted-foreground">
                  From {plural(latest.won, "won deal")}, {timeAgo(latest.created_at)}
                </p>
                <Button type="button" size="sm" onClick={createCampaign} loading={busy === "campaign"}>
                  <Megaphone className="size-3.5" /> Create this campaign
                </Button>
              </div>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

function Traits({ title, traits, total, what, bar }: { title: string; traits: BuyerTrait[]; total?: number; what: string; bar: string }) {
  const most = Math.max(total ?? 0, ...traits.map((t) => t.count));
  return (
    <div>
      <MicroLabel className="mb-1">{title}</MicroLabel>
      <ul className="flex flex-col divide-y divide-border">
        {traits.map((t) => (
          <li key={t.trait} className="py-2">
            <div className="flex items-center gap-3">
              <span className="min-w-0 flex-1 text-sm font-medium">{t.trait}</span>
              <span className="h-1.5 w-16 shrink-0 overflow-hidden rounded-full bg-muted" aria-hidden>
                <span className={cn("block h-full rounded-full", bar)} style={{ width: `${(t.count / most) * 100}%` }} />
              </span>
              <span className="w-20 shrink-0 text-right text-xs text-muted-foreground tabular-nums">{plural(t.count, what)}</span>
            </div>
            {t.why && <p className="mt-0.5 text-xs text-pretty text-muted-foreground">{t.why}</p>}
          </li>
        ))}
      </ul>
    </div>
  );
}
