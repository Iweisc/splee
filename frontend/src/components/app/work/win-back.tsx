"use client";

import { useState } from "react";
import Link from "next/link";
import { Check, Loader2, PenLine, RotateCcw, Send, Sparkles, X } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Deal, WinBack, WinBackView } from "@/lib/types";
import { cn, dealMoney, plural } from "@/lib/utils";
import { reasonLabel } from "./deal-review";
import { CardHeader, CardHeaderText, LoadError, MicroLabel, useDealActions } from "./states";

const busyStatus = (w: WinBack) => w.status === "planning" || w.status === "writing";
const day = (d: string, year = false) =>
  new Date(`${d}T00:00:00`).toLocaleDateString("en-US", { day: "numeric", month: "short", ...(year && { year: "numeric" }) });
const today = () => new Date().toLocaleDateString("en-CA");
const sent = (w: WinBack) => w.status === "drafted" && w.email_status !== "draft";

/** Writing it now, moving its day, dropping it and sending the draft, each refreshing every view of it. */
function useActions(dealId: string, onChanged: () => unknown) {
  const { busy, act, send } = useDealActions(onChanged, (k) => k.includes("/win-back"));
  return {
    busy,
    write: () => act("write", () => api(`/deals/${dealId}/win-back`, { method: "POST" })),
    move: (w: WinBack, reach_out_on: string) => act("move", () => api(`/win-backs/${w.id}`, { method: "PATCH", body: { reach_out_on } })),
    drop: (w: WinBack) =>
      act(
        "drop",
        () => api(`/win-backs/${w.id}`, { method: "PATCH", body: { dismissed: true } }),
        "Dropped. The agent won't write to them about this deal.",
      ),
    send: (w: WinBack) => send(w.email_id),
  };
}

type Actions = ReturnType<typeof useActions>;

/** A date field that moves the win-back's day when it changes. */
function MoveDay({ w, actions, label }: { w: WinBack; actions: Actions; label: string }) {
  return (
    <label className="flex items-center gap-2 text-xs text-muted-foreground">
      {label}
      <Input
        type="date"
        min={today()}
        defaultValue={w.reach_out_on ?? ""}
        disabled={actions.busy === "move"}
        onChange={(e) => e.target.value && actions.move(w, e.target.value)}
        className="h-8 w-auto"
      />
    </label>
  );
}

/** An email the agent drafted in the thread, to approve from here. */
export function ThreadDraft({
  subject,
  body,
  projectId,
  onAnother,
  onSend,
  busy,
}: {
  subject: string | null;
  body: string | null;
  projectId: string;
  onAnother: () => void;
  onSend: () => void;
  busy: string | null;
}) {
  return (
    <div className="rounded-lg border border-border bg-card p-3">
      <p className="text-sm font-medium">{subject}</p>
      <p className="mt-1 text-sm whitespace-pre-wrap text-foreground/80">{body}</p>
      <div className="mt-3 flex flex-wrap items-center justify-end gap-2">
        <Link href={`/app/p/${projectId}/emails`} className="mr-auto text-xs text-muted-foreground hover:text-foreground hover:underline">
          Edit it on the Emails page
        </Link>
        <Button type="button" size="sm" variant="ghost" onClick={onAnother} loading={busy === "write"}>
          <PenLine className="size-3.5" /> Write another
        </Button>
        <Button type="button" size="sm" onClick={onSend} loading={busy === "send"}>
          <Send className="size-3.5" /> Approve and send
        </Button>
      </div>
    </div>
  );
}

function Draft({ w, actions, projectId }: { w: WinBack; actions: Actions; projectId: string }) {
  return (
    <ThreadDraft
      subject={w.email_subject}
      body={w.email_body}
      projectId={projectId}
      onAnother={actions.write}
      onSend={() => actions.send(w)}
      busy={actions.busy}
    />
  );
}

/** What the agent planned and why, in a line or two. */
function Reasons({ w }: { w: WinBack }) {
  return (
    <>
      {w.came_back && (
        <p className="flex items-start gap-1.5 text-sm text-pretty text-brand-700">
          <Sparkles className="mt-0.5 size-3.5 shrink-0" /> {w.came_back}.
        </p>
      )}
      {w.why && <p className="text-sm text-pretty text-muted-foreground">{w.why}</p>}
      {w.angle && <p className="text-sm text-pretty text-muted-foreground">{w.angle}</p>}
    </>
  );
}

/** In a lost deal's window: when the agent will try again and why, or the email it wrote. */
export function WinBackSection({ deal }: { deal: Deal }) {
  const key = `/deals/${deal.id}/win-back`;
  const {
    data: w,
    error,
    mutate,
  } = useSWR<WinBack | null>(deal.status === "lost" ? key : null, fetcher, {
    refreshInterval: (w) => (w && busyStatus(w) ? 2500 : 0),
  });
  const actions = useActions(deal.id, () => mutate());
  if (deal.status !== "lost" || error || w === undefined) return null;

  const heading = !w
    ? "Win it back"
    : w.status === "drafted"
      ? sent(w)
        ? "Win-back sent"
        : "Win-back ready"
      : w.reach_out_on && w.status === "waiting"
        ? `Try again on ${day(w.reach_out_on, true)}`
        : "Win it back";
  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <CardHeader>
        <CardHeaderText>
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <RotateCcw className="size-3.5" /> {heading}
          </p>
          <div className="mt-1 flex flex-col gap-1 text-xs">
            {!w ? (
              <p className="text-sm text-muted-foreground">
                A day after a deal is lost, the agent picks the day to try again from why it was lost. It can write to them now instead.
              </p>
            ) : w.status === "planning" ? (
              <p className="text-sm text-muted-foreground">The agent is reading why you lost it and picking a day…</p>
            ) : w.status === "writing" ? (
              <p className="text-sm text-muted-foreground">The agent is writing to {w.lead_name}…</p>
            ) : w.status === "failed" ? (
              <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>
            ) : w.status === "skipped" ? (
              <p className="text-sm text-pretty text-muted-foreground">No win-back: {w.skipped.replace(/\.$/, "")}.</p>
            ) : sent(w) ? (
              <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
                <Check className="size-3.5" /> The email is approved and goes out in the thread.
              </p>
            ) : (
              <Reasons w={w} />
            )}
          </div>
        </CardHeaderText>
        {w && busyStatus(w) ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          (!w || w.status !== "drafted") && (
            <Button type="button" size="sm" variant="outline" onClick={actions.write} loading={actions.busy === "write"}>
              <PenLine className="size-3.5" /> Write it now
            </Button>
          )
        )}
      </CardHeader>
      {w?.status === "drafted" && w.email_status === "draft" && <Draft w={w} actions={actions} projectId={deal.project_id} />}
      {w && !busyStatus(w) && w.status !== "drafted" && (
        <div className="flex flex-wrap items-center justify-between gap-2">
          <MoveDay w={w} actions={actions} label={w.status === "skipped" ? "Try anyway on" : "Move to"} />
          <Button type="button" size="sm" variant="ghost" onClick={() => actions.drop(w)} loading={actions.busy === "drop"}>
            <X className="size-3.5" /> Don&apos;t win it back
          </Button>
        </div>
      )}
    </section>
  );
}

/** One lost deal on the Win-back tab. */
function Row({ w, projectId, onOpen, onChanged }: { w: WinBack; projectId: string; onOpen: (dealId: string) => void; onChanged: () => unknown }) {
  const actions = useActions(w.deal_id, onChanged);
  const ready = w.status === "drafted" && w.email_status === "draft";
  return (
    <li className="flex flex-col gap-2 py-3">
      <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
        <span className={cn("mt-0.5 w-16 shrink-0 text-xs font-medium tabular-nums", w.came_back ? "text-brand-700" : "text-muted-foreground")}>
          {w.reach_out_on && w.status !== "skipped" ? day(w.reach_out_on) : "–"}
        </span>
        <div className="min-w-0 flex-1">
          <button type="button" onClick={() => onOpen(w.deal_id)} className="text-left text-sm font-medium hover:underline">
            {w.company_name || w.lead_name}
          </button>
          <span className="text-sm text-muted-foreground">
            {w.company_name && ` · ${w.lead_name}`}
            {w.value_cents > 0 && ` · ${dealMoney(w.value_cents / 100, w.currency.toUpperCase())}`}
            {w.reason && ` · ${reasonLabel("lost", w.reason)}`}
          </span>
          <div className="mt-1 flex flex-col gap-0.5">
            {w.status === "skipped" ? (
              <p className="text-sm text-pretty text-muted-foreground">{w.skipped}</p>
            ) : w.status === "failed" ? (
              <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>
            ) : w.status === "planning" ? (
              <p className="text-sm text-muted-foreground">Picking a day…</p>
            ) : (
              <Reasons w={w} />
            )}
          </div>
        </div>
        <div className="flex shrink-0 items-center gap-1">
          {busyStatus(w) ? (
            <Loader2 className="size-4 animate-spin text-muted-foreground" />
          ) : sent(w) ? (
            <span className="flex items-center gap-1 text-xs text-muted-foreground">
              <Check className="size-3.5" /> Sent
            </span>
          ) : (
            !ready && (
              <Button type="button" size="sm" variant="outline" onClick={actions.write} loading={actions.busy === "write"}>
                <PenLine className="size-3.5" /> Write it now
              </Button>
            )
          )}
        </div>
      </div>
      {ready && (
        <div className="sm:pl-19">
          <Draft w={w} actions={actions} projectId={projectId} />
        </div>
      )}
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
  items: WinBack[];
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
        {items.map((w) => (
          <Row key={w.id} w={w} {...rest} />
        ))}
      </ul>
    </Card>
  );
}

/** The pipeline's Win-back tab: every lost deal the agent plans to try again, the ones that came back, and drafts to send. */
export function WinBackPanel({ projectId, onOpen }: { projectId: string; onOpen: (dealId: string) => void }) {
  const { data, error, mutate } = useSWR<WinBackView>(`/projects/${projectId}/win-backs`, fetcher, {
    refreshInterval: (v) => (v?.items.some(busyStatus) ? 4000 : 0),
  });
  const [in30Days] = useState(() => new Date(Date.now() + 30 * 86_400_000).toLocaleDateString("en-CA"));
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the win-backs" />
      </Card>
    );
  }
  if (!data) return <Skeleton className="h-64 rounded-2xl" />;

  const items = data.items;
  const ready = items.filter((w) => w.status === "drafted" && w.email_status === "draft");
  const cameBack = items.filter((w) => w.came_back && (w.status === "waiting" || w.status === "writing"));
  const upcoming = items
    .filter((w) => !cameBack.includes(w) && ["planning", "waiting", "writing", "failed"].includes(w.status))
    .sort((a, b) => (a.reach_out_on ?? "").localeCompare(b.reach_out_on ?? ""));
  const done = items.filter(sent);
  const skipped = items.filter((w) => w.status === "skipped");
  const soon = upcoming.filter((w) => w.reach_out_on && w.reach_out_on <= in30Days);
  const shared = { projectId, onOpen, onChanged: () => mutate() };

  return (
    <div className="flex flex-col gap-4">
      {!data.enabled && (
        <p className="rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
          Winning back lost deals is off, so newly lost deals get no plan. Turn it on on the{" "}
          <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
            Autopilot page
          </Link>
          .
        </p>
      )}
      {!items.length ? (
        <Card>
          <EmptyState icon={<RotateCcw className="size-5" />} title="No lost deals to win back">
            A day after a deal is lost, the agent reads why and picks the day to try again, like a month before the contract they chose renews. If
            they come back to your website sooner, it writes to them then.
          </EmptyState>
        </Card>
      ) : (
        <>
          <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
            <Kpi label="Ready to send" value={ready.length} hint="drafts waiting for you" />
            <Kpi label="Came back" value={cameBack.length} hint="read your site or emails again" />
            <Kpi label="Next 30 days" value={soon.length} hint={`of ${plural(upcoming.length, "planned win-back")}`} />
            <Kpi label="Sent" value={done.length} hint="win-back emails approved" />
          </div>
          <Group title="Ready to send" hint="The agent wrote these today. Approve one and it goes out in the thread." items={ready} {...shared} />
          <Group
            title="Came back"
            hint="They showed interest again, so the agent writes to them now instead of waiting."
            items={cameBack}
            {...shared}
          />
          <Group
            title="Coming up"
            hint="The day the agent picked from why each deal was lost. Open a deal to move its day."
            items={upcoming}
            {...shared}
          />
          <Group title="Sent" hint="Win-back emails that went out, or are about to." items={done} {...shared} />
          <Group title="Not worth another try" hint="The agent's reasons. Open a deal to pick a day anyway." items={skipped} {...shared} />
        </>
      )}
    </div>
  );
}

export function Kpi({ label, value, hint }: { label: string; value: number; hint: string }) {
  return (
    <Card className="p-4">
      <MicroLabel>{label}</MicroLabel>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      <p className="text-xs text-muted-foreground">{hint}</p>
    </Card>
  );
}
