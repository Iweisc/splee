"use client";

import { useRef, useState } from "react";
import { Loader2, RefreshCw, UserSearch, Users } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { Avatar, Badge, type BadgeTone } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { BuyingCommitteeView, CommitteeRole } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";
import { LEAD_STATUS } from "./status";

const ROLES: Record<CommitteeRole, { label: string; tone: BadgeTone }> = {
  decision_maker: { label: "Decides", tone: "dark" },
  champion: { label: "Champion", tone: "green" },
  influencer: { label: "Has a say", tone: "blue" },
  user: { label: "Would use it", tone: "neutral" },
  unknown: { label: "Not sure", tone: "neutral" },
};

/** Everyone at the lead's company, their part in buying, and the agent finding who's missing. See agent/committee.rs. */
export function BuyingCommitteeCard({
  leadId,
  company,
  campaign,
  openLead,
}: {
  leadId: string;
  company: string;
  campaign: string | null;
  openLead: (id: string) => void;
}) {
  const key = `/leads/${leadId}/committee`;
  const { mutate: mutateAll } = useSWRConfig();
  const wasWorking = useRef(false);
  const { data, mutate } = useSWR<BuyingCommitteeView>(key, fetcher, {
    refreshInterval: (v) => (v?.latest?.status === "working" ? 3000 : 0),
    // The people it found are leads now, so the lists showing leads refresh once when it's done.
    onSuccess: (v) => {
      const working = v.latest?.status === "working";
      if (wasWorking.current && !working) mutateAll((k) => typeof k === "string" && k.includes("/leads") && k !== key);
      wasWorking.current = working;
    },
  });
  const [asking, setAsking] = useState(false);
  if (!data) return null;

  const ask = async () => {
    setAsking(true);
    try {
      const started = await api<BuyingCommitteeView>(key, { method: "POST" });
      wasWorking.current = true;
      await mutate(started, { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setAsking(false);
    }
  };
  const latest = data.latest;
  const working = latest?.status === "working";
  const committee = latest?.committee ?? null;
  const roles = new Map(committee?.members.map((m) => [m.lead_id, m]));

  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <MicroLabel className="flex items-center gap-1.5">
            <Users className="size-3.5" /> Buying committee at {company}
          </MicroLabel>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {working
              ? `Mapping who buys at ${company} and looking for who's missing…`
              : committee && latest
                ? `${committee.summary} Mapped ${timeAgo(latest.created_at)}.`
                : `${plural(data.people.length, "person", "people")} here so far. The agent can say who decides and find who's missing.`}
          </p>
        </div>
        {working ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          campaign && (
            <Button size="sm" variant={committee ? "ghost" : "outline"} onClick={ask} loading={asking}>
              {committee ? <RefreshCw className="size-3.5" /> : <UserSearch className="size-3.5" />}
              {committee ? "Look again" : "Find the rest"}
            </Button>
          )
        )}
      </div>

      <ul className="mt-3 flex flex-col divide-y divide-border">
        {data.people.map((p) => {
          const member = roles.get(p.id);
          const found = latest && !working && p.created_at > latest.created_at;
          return (
            <li key={p.id} className="flex items-center gap-3 py-2">
              <Avatar name={p.full_name} className="size-7" />
              <div className="min-w-0 flex-1">
                <p className="flex flex-wrap items-center gap-x-2 text-sm">
                  {p.id === leadId ? (
                    <span className="font-medium">{p.full_name}</span>
                  ) : (
                    <button onClick={() => openLead(p.id)} className="font-medium hover:underline">
                      {p.full_name}
                    </button>
                  )}
                  <span className="text-xs text-muted-foreground">{LEAD_STATUS[p.status].label}</span>
                  {found && <Badge tone="amber">Just found</Badge>}
                </p>
                <p className="truncate text-xs text-muted-foreground">{[p.title, member?.note].filter(Boolean).join(" · ")}</p>
              </div>
              {member && <Badge tone={ROLES[member.role].tone}>{ROLES[member.role].label}</Badge>}
            </li>
          );
        })}
      </ul>

      {committee && committee.missing.length > 0 && latest && !working && (
        <div className="mt-3 rounded-lg bg-muted p-3">
          <p className="text-xs font-medium">
            {latest.found > 0
              ? `Looked for these roles and added ${plural(latest.found, "person", "people")} to ${campaign ?? "the campaign"}:`
              : "Looked for these roles and found nobody new:"}
          </p>
          <ul className="mt-1.5 flex flex-col gap-1">
            {committee.missing.map((m) => (
              <li key={m.role} className="text-xs text-pretty text-muted-foreground">
                <span className="font-medium text-foreground">{m.role}.</span> {m.why}
              </li>
            ))}
          </ul>
        </div>
      )}
      {latest?.status === "failed" && <p className="mt-3 text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>}
    </section>
  );
}
