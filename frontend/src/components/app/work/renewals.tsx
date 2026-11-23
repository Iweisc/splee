"use client";

import Link from "next/link";
import { CalendarClock, Check, Loader2, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { Badge, type BadgeTone, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, Renewal, RenewalView } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { LoadError, MicroLabel, useDealActions } from "./states";
import { Kpi, ThreadDraft } from "./win-back";

const RISK: Record<NonNullable<Renewal["risk"]>, { label: string; tone: BadgeTone }> = {
  high: { label: "At risk", tone: "red" },
  medium: { label: "Fix first", tone: "amber" },
  low: { label: "Set to renew", tone: "green" },
};

const date = (d: string) => new Date(`${d}T00:00:00`);
const daysTo = (d: string) => Math.round((date(d).getTime() - new Date(new Date().toDateString()).getTime()) / 86_400_000);
const longDay = (d: string) => date(d).toLocaleDateString("en-US", { day: "numeric", month: "short", year: "numeric" });
const month = (d: string) => date(d).toLocaleDateString("en-US", { month: "long", year: "numeric" });
const ready = (r: Renewal) => r.status === "done" && r.email_status === "draft";
const emailed = (r: Renewal) => r.status === "done" && ["approved", "scheduled", "sending", "sent"].includes(r.email_status ?? "");
const inDays = (n: number) => (n === 0 ? "today" : n === 1 ? "tomorrow" : `in ${n} days`);

/** Prepping now, setting the date and sending the draft, each refreshing every view of it. */
function useActions(dealId: string, onChanged: () => unknown) {
  const { busy, act, send } = useDealActions(onChanged, (k) => k.includes("/renewal") || k.endsWith("/customers"));
  return {
    busy,
    prep: () => act("write", () => api(`/deals/${dealId}/renewal`, { method: "POST" })),
    setDate: (renews_on: string | null) => act("date", () => api(`/deals/${dealId}/renewal`, { method: "PUT", body: { renews_on } })),
    send: (r: Renewal) => send(r.email_id),
  };
}

type Actions = ReturnType<typeof useActions>;

/** What the agent made of the account: why, the steps to a signed renewal, and why there's no email when there isn't one. */
function Prep({ r }: { r: Renewal }) {
  if (r.status === "later") return null;
  if (r.status === "writing") return <p className="text-sm text-muted-foreground">The agent is reading the account…</p>;
  if (r.status === "failed") return <p className="text-sm text-danger">The agent couldn&apos;t finish. Prep it again.</p>;
  if (r.status === "skipped") return <p className="text-sm text-pretty text-muted-foreground">Not prepped: {r.skipped.replace(/\.$/, "")}.</p>;
  return (
    <>
      {r.why && <p className="text-sm text-pretty text-muted-foreground">{r.why}</p>}
      {r.steps.length > 0 && (
        <ol className="mt-1 flex list-decimal flex-col gap-0.5 pl-5 text-sm text-pretty">
          {r.steps.map((s) => (
            <li key={s}>{s}</li>
          ))}
        </ol>
      )}
      {r.note && !ready(r) && !emailed(r) && <p className="text-xs text-pretty text-muted-foreground">No email: {r.note.replace(/\.$/, "")}.</p>}
    </>
  );
}

function RiskBadge({ r }: { r: Renewal }) {
  if (r.status !== "done" || !r.risk) return null;
  return <Badge tone={RISK[r.risk].tone}>{RISK[r.risk].label}</Badge>;
}

function Draft({ r, actions, projectId }: { r: Renewal; actions: Actions; projectId: string }) {
  return (
    <ThreadDraft
      subject={r.email_subject}
      body={r.email_body}
      projectId={projectId}
      onAnother={actions.prep}
      onSend={() => actions.send(r)}
      busy={actions.busy}
    />
  );
}

/** In a won deal's window: when they renew, which the team can change, and the agent's prep for it. */
export function RenewalSection({ deal }: { deal: Deal }) {
  const key = deal.status === "won" ? `/deals/${deal.id}/renewal` : null;
  const { data: r, error, mutate } = useSWR<Renewal | null>(key, fetcher, { refreshInterval: (r) => (r?.status === "writing" ? 2500 : 0) });
  const actions = useActions(deal.id, () => mutate());
  if (deal.status !== "won" || error || !r?.renews_on) return null;
  const days = daysTo(r.renews_on);

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex flex-wrap items-center gap-1.5 text-sm font-medium">
            <CalendarClock className="size-3.5" /> Renews {inDays(days)} <RiskBadge r={r} />
          </p>
          <div className="mt-2 flex flex-wrap items-center gap-2 text-sm">
            <label htmlFor={`renews-${deal.id}`} className="text-muted-foreground">
              Renews on
            </label>
            <Input
              id={`renews-${deal.id}`}
              type="date"
              defaultValue={r.renews_on}
              key={r.renews_on}
              onBlur={(e) => e.target.value && e.target.value !== r.renews_on && actions.setDate(e.target.value)}
              className="h-8 w-auto"
            />
            {r.assumed ? (
              <span className="text-xs text-muted-foreground">a year after the win</span>
            ) : (
              <Button type="button" size="sm" variant="ghost" onClick={() => actions.setDate(null)} loading={actions.busy === "date"}>
                Use the win&apos;s anniversary
              </Button>
            )}
          </div>
          <div className="mt-2 flex flex-col gap-1">
            {r.status === "later" ? (
              <p className="text-sm text-pretty text-muted-foreground">
                The agent preps the renewal 90 days ahead: how likely they are to renew, the steps to get there and an email that opens the
                conversation. It can prep it now.
              </p>
            ) : (
              <Prep r={r} />
            )}
          </div>
        </div>
        {r.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          !ready(r) && (
            <Button type="button" size="sm" variant="outline" onClick={actions.prep} loading={actions.busy === "write"}>
              <RefreshCw className="size-3.5" /> {r.status === "later" ? "Prep it now" : "Prep it again"}
            </Button>
          )
        )}
      </div>
      {ready(r) && <Draft r={r} actions={actions} projectId={deal.project_id} />}
    </section>
  );
}

/** One customer on the Renewals tab. */
function Row({ r, projectId, onOpen, onChanged }: { r: Renewal; projectId: string; onOpen: (dealId: string) => void; onChanged: () => unknown }) {
  const actions = useActions(r.deal_id, onChanged);
  const on = r.renews_on!;
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="w-16 shrink-0 text-center">
          <p className="text-lg leading-tight font-semibold tabular-nums">{date(on).getDate()}</p>
          <p className="text-xs text-muted-foreground">{inDays(daysTo(on))}</p>
        </div>
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
              r.owner_name,
              r.assumed ? `${longDay(on)}, a year after the win` : longDay(on),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
          <div className="mt-1 flex flex-col gap-0.5">
            <Prep r={r} />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {r.status === "writing" ? (
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          ) : emailed(r) ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Check className="size-3.5" /> Emailed
            </span>
          ) : r.status === "later" || r.status === "failed" ? (
            <Button type="button" size="sm" variant="outline" onClick={actions.prep} loading={actions.busy === "write"}>
              Prep now
            </Button>
          ) : null}
        </div>
      </div>
      {ready(r) && <Draft r={r} actions={actions} projectId={projectId} />}
    </li>
  );
}

/** The Customers page's Renewals tab: customers renewing in the next six months by month, with the agent's prep for each. */
export function RenewalsPanel({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<RenewalView>(`/projects/${projectId}/renewals`, fetcher, {
    refreshInterval: (v) => (v?.items.some((r) => r.status === "writing") ? 4000 : 0),
  });
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the renewals" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;

  const items = data.items.filter((r) => r.renews_on);
  const soon = items.filter((r) => daysTo(r.renews_on!) <= data.ahead_days);
  const months = [...new Set(items.map((r) => month(r.renews_on!)))];
  const shared = { projectId, onOpen, onChanged: () => mutate() };

  return (
    <div className="flex flex-col gap-4">
      {!data.enabled && (
        <p className="rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
          Prepping renewals is off, so the agent doesn&apos;t prep them on its own. Turn it on on the{" "}
          <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
            Autopilot page
          </Link>
          , or prep one now.
        </p>
      )}
      {!items.length ? (
        <Card>
          <EmptyState icon={<CalendarClock className="size-5" />} title="No renewals in the next six months">
            A customer renews a year after you won them, or on the date you set in their deal. {data.ahead_days} days before, the agent preps it: how
            likely they are to renew, the steps to get there and an email that opens the conversation.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Kpi label={`Next ${data.ahead_days} days`} value={soon.length} hint="customers renewing" />
            <Kpi label="At risk or fix first" value={items.filter((r) => r.risk === "high" || r.risk === "medium").length} hint="of those prepped" />
            <Kpi label="Emails ready" value={items.filter(ready).length} hint="drafts waiting for you" />
            <Kpi label="Next six months" value={items.length} hint="customers renewing" />
          </div>
          {months.map((m) => (
            <Card key={m} className="p-4">
              <MicroLabel>{m}</MicroLabel>
              <ul className="mt-1 flex flex-col divide-y divide-border">
                {items
                  .filter((r) => month(r.renews_on!) === m)
                  .map((r) => (
                    <Row key={r.deal_id} r={r} {...shared} />
                  ))}
              </ul>
            </Card>
          ))}
        </>
      )}
    </div>
  );
}
