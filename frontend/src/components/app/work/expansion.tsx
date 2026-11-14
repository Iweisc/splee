"use client";

import { useState } from "react";
import Link from "next/link";
import { Check, Loader2, Sparkles, TrendingUp, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, Expansion, ExpansionView } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { LoadError, MicroLabel, toastError } from "./states";
import { Kpi, ThreadDraft } from "./win-back";

const ready = (x: Expansion) => x.status === "drafted";
const day = (iso: string) => new Date(iso).toLocaleDateString("en-US", { day: "numeric", month: "short", year: "numeric" });

/** Looking again, dropping one and sending the draft, each refreshing every view of it. */
function useActions(dealId: string, onChanged: () => unknown) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<string | null>(null);
  const act = async (what: string, call: () => Promise<unknown>, done?: string) => {
    setBusy(what);
    try {
      await call();
      if (done) toast.success(done);
      await onChanged();
      mutate((k) => typeof k === "string" && k.includes("/expansion"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  return {
    busy,
    look: () => act("write", () => api(`/deals/${dealId}/expansion`, { method: "POST" })),
    drop: (x: Expansion) => act("drop", () => api(`/expansions/${x.id}`, { method: "PATCH", body: { dismissed: true } })),
    send: (x: Expansion) =>
      act(
        "send",
        () => api(`/emails/${x.email_id}`, { method: "PATCH", body: { status: "approved" } }),
        "Approved. It goes out in the thread shortly.",
      ),
  };
}

type Actions = ReturnType<typeof useActions>;

/** What changed at their company and what the agent made of it. */
function Reasons({ x }: { x: Expansion }) {
  return (
    <>
      {x.signal && (
        <p className="flex items-start gap-1.5 text-sm text-pretty text-brand-700">
          <Sparkles className="mt-0.5 size-3.5 shrink-0" /> {x.signal}
        </p>
      )}
      {x.status === "skipped" ? (
        <p className="text-sm text-pretty text-muted-foreground">No opening: {x.skipped.replace(/\.$/, "")}.</p>
      ) : x.status === "failed" ? (
        <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>
      ) : x.status === "writing" ? (
        <p className="text-sm text-muted-foreground">The agent is reading the account and what changed…</p>
      ) : (
        x.why && <p className="text-sm text-pretty text-muted-foreground">{x.why}</p>
      )}
    </>
  );
}

function Draft({ x, actions, projectId }: { x: Expansion; actions: Actions; projectId: string }) {
  return (
    <ThreadDraft
      subject={x.email_subject}
      body={x.email_body}
      projectId={projectId}
      onAnother={actions.look}
      onSend={() => actions.send(x)}
      busy={actions.busy}
    />
  );
}

/** In a won deal's window: the latest look for room to grow the account, or a button to look now. */
export function ExpansionSection({ deal }: { deal: Deal }) {
  const {
    data: x,
    error,
    mutate,
  } = useSWR<Expansion | null>(deal.status === "won" ? `/deals/${deal.id}/expansion` : null, fetcher, {
    refreshInterval: (x) => (x?.status === "writing" ? 2500 : 0),
  });
  const actions = useActions(deal.id, () => mutate());
  if (deal.status !== "won" || error || x === undefined) return null;

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <TrendingUp className="size-3.5" /> {x && ready(x) ? "Room to grow" : x?.status === "sent" ? "Expansion email sent" : "Grow this account"}
          </p>
          <div className="mt-1 flex flex-col gap-1">
            {x ? (
              <Reasons x={x} />
            ) : (
              <p className="text-sm text-pretty text-muted-foreground">
                A month after the win, the agent watches their company for news, new job openings and visits to your pricing, and drafts an email when
                there&apos;s room to sell more. It can look now.
              </p>
            )}
          </div>
        </div>
        {x?.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          (!x || !ready(x)) && (
            <Button type="button" size="sm" variant="outline" onClick={actions.look} loading={actions.busy === "write"}>
              <TrendingUp className="size-3.5" /> Look now
            </Button>
          )
        )}
      </div>
      {x && ready(x) && (
        <>
          <Draft x={x} actions={actions} projectId={deal.project_id} />
          <Button type="button" size="sm" variant="ghost" className="self-end" onClick={() => actions.drop(x)} loading={actions.busy === "drop"}>
            <X className="size-3.5" /> Not now
          </Button>
        </>
      )}
    </section>
  );
}

/** One look on the Expansion tab. */
function Row({ x, projectId, onOpen, onChanged }: { x: Expansion; projectId: string; onOpen: (dealId: string) => void; onChanged: () => unknown }) {
  const actions = useActions(x.deal_id, onChanged);
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <button type="button" onClick={() => onOpen(x.deal_id)} className="text-left text-sm font-medium hover:underline">
            {x.company_name || x.lead_name}
          </button>
          <span className="text-sm text-muted-foreground">
            {x.company_name && ` · ${x.lead_name}`}
            {x.value_cents > 0 && ` · ${dealMoney(x.value_cents / 100, x.currency.toUpperCase())}`}
            {x.won_at && ` · customer since ${day(x.won_at)}`}
          </span>
          <div className="mt-1 flex flex-col gap-0.5">
            <Reasons x={x} />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {x.status === "writing" ? (
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          ) : x.status === "sent" ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Check className="size-3.5" /> Sent
            </span>
          ) : (
            ready(x) && (
              <Button type="button" size="sm" variant="ghost" onClick={() => actions.drop(x)} loading={actions.busy === "drop"} aria-label="Not now">
                <X className="size-3.5" />
              </Button>
            )
          )}
        </div>
      </div>
      {ready(x) && <Draft x={x} actions={actions} projectId={projectId} />}
    </li>
  );
}

function Group({
  title,
  hint,
  items,
  ...rest
}: {
  title: string;
  hint: string;
  items: Expansion[];
  projectId: string;
  onOpen: (dealId: string) => void;
  onChanged: () => unknown;
}) {
  if (!items.length) return null;
  return (
    <Card className="p-4">
      <MicroLabel>{title}</MicroLabel>
      <p className="text-xs text-muted-foreground">{hint}</p>
      <ul className="mt-1 flex flex-col divide-y divide-border">
        {items.map((x) => (
          <Row key={x.id} x={x} {...rest} />
        ))}
      </ul>
    </Card>
  );
}

/** The Customers page's Expansion tab: customers whose company changed, the drafts to send, and what the agent passed on. */
export function ExpansionPanel({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<ExpansionView>(`/projects/${projectId}/expansions`, fetcher, {
    refreshInterval: (v) => (v?.items.some((x) => x.status === "writing") ? 4000 : 0),
  });
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the expansions" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;

  const items = data.items;
  const drafts = items.filter(ready);
  const sent = items.filter((x) => x.status === "sent");
  const writing = items.filter((x) => x.status === "writing");
  const passed = items.filter((x) => x.status === "skipped" || x.status === "failed");
  const shared = { projectId, onOpen, onChanged: () => mutate() };

  return (
    <div className="flex flex-col gap-4">
      {!data.enabled && (
        <p className="rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
          Growing customers is off, so the agent isn&apos;t watching their companies. Turn it on on the{" "}
          <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
            Autopilot page
          </Link>
          .
        </p>
      )}
      {!items.length ? (
        <Card>
          <EmptyState icon={<TrendingUp className="size-5" />} title="No openings to grow a customer yet">
            A month after a deal is won, the agent watches the customer&apos;s company: news, new job openings, and someone there reading your
            pricing. When one shows room to sell more, it drafts the email to the person who bought.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-3 gap-3">
            <Kpi label="Ready to send" value={drafts.length} hint="drafts waiting for you" />
            <Kpi label="Sent" value={sent.length} hint="expansion emails approved" />
            <Kpi label="No opening" value={passed.length} hint="changes the agent passed on" />
          </div>
          <Group
            title="Ready to send"
            hint="Something changed at these customers. Approve one and it goes out in the thread."
            items={drafts}
            {...shared}
          />
          <Group title="Reading" hint="The agent is looking at these accounts now." items={writing} {...shared} />
          <Group title="Sent" hint="Expansion emails that went out, or are about to." items={sent} {...shared} />
          <Group title="No opening" hint="What changed and why the agent didn't write. Open a deal to look again." items={passed} {...shared} />
        </>
      )}
    </div>
  );
}
