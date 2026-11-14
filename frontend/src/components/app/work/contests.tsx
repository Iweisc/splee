"use client";

import { useState } from "react";
import { ChevronDown, Plus, Trash2, Trophy } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Contest, ContestMetric, ContestsView } from "@/lib/types";
import { cn, dealMoney } from "@/lib/utils";
import { toastError } from "./states";

export const METRICS: { value: ContestMetric; label: string }[] = [
  { value: "won_value", label: "Revenue won" },
  { value: "deals_won", label: "Deals won" },
  { value: "pipeline", label: "New pipeline" },
  { value: "calls", label: "Calls made" },
  { value: "practice", label: "Practice calls passed" },
];

/** A score as the leaderboard shows it: money for revenue and pipeline, a count otherwise. */
export function score(metric: ContestMetric, value: number, currency: string) {
  if (metric === "won_value" || metric === "pipeline") return dealMoney(value / 100, currency.toUpperCase());
  const unit = { deals_won: ["deal", "deals"], calls: ["call", "calls"], practice: ["pass", "passes"] }[metric];
  return `${value.toLocaleString("en-US")} ${value === 1 ? unit[0] : unit[1]}`;
}

export const ended = (c: Contest) => new Date(c.ends_at).getTime() <= Date.now();

/** "Ends today", "Ends tomorrow" or "Ends Fri, Nov 21". */
export function endsLine(c: Contest) {
  const end = new Date(c.ends_at);
  const days = Math.round((new Date(end).setHours(0, 0, 0, 0) - new Date().setHours(0, 0, 0, 0)) / 86_400_000);
  if (days <= 0) return "Ends today";
  if (days === 1) return "Ends tomorrow";
  return `Ends ${end.toLocaleDateString("en-US", { weekday: "short", month: "short", day: "numeric" })}`;
}

function ordinal(n: number) {
  const teen = n % 100 >= 11 && n % 100 <= 13;
  return `${n}${teen ? "th" : ({ 1: "st", 2: "nd", 3: "rd" }[n % 10] ?? "th")}`;
}

/** Where you stand, like "2nd of 3, 3 calls behind Dana" or "In the lead by $2,400". */
export function standing(c: Contest, userId: string, currency: string) {
  const s = c.standings;
  const i = s.findIndex((r) => r.user_id === userId);
  if (i < 0) return null;
  const me = s[i];
  const first = (name: string) => name.split(" ")[0];
  if (s[0].value === 0) return "Nobody has scored yet.";
  if (me.value === 0) return `Nothing yet. ${first(s[0].name)} leads with ${score(c.metric, s[0].value, currency)}.`;
  if (me.value === s[0].value) {
    const next = s.find((r) => r.user_id !== userId && r.value < me.value);
    const tied = s.filter((r) => r.value === me.value).length > 1;
    if (tied) return `Tied for the lead with ${score(c.metric, me.value, currency)}.`;
    return next ? `In the lead by ${score(c.metric, me.value - next.value, currency)}.` : `In the lead with ${score(c.metric, me.value, currency)}.`;
  }
  const rank = s.filter((r) => r.value > me.value).length + 1;
  const ahead = s[rank - 2];
  return `${ordinal(rank)} of ${s.length}, ${score(c.metric, ahead.value - me.value, currency)} behind ${first(ahead.name)}.`;
}

/** On the Targets tab: each running contest's leaderboard, the ones that just ended, and for owners and admins a way to
 * start one. */
export function ContestsCard({ admin }: { admin: boolean }) {
  const { data: v, mutate } = useSWR<ContestsView>("/contests", fetcher, { refreshInterval: 60_000 });
  const [starting, setStarting] = useState(false);
  if (!v) return null;
  if (!admin && !v.contests.length) return null;
  const running = v.contests.filter((c) => !ended(c));
  const done = v.contests.filter(ended);

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-64">
          <p className="flex items-center gap-2 text-sm font-semibold">
            <Trophy className="size-4 text-muted-foreground" /> Contests
          </p>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            {running.length
              ? "Everyone on the team is in. The winner is posted to your chats when it ends."
              : "Run a contest on revenue, deals, pipeline, calls or practice, with a prize, until a date you pick."}
          </p>
        </div>
        {admin && !starting && (
          <Button type="button" size="sm" variant={running.length ? "outline" : "primary"} onClick={() => setStarting(true)}>
            <Plus className="size-3.5" /> Start a contest
          </Button>
        )}
      </div>
      {starting && (
        <StartForm
          onDone={async (saved) => {
            if (saved) await mutate(saved, { revalidate: false });
            setStarting(false);
          }}
        />
      )}
      {running.map((c) => (
        <Board key={c.id} c={c} currency={v.currency} admin={admin} onDeleted={() => mutate()} />
      ))}
      {done.length > 0 && (
        <ul className="divide-y divide-border border-t border-border bg-muted/40 text-sm">
          {done.map((c) => {
            const top = c.standings[0];
            const tied = c.standings.filter((s) => top && s.value === top.value).length > 1;
            return (
              <li key={c.id}>
                <details className="group px-4 py-2.5">
                  <summary className="flex cursor-pointer list-none flex-wrap items-baseline gap-x-3 gap-y-0.5 [&::-webkit-details-marker]:hidden">
                    <span className="font-medium">{c.name}</span>
                    <span className="text-muted-foreground">
                      {!top || top.value === 0
                        ? "Ended with nobody scoring"
                        : tied
                          ? `Ended in a tie at ${score(c.metric, top.value, v.currency)}`
                          : `Won by ${top.name} with ${score(c.metric, top.value, v.currency)}`}
                      {c.prize && top?.value ? `. Prize: ${c.prize}` : ""}
                    </span>
                    <span className="ml-auto flex items-center gap-1 text-xs text-muted-foreground">
                      Ended {new Date(c.ends_at).toLocaleDateString("en-US", { month: "short", day: "numeric" })}
                      <ChevronDown className="size-3.5 transition-transform group-open:rotate-180" />
                    </span>
                  </summary>
                  <Standings c={c} currency={v.currency} />
                </details>
              </li>
            );
          })}
        </ul>
      )}
    </Card>
  );
}

/** A running contest: what it counts, when it ends, the prize, and its standings. */
function Board({ c, currency, admin, onDeleted }: { c: Contest; currency: string; admin: boolean; onDeleted: () => unknown }) {
  const remove = async () => {
    if (!confirm(`Delete "${c.name}"? Nobody is told, and its leaderboard goes with it.`)) return;
    try {
      await api(`/contests/${c.id}`, { method: "DELETE" });
      await onDeleted();
    } catch (e) {
      toastError(e);
    }
  };
  return (
    <div className="border-t border-border px-4 py-3">
      <div className="flex items-start gap-3">
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold">{c.name}</p>
          <p className="text-xs text-pretty text-muted-foreground">
            {METRICS.find((m) => m.value === c.metric)?.label} · {endsLine(c)}
            {c.prize && ` · Prize: ${c.prize}`}
          </p>
        </div>
        {admin && (
          <button type="button" onClick={remove} className="mt-0.5 text-muted-foreground hover:text-foreground" aria-label={`Delete ${c.name}`}>
            <Trash2 className="size-3.5" />
          </button>
        )}
      </div>
      <Standings c={c} currency={currency} />
    </div>
  );
}

/** The leaderboard, the leader's bar full and everyone else's against it, your own in green. */
function Standings({ c, currency }: { c: Contest; currency: string }) {
  const { me } = useMe();
  const top = c.standings[0]?.value ?? 0;
  return (
    <ol className="mt-2 flex flex-col gap-1.5">
      {c.standings.map((s, i) => {
        const leads = top > 0 && s.value === top;
        const mine = s.user_id === me?.user.id;
        return (
          <li
            key={s.user_id}
            className="grid grid-cols-[1.25rem_minmax(0,9rem)_1fr_auto] items-center gap-x-3 text-sm max-sm:grid-cols-[1.25rem_1fr_auto]"
          >
            <span className="text-xs text-muted-foreground tabular-nums">
              {leads ? <Trophy className="size-3.5 text-amber-500" aria-label="Leading" /> : i + 1}
            </span>
            <span className={cn("truncate", mine && "font-semibold")}>{s.name}</span>
            <span className="h-2 overflow-hidden rounded-full bg-black/[0.06] max-sm:hidden">
              <span
                className={cn("block h-full rounded-full transition-[width] duration-500", mine ? "bg-brand-600" : "bg-foreground/60")}
                style={{ width: `${top > 0 ? Math.max((s.value / top) * 100, s.value > 0 ? 2 : 0) : 0}%` }}
              />
            </span>
            <span className={cn("text-right tabular-nums", leads ? "font-semibold" : "text-muted-foreground")}>
              {score(c.metric, s.value, currency)}
            </span>
          </li>
        );
      })}
    </ol>
  );
}

/** The end of the coming Friday, or of next week's when today is Friday or later. */
function nextFriday() {
  const d = new Date();
  d.setDate(d.getDate() + ((5 - d.getDay() + 7) % 7 || 7));
  return d.toLocaleDateString("en-CA");
}

function StartForm({ onDone }: { onDone: (saved: ContestsView | null) => unknown }) {
  const [name, setName] = useState("");
  const [metric, setMetric] = useState<ContestMetric>("calls");
  const [until, setUntil] = useState(nextFriday);
  const [prize, setPrize] = useState("");
  const [saving, setSaving] = useState(false);
  const save = async () => {
    setSaving(true);
    try {
      // The contest runs to the end of the day picked, in the browser's time zone.
      const ends_at = new Date(`${until}T23:59:59`).toISOString();
      await onDone(await api<ContestsView>("/contests", { method: "POST", body: { name, metric, prize, ends_at } }));
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };
  return (
    <form
      className="flex flex-wrap items-end gap-3 border-t border-border bg-muted/40 px-4 py-3 text-sm"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <label className="flex min-w-48 flex-1 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Name</span>
        <Input
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          placeholder="Call blitz week"
          className="h-9"
          required
          autoFocus
        />
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Most</span>
        <Select value={metric} onChange={(e) => setMetric(e.target.value as ContestMetric)} className="h-9 w-auto">
          {METRICS.map((m) => (
            <option key={m.value} value={m.value}>
              {m.label}
            </option>
          ))}
        </Select>
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Until</span>
        <Input
          type="date"
          value={until}
          min={new Date().toLocaleDateString("en-CA")}
          onChange={(e) => setUntil(e.target.value)}
          className="h-9 w-auto"
          required
        />
      </label>
      <label className="flex min-w-40 flex-1 flex-col gap-1">
        <span className="text-xs text-muted-foreground">Prize</span>
        <Input value={prize} onChange={(e) => setPrize(e.target.value)} maxLength={120} placeholder="Team lunch on me" className="h-9" />
      </label>
      <div className="flex gap-2">
        <Button type="submit" size="sm" loading={saving}>
          Start it
        </Button>
        <Button type="button" size="sm" variant="ghost" onClick={() => onDone(null)}>
          Cancel
        </Button>
      </div>
    </form>
  );
}
