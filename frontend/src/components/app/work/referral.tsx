"use client";

import { useState } from "react";
import Link from "next/link";
import { Check, Copy, Eye, HeartHandshake, Loader2, PenLine, UserPlus, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, Referral, ReferralView } from "@/lib/types";
import { plural } from "@/lib/utils";
import { LoadError, MicroLabel, toastError } from "./states";
import { Kpi, ThreadDraft } from "./win-back";

const ready = (r: Referral) => r.status === "drafted";
const pageUrl = (r: Referral) => `${window.location.origin}/r/${r.token}`;

/** Writing the ask now, dropping it and sending the draft, each refreshing every view of it. */
function useActions(dealId: string, onChanged: () => unknown) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<string | null>(null);
  const act = async (what: string, call: () => Promise<unknown>, done?: string) => {
    setBusy(what);
    try {
      await call();
      if (done) toast.success(done);
      await onChanged();
      mutate((k) => typeof k === "string" && k.includes("/referral"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  return {
    busy,
    write: () => act("write", () => api(`/deals/${dealId}/referral`, { method: "POST" })),
    drop: (r: Referral) => act("drop", () => api(`/referrals/${r.id}`, { method: "PATCH", body: { dismissed: true } })),
    send: (r: Referral) =>
      act(
        "send",
        () => api(`/emails/${r.email_id}`, { method: "PATCH", body: { status: "approved" } }),
        "Approved. It goes out in the thread shortly.",
      ),
  };
}

type Actions = ReturnType<typeof useActions>;

function copy(r: Referral) {
  navigator.clipboard.writeText(pageUrl(r)).then(
    () => toast.success("Copied the page's link"),
    () => toast.error("Couldn't copy the link"),
  );
}

/** Why the agent asked or didn't, the companies it named, and how the page is doing. */
function Details({ r }: { r: Referral }) {
  return (
    <>
      {r.status === "skipped" ? (
        <p className="text-sm text-pretty text-muted-foreground">Not now: {r.skipped.replace(/\.$/, "")}.</p>
      ) : r.status === "failed" ? (
        <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>
      ) : r.status === "writing" ? (
        <p className="text-sm text-muted-foreground">The agent is reading how they feel about you and who they might know…</p>
      ) : (
        r.why && <p className="text-sm text-pretty text-muted-foreground">{r.why}</p>
      )}
      {r.companies.length > 0 && r.status !== "skipped" && (
        <ul className="mt-1 flex flex-wrap gap-1.5">
          {r.companies.map((c) => (
            <li key={c.domain} title={c.why} className="flex items-center gap-1.5 rounded-full border border-border px-2 py-0.5 text-xs">
              <Favicon domain={c.domain} size={12} /> {c.name}
            </li>
          ))}
        </ul>
      )}
      {(r.views > 0 || r.referred > 0) && (
        <p className="mt-1 flex flex-wrap items-center gap-x-3 text-xs text-muted-foreground">
          <span className="flex items-center gap-1">
            <Eye className="size-3.5" /> Page opened {plural(r.views, "time")}
          </span>
          {r.referred > 0 && (
            <span className="flex items-center gap-1 font-medium text-brand-700">
              <UserPlus className="size-3.5" /> {plural(r.referred, "lead")} referred
            </span>
          )}
        </p>
      )}
    </>
  );
}

function Draft({ r, actions, projectId }: { r: Referral; actions: Actions; projectId: string }) {
  return (
    <ThreadDraft
      subject={r.email_subject}
      body={r.email_body}
      projectId={projectId}
      onAnother={actions.write}
      onSend={() => actions.send(r)}
      busy={actions.busy}
    />
  );
}

/** In a won deal's window: the ask for introductions, its draft, and its page. */
export function ReferralSection({ deal }: { deal: Deal }) {
  const {
    data: r,
    error,
    mutate,
  } = useSWR<Referral | null>(deal.status === "won" ? `/deals/${deal.id}/referral` : null, fetcher, {
    refreshInterval: (r) => (r?.status === "writing" ? 2500 : 0),
  });
  const actions = useActions(deal.id, () => mutate());
  if (deal.status !== "won" || error || r === undefined) return null;

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <HeartHandshake className="size-3.5" />{" "}
            {r && ready(r) ? "Ready to ask for introductions" : r?.status === "sent" ? "Asked for introductions" : "Ask for introductions"}
          </p>
          <div className="mt-1 flex flex-col gap-1">
            {r ? (
              <Details r={r} />
            ) : (
              <p className="text-sm text-pretty text-muted-foreground">
                Six weeks after the win, the agent reads whether they&apos;re happy and drafts an email asking whether they know someone at companies
                like theirs. It can write it now.
              </p>
            )}
          </div>
        </div>
        {r?.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          (!r || !ready(r)) && (
            <Button type="button" size="sm" variant="outline" onClick={actions.write} loading={actions.busy === "write"}>
              <PenLine className="size-3.5" /> {r ? "Write again" : "Write it now"}
            </Button>
          )
        )}
      </div>
      {r && ready(r) && <Draft r={r} actions={actions} projectId={deal.project_id} />}
      {r && (ready(r) || r.status === "sent") && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Button type="button" size="sm" variant="ghost" onClick={() => copy(r)}>
            <Copy className="size-3.5" /> Copy their page&apos;s link
          </Button>
          {ready(r) && (
            <Button type="button" size="sm" variant="ghost" onClick={() => actions.drop(r)} loading={actions.busy === "drop"}>
              <X className="size-3.5" /> Not now
            </Button>
          )}
        </div>
      )}
    </section>
  );
}

/** One customer on the Referrals tab. */
function Row({ r, projectId, onOpen, onChanged }: { r: Referral; projectId: string; onOpen: (dealId: string) => void; onChanged: () => unknown }) {
  const actions = useActions(r.deal_id, onChanged);
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <div className="min-w-0 flex-1">
          <button type="button" onClick={() => onOpen(r.deal_id)} className="text-left text-sm font-medium hover:underline">
            {r.company_name || r.lead_name}
          </button>
          <span className="text-sm text-muted-foreground">{r.company_name && ` · ${r.lead_name}`}</span>
          <div className="mt-1 flex flex-col gap-0.5">
            <Details r={r} />
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {r.status === "writing" ? (
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          ) : r.status === "sent" ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Check className="size-3.5" /> Sent
            </span>
          ) : (
            ready(r) && (
              <Button type="button" size="sm" variant="ghost" onClick={() => actions.drop(r)} loading={actions.busy === "drop"} aria-label="Not now">
                <X className="size-3.5" />
              </Button>
            )
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
  items: Referral[];
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

/** The pipeline's Referrals tab: asks to send, asks sent with how their pages did, and the leads customers sent. */
export function ReferralPanel({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<ReferralView>(`/projects/${projectId}/referrals`, fetcher, {
    refreshInterval: (v) => (v?.items.some((r) => r.status === "writing") ? 4000 : 0),
  });
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the referrals" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;

  const items = data.items;
  const drafts = items.filter(ready);
  const sent = items.filter((r) => r.status === "sent");
  const writing = items.filter((r) => r.status === "writing");
  const notNow = items.filter((r) => r.status === "skipped" || r.status === "failed");
  const views = items.reduce((n, r) => n + r.views, 0);
  const from = (id: string) => items.find((r) => r.id === id);
  const shared = { projectId, onOpen, onChanged: () => mutate() };

  return (
    <div className="flex flex-col gap-4">
      {!data.enabled && (
        <p className="rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
          Asking for referrals is off, so new customers aren&apos;t asked. Turn it on on the{" "}
          <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
            Autopilot page
          </Link>
          .
        </p>
      )}
      {!items.length && !data.referred.length ? (
        <Card>
          <EmptyState icon={<HeartHandshake className="size-5" />} title="No referral asks yet">
            Six weeks after a deal is won, the agent reads whether the customer is happy. If they are, it drafts an email asking whether they know
            someone at a few companies like theirs, with a page of their own to forward.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Kpi label="Ready to send" value={drafts.length} hint="asks waiting for you" />
            <Kpi label="Asked" value={sent.length} hint="customers you wrote to" />
            <Kpi label="Page views" value={views} hint="opens of the pages they forward" />
            <Kpi label="Referred" value={data.referred.length} hint="leads customers sent you" />
          </div>
          {data.referred.length > 0 && (
            <Card className="p-4">
              <MicroLabel>Leads your customers sent</MicroLabel>
              <ul className="mt-1 flex flex-col divide-y divide-border">
                {data.referred.map((l) => {
                  const r = from(l.referral_id);
                  return (
                    <li key={l.lead_id} className="flex flex-wrap items-center gap-x-2 py-2.5 text-sm">
                      <Link href={`/app/p/${projectId}/inbox?lead=${l.lead_id}`} className="font-medium hover:underline">
                        {l.full_name}
                      </Link>
                      {l.company_name && <span className="text-muted-foreground">{l.company_name}</span>}
                      {r && <span className="ml-auto text-xs text-muted-foreground">from {r.company_name || r.lead_name}</span>}
                    </li>
                  );
                })}
              </ul>
            </Card>
          )}
          <Group title="Ready to send" hint="These customers look happy. Approve one and it goes out in the thread." items={drafts} {...shared} />
          <Group title="Writing" hint="The agent is reading these accounts now." items={writing} {...shared} />
          <Group title="Asked" hint="How each customer's page is doing." items={sent} {...shared} />
          <Group title="Not now" hint="Why the agent held off. Open a deal to write one anyway." items={notNow} {...shared} />
        </>
      )}
    </div>
  );
}
