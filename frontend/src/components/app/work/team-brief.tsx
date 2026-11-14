"use client";

import { useState } from "react";
import { Loader2, MessagesSquare, RefreshCw, Users } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { TeamBriefView } from "@/lib/types";
import { timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

/** On the Targets tab for owners and admins: the agent's read of the team this week, each teammate in the order to talk to
 * them with their 1:1 prep a click away, and whether a new one is emailed every Monday. */
export function TeamBriefCard({ onPrep }: { onPrep: (userId: string) => void }) {
  const { data: v, mutate } = useSWR<TeamBriefView>("/team-brief", fetcher, {
    refreshInterval: (d) => (d?.latest?.status === "writing" ? 2000 : 0),
  });
  const [busy, setBusy] = useState(false);
  if (!v) return null;
  const latest = v.latest;
  const writing = latest?.status === "writing";
  const b = latest?.status === "ready" ? latest.brief : null;

  const write = async () => {
    setBusy(true);
    try {
      await mutate(api<TeamBriefView>("/team-brief", { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const weekly = async (on: boolean) => {
    try {
      await mutate(api<TeamBriefView>("/team-brief/weekly", { method: "PUT", body: { on } }), {
        optimisticData: { ...v, weekly: on },
        revalidate: false,
      });
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-64">
          <p className="flex items-center gap-2 text-sm font-semibold">
            <Users className="size-4 text-muted-foreground" /> This week&apos;s team brief
          </p>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            {writing
              ? "Reading everyone's week…"
              : b
                ? b.headline
                : latest?.error ||
                  "The agent reads each teammate's month, open deals, coached and practice calls, assigned practice and ramp, and writes who needs you this week."}
          </p>
        </div>
        <div className="flex items-center gap-3">
          <label className="flex items-center gap-2 text-xs text-muted-foreground">
            <Switch checked={v.weekly} onChange={weekly} label="Email it every Monday" />
            Every Monday
          </label>
          <Button type="button" size="sm" variant={b ? "outline" : "primary"} onClick={write} loading={busy} disabled={writing}>
            {writing ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}{" "}
            {latest ? "Write a new one" : "Write it now"}
          </Button>
        </div>
      </div>
      {b && (
        <>
          <ol className="divide-y divide-border border-t border-border">
            {b.people.map((p, i) => (
              <li key={p.user_id} className="flex flex-wrap items-start gap-x-3 gap-y-1 px-4 py-3 text-sm text-pretty">
                <span className="mt-0.5 w-4 text-xs text-muted-foreground tabular-nums">{i + 1}</span>
                <div className="min-w-0 flex-1 basis-64">
                  <p className="font-medium">{p.name}</p>
                  <p className="text-muted-foreground">{p.note}</p>
                  <p className="mt-1">
                    <span className="font-medium">Do:</span> {p.action}
                  </p>
                </div>
                <Button type="button" size="sm" variant="ghost" className="max-sm:ml-7" onClick={() => onPrep(p.user_id)}>
                  <MessagesSquare className="size-3.5" /> 1:1 prep
                </Button>
              </li>
            ))}
          </ol>
          {b.risks.length > 0 && (
            <div className="border-t border-border bg-muted/40 px-4 py-3 text-sm">
              <MicroLabel>Watch out for</MicroLabel>
              <ul className="mt-1 flex list-disc flex-col gap-1 pl-5 text-pretty">
                {b.risks.map((r) => (
                  <li key={r}>{r}</li>
                ))}
              </ul>
            </div>
          )}
          {latest && (
            <p className="border-t border-border px-4 py-2 text-xs text-muted-foreground">
              {latest.auto ? "Monday's brief" : "Written on request"} · {timeAgo(latest.created_at)}
            </p>
          )}
        </>
      )}
    </Card>
  );
}
