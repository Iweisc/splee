"use client";

import { useState } from "react";
import Link from "next/link";
import { AlertTriangle, Check, Loader2, ShieldCheck, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Badge, type BadgeTone, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ChurnRisk, ChurnRiskView, Deal } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { LoadError, MicroLabel, toastError } from "./states";
import { Kpi, ThreadDraft } from "./win-back";

const RISK: Record<NonNullable<ChurnRisk["risk"]>, { label: string; tone: BadgeTone }> = {
  high: { label: "High risk", tone: "red" },
  medium: { label: "Watch", tone: "amber" },
  low: { label: "Looks fine", tone: "green" },
};

const checked = (r: ChurnRisk) => r.status === "done";
const atRisk = (r: ChurnRisk) => checked(r) && (r.risk === "high" || r.risk === "medium");
const ready = (r: ChurnRisk) => atRisk(r) && r.email_status === "draft";
const emailed = (r: ChurnRisk) => atRisk(r) && ["approved", "scheduled", "sending", "sent"].includes(r.email_status ?? "");
const day = (iso: string) => new Date(iso).toLocaleDateString("en-US", { day: "numeric", month: "short", year: "numeric" });

/** Checking again, dropping one and sending the draft, each refreshing every view of it. */
function useActions(dealId: string, onChanged: () => unknown) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<string | null>(null);
  const act = async (what: string, call: () => Promise<unknown>, done?: string) => {
    setBusy(what);
    try {
      await call();
      if (done) toast.success(done);
      await onChanged();
      mutate((k) => typeof k === "string" && k.includes("/churn-risk"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  return {
    busy,
    check: () => act("write", () => api(`/deals/${dealId}/churn-risk`, { method: "POST" })),
    drop: (r: ChurnRisk) => act("drop", () => api(`/churn-risks/${r.id}`, { method: "PATCH", body: { dismissed: true } })),
    send: (r: ChurnRisk) =>
      act(
        "send",
        () => api(`/emails/${r.email_id}`, { method: "PATCH", body: { status: "approved" } }),
        "Approved. It goes out in the thread shortly.",
      ),
  };
}

type Actions = ReturnType<typeof useActions>;

/** The warning signs, what the agent made of them, and what to do. */
function Reasons({ r }: { r: ChurnRisk }) {
  return (
    <>
      {r.signal && (
        <p className="flex items-start gap-1.5 text-sm text-pretty text-amber-800">
          <AlertTriangle className="mt-0.5 size-3.5 shrink-0" /> {r.signal}
        </p>
      )}
      {r.status === "skipped" ? (
        <p className="text-sm text-pretty text-muted-foreground">Not checked: {r.skipped.replace(/\.$/, "")}.</p>
      ) : r.status === "failed" ? (
        <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>
      ) : r.status === "writing" ? (
        <p className="text-sm text-muted-foreground">The agent is reading the account and the warning signs…</p>
      ) : (
        <>
          {r.why && <p className="text-sm text-pretty text-muted-foreground">{r.why}</p>}
          {r.play && atRisk(r) && (
            <p className="text-sm text-pretty">
              <span className="font-medium">What to do:</span> {r.play}
            </p>
          )}
          {r.note && atRisk(r) && !ready(r) && <p className="text-xs text-pretty text-muted-foreground">No email: {r.note.replace(/\.$/, "")}.</p>}
        </>
      )}
    </>
  );
}

function RiskBadge({ r }: { r: ChurnRisk }) {
  if (!checked(r) || !r.risk) return null;
  return <Badge tone={RISK[r.risk].tone}>{RISK[r.risk].label}</Badge>;
}

function Draft({ r, actions, projectId }: { r: ChurnRisk; actions: Actions; projectId: string }) {
  return (
    <ThreadDraft
      subject={r.email_subject}
      body={r.email_body}
      projectId={projectId}
      onAnother={actions.check}
      onSend={() => actions.send(r)}
      busy={actions.busy}
    />
  );
}

/** In a won deal's window: the latest check on whether the customer might leave, or a button to check now. */
export function ChurnSection({ deal }: { deal: Deal }) {
  const {
    data: r,
    error,
    mutate,
  } = useSWR<ChurnRisk | null>(deal.status === "won" ? `/deals/${deal.id}/churn-risk` : null, fetcher, {
    refreshInterval: (r) => (r?.status === "writing" ? 2500 : 0),
  });
  const actions = useActions(deal.id, () => mutate());
  if (deal.status !== "won" || error || r === undefined) return null;

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
            <ShieldCheck className="size-3.5" /> Keep this customer {r && <RiskBadge r={r} />}
          </p>
          <div className="mt-1 flex flex-col gap-1">
            {r ? (
              <Reasons r={r} />
            ) : (
              <p className="text-sm text-pretty text-muted-foreground">
                The agent watches for warning signs: the buyer leaving, a refund, payments that stop, or a reply that reads as unhappy. Then it says
                how likely they are to leave and what to do. It can check now.
              </p>
            )}
          </div>
        </div>
        {r?.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          (!r || !ready(r)) && (
            <Button type="button" size="sm" variant="outline" onClick={actions.check} loading={actions.busy === "write"}>
              <ShieldCheck className="size-3.5" /> Check now
            </Button>
          )
        )}
      </div>
      {r && ready(r) && <Draft r={r} actions={actions} projectId={deal.project_id} />}
      {r && atRisk(r) && (
        <Button type="button" size="sm" variant="ghost" className="self-end" onClick={() => actions.drop(r)} loading={actions.busy === "drop"}>
          <X className="size-3.5" /> Handled
        </Button>
      )}
    </section>
  );
}

/** One customer on the At risk tab. */
function Row({ r, projectId, onOpen, onChanged }: { r: ChurnRisk; projectId: string; onOpen: (dealId: string) => void; onChanged: () => unknown }) {
  const actions = useActions(r.deal_id, onChanged);
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <span className="flex flex-wrap items-center gap-x-2">
            <button type="button" onClick={() => onOpen(r.deal_id)} className="text-left text-sm font-medium hover:underline">
              {r.company_name || r.lead_name}
            </button>
            <RiskBadge r={r} />
          </span>
          <p className="text-sm text-muted-foreground">
            {[
              r.company_name && r.lead_name,
              r.value_cents > 0 && dealMoney(r.value_cents / 100, r.currency.toUpperCase()),
              r.won_at && `customer since ${day(r.won_at)}`,
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
          <div className="mt-1 flex flex-col gap-0.5">
            <Reasons r={r} />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {r.status === "writing" ? (
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          ) : emailed(r) ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Check className="size-3.5" /> Emailed
            </span>
          ) : null}
          {atRisk(r) && (
            <Button type="button" size="sm" variant="ghost" onClick={() => actions.drop(r)} loading={actions.busy === "drop"} aria-label="Handled">
              <X className="size-3.5" />
            </Button>
          )}
        </div>
      </div>
      {ready(r) && <Draft r={r} actions={actions} projectId={projectId} />}
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
  items: ChurnRisk[];
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
        {items.map((r) => (
          <Row key={r.id} r={r} {...rest} />
        ))}
      </ul>
    </Card>
  );
}

/** The pipeline's At risk tab: customers showing warning signs, what to do about each, and the ones that turned out fine. */
export function ChurnPanel({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<ChurnRiskView>(`/projects/${projectId}/churn-risks`, fetcher, {
    refreshInterval: (v) => (v?.items.some((r) => r.status === "writing") ? 4000 : 0),
  });
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the customers at risk" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;

  const items = data.items;
  const high = items.filter((r) => atRisk(r) && r.risk === "high");
  const watch = items.filter((r) => atRisk(r) && r.risk === "medium");
  const writing = items.filter((r) => r.status === "writing");
  const fine = items.filter((r) => checked(r) && r.risk === "low");
  const unchecked = items.filter((r) => r.status === "skipped" || r.status === "failed");
  const shared = { projectId, onOpen, onChanged: () => mutate() };

  return (
    <div className="flex flex-col gap-4">
      {!data.enabled && (
        <p className="rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
          Watching customers is off, so the agent isn&apos;t looking for warning signs. Turn it on on the{" "}
          <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
            Autopilot page
          </Link>
          .
        </p>
      )}
      {!items.length ? (
        <Card>
          <EmptyState icon={<ShieldCheck className="size-5" />} title="No warning signs at your customers">
            The agent watches every customer for the person who bought leaving, a refund, payments that stop coming, and replies that read as unhappy.
            When it sees one, it says how likely they are to leave and what to do, and drafts the email when one would help.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Kpi label="High risk" value={high.length} hint="customers likely to leave" />
            <Kpi label="Watch" value={watch.length} hint="warning signs to act on" />
            <Kpi label="Emails ready" value={items.filter(ready).length} hint="drafts waiting for you" />
            <Kpi label="No warning signs" value={Math.max(data.customers - high.length - watch.length, 0)} hint="of your customers" />
          </div>
          <Group title="High risk" hint="These customers look likely to leave. Start with these this week." items={high} {...shared} />
          <Group title="Watch" hint="A warning sign worth acting on before it grows." items={watch} {...shared} />
          <Group title="Checking" hint="The agent is reading these accounts now." items={writing} {...shared} />
          <Group title="Looks fine" hint="The agent read the warning signs and found nothing to worry about." items={fine} {...shared} />
          <Group title="Not checked" hint="Why the agent didn't read these. Open a deal to check now." items={unchecked} {...shared} />
        </>
      )}
    </div>
  );
}
