"use client";

import { useState } from "react";
import { HandHelping, Loader2, PenLine, RefreshCw } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, Handoff, HandoffContent } from "@/lib/types";
import { timeAgo } from "@/lib/utils";
import { OwnerMenu } from "./owner-menu";
import { toastError } from "./states";

/** In a won deal's window: what the agent wrote for whoever takes the customer on, and handing them over. See backend/src/handoffs.rs. */
export function HandoffSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/handoff`;
  const { mutate: mutateAll } = useSWRConfig();
  const { data: h, mutate } = useSWR<Handoff | null>(deal.status === "won" ? key : null, fetcher, {
    refreshInterval: (h) => (h?.status === "writing" ? 2500 : 0),
  });
  const [busy, setBusy] = useState<string | null>(null);
  const them = deal.company_name || deal.lead_name || "them";
  if (deal.status !== "won" || h === undefined) return null;

  const act = async (what: string, call: () => Promise<Handoff | null>, done?: string) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
      // The onboarding plan is written in the same run.
      mutateAll(`/deals/${deal.id}/plan`);
      if (done) toast.success(done);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => api<Handoff | null>(key, { method: "POST" }));
  const hand = (ownerId: string | null) => {
    if (!ownerId || ownerId === h?.owner_id) return;
    void act("hand", () => api<Handoff | null>(`${key}/owner`, { body: { owner_id: ownerId } }), "Handed over");
  };

  const writing = h?.status === "writing";
  const c = h?.handoff;
  if (!h || !c) {
    return (
      // The button wraps under the text on a phone rather than squeezing it into a narrow column.
      <section className="flex flex-wrap items-start justify-between gap-x-3 gap-y-2 rounded-xl border border-dashed border-border-strong p-4">
        <div className="min-w-0 flex-1 basis-56">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <HandHelping className="size-3.5" /> Handoff
          </p>
          <p className="mt-1 text-xs text-pretty text-muted-foreground">
            {writing
              ? `The agent is reading the whole sale and writing what whoever takes ${them} on needs to know…`
              : h?.status === "failed"
                ? "Couldn't write the handoff. Try again."
                : `What ${them} bought, what you promised, who's who and what success means to them, for whoever onboards them. It also turns the close plan into their onboarding plan.`}
          </p>
        </div>
        {writing ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
            <PenLine className="size-3.5" /> Write the handoff
          </Button>
        )}
      </section>
    );
  }

  return (
    <section className="rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center gap-2">
        <p className="flex items-center gap-1.5 text-sm font-medium">
          <HandHelping className="size-3.5" /> Handoff
        </p>
        <div className="ml-auto flex flex-wrap items-center justify-end gap-1">
          {writing ? (
            <span className="flex items-center gap-1.5 px-2 text-xs text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Writing…
            </span>
          ) : (
            <Button
              type="button"
              size="sm"
              variant="ghost"
              onClick={write}
              loading={busy === "write"}
              title="Keeps the onboarding steps already done"
            >
              <RefreshCw className="size-3.5" /> Write again
            </Button>
          )}
        </div>
      </div>
      <p className="mt-2 text-sm text-pretty">{c.summary}</p>
      <Body c={c} />
      <div className="mt-4 flex flex-wrap items-center gap-x-3 gap-y-2 border-t border-border pt-3">
        <span className="text-sm font-medium">{h.owner_id ? "Taken on by" : "Hand to"}</span>
        <OwnerMenu ownerId={h.owner_id} ownerName={h.owner_name} onPick={hand} disabled={busy === "hand"} className="w-48" />
        <span className="text-xs text-pretty text-muted-foreground">
          {h.owner_id && h.handed_at
            ? `${h.handed_by && h.handed_by !== h.owner_name ? `${h.handed_by} handed them over` : "Taken on"} ${timeAgo(h.handed_at)}.`
            : "They get the handoff by email, and your chats hear who has it."}
        </span>
      </div>
    </section>
  );
}

function Part({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div>
      <p className="text-xs font-medium text-muted-foreground">{title}</p>
      <div className="mt-1 text-sm">{children}</div>
    </div>
  );
}

function Body({ c }: { c: HandoffContent }) {
  return (
    <div className="mt-3 flex flex-col gap-3">
      {c.bought && (
        <Part title="What they bought">
          <p className="text-pretty">{c.bought}</p>
        </Part>
      )}
      {c.promises.length > 0 && (
        <Part title="What you promised">
          <ul className="flex flex-col gap-1.5">
            {c.promises.map((p, i) => (
              <li key={i} className="text-pretty">
                {p.promise}
                {p.quote && <q className="mt-0.5 block border-l-2 border-border-strong pl-2 text-xs text-muted-foreground">{p.quote}</q>}
              </li>
            ))}
          </ul>
        </Part>
      )}
      {c.people.length > 0 && (
        <Part title="Who's who">
          <ul className="flex flex-col gap-1">
            {c.people.map((p, i) => (
              <li key={i} className="text-pretty">
                <span className="font-medium">{p.name}</span>
                {p.role && <span className="text-muted-foreground">, {p.role}</span>}
                {p.note && <span className="text-muted-foreground">. {p.note}</span>}
              </li>
            ))}
          </ul>
        </Part>
      )}
      <div className="grid gap-3 sm:grid-cols-2">
        {c.success.length > 0 && (
          <Part title="Success to them">
            <ul className="list-disc pl-4">
              {c.success.map((s, i) => (
                <li key={i} className="text-pretty">
                  {s}
                </li>
              ))}
            </ul>
          </Part>
        )}
        {c.risks.length > 0 && (
          <Part title="Watch out for">
            <ul className="list-disc pl-4">
              {c.risks.map((s, i) => (
                <li key={i} className="text-pretty">
                  {s}
                </li>
              ))}
            </ul>
          </Part>
        )}
      </div>
    </div>
  );
}
