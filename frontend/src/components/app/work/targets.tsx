"use client";

import { useState } from "react";
import { MessagesSquare, Pencil } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { NumberInput } from "@/components/ui/form";
import { Badge, type BadgeTone, Card, Skeleton } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { TargetRow, Targets } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { OneOnOneDialog } from "./one-on-one";
import { CapacityCard } from "./capacity";
import { CommissionsCard } from "./commissions";
import { ContestsCard } from "./contests";
import { TeamBriefCard } from "./team-brief";
import { LoadError, MicroLabel, toastError } from "./states";

/** Where a teammate stands: hit it, on track when they're ahead of the month's pace or commit covers the rest, within
 * reach with best case too, or behind. */
function standing(r: TargetRow, pace: number): { label: string; tone: BadgeTone } | null {
  if (!r.target_cents) return null;
  if (r.won_cents >= r.target_cents) return { label: "Hit", tone: "green" };
  if (r.won_cents >= r.target_cents * pace || r.won_cents + r.commit_cents >= r.target_cents) return { label: "On track", tone: "green" };
  if (r.won_cents + r.commit_cents + r.best_case_cents >= r.target_cents) return { label: "Within reach", tone: "amber" };
  return { label: "Behind", tone: "red" };
}

/** Won, commit and best case side by side, against the target and where the month says they should be by now. */
function Bar({ r, target, pace }: { r: TargetRow; target: number; pace: number }) {
  const full = Math.max(target, r.won_cents + r.commit_cents + r.best_case_cents);
  const pct = (cents: number) => `${(cents / full) * 100}%`;
  return (
    <div className="relative h-3 overflow-hidden rounded-full bg-black/[0.06]">
      <div className="flex h-full">
        <div className="bg-brand-600" style={{ width: pct(r.won_cents) }} title="Won" />
        <div className="bg-brand-600/45" style={{ width: pct(r.commit_cents) }} title="Commit" />
        <div className="bg-brand-600/20" style={{ width: pct(r.best_case_cents) }} title="Best case" />
      </div>
      <div className="absolute inset-y-0 w-0.5 bg-foreground/50" style={{ left: pct(target * pace) }} title="Where the month says they should be" />
      <div className="absolute inset-y-0 w-0.5 bg-foreground" style={{ left: `calc(${pct(target)} - 2px)` }} title="Target" />
    </div>
  );
}

function TargetEditor({ r, currency, onSaved }: { r: TargetRow; currency: string; onSaved: () => unknown }) {
  const [editing, setEditing] = useState(false);
  const [value, setValue] = useState((r.target_cents ?? 0) / 100);
  const [saving, setSaving] = useState(false);
  if (!editing) {
    return (
      <Button type="button" size="sm" variant="ghost" onClick={() => setEditing(true)}>
        <Pencil className="size-3.5" /> {r.target_cents ? "Change target" : "Set a target"}
      </Button>
    );
  }
  const save = async () => {
    setSaving(true);
    try {
      await api(`/targets/${r.user_id}`, { method: "PUT", body: { target_cents: Math.round(value * 100) || null } });
      await onSaved();
      setEditing(false);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };
  return (
    <form
      className="flex items-center gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <NumberInput
        min={0}
        step="any"
        value={value}
        onValueChange={setValue}
        aria-label={`${r.name}'s monthly target in ${currency.toUpperCase()}`}
        className="h-8 w-32"
        autoFocus
      />
      <Button type="submit" size="sm" loading={saving}>
        Save
      </Button>
    </form>
  );
}

function Row({
  r,
  v,
  admin,
  onOpen,
  onPrep,
  onSaved,
}: {
  r: TargetRow;
  v: Targets;
  admin: boolean;
  onOpen: (dealId: string) => void;
  /** Set when the viewer may prep a 1:1 with them: an owner or admin, or the teammate themselves. */
  onPrep?: () => void;
  onSaved: () => unknown;
}) {
  const money = (cents: number) => dealMoney(cents / 100, v.currency.toUpperCase());
  const pace = v.day / v.days;
  const s = standing(r, pace);
  const gap = r.target_cents ? Math.max(r.target_cents - r.won_cents, 0) : 0;
  return (
    <li className="flex flex-col gap-2 py-4">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <p className="text-sm font-medium">{r.name}</p>
        {s && <Badge tone={s.tone}>{s.label}</Badge>}
        <p className="text-sm text-muted-foreground tabular-nums">
          {money(r.won_cents)} won{r.target_cents ? ` of ${money(r.target_cents)}` : ""}
          {gap > 0 && ` · ${money(gap)} to go`}
        </p>
        {(onPrep || (admin && r.user_id)) && (
          <div className="ml-auto flex flex-wrap items-center gap-1">
            {onPrep && (
              <Button type="button" size="sm" variant="ghost" onClick={onPrep}>
                <MessagesSquare className="size-3.5" /> 1:1 prep
              </Button>
            )}
            {admin && r.user_id && <TargetEditor key={r.target_cents} r={r} currency={v.currency} onSaved={onSaved} />}
          </div>
        )}
      </div>
      {r.target_cents ? <Bar r={r} target={r.target_cents} pace={pace} /> : null}
      {(r.commit_cents > 0 || r.best_case_cents > 0) && (
        <p className="text-xs text-muted-foreground tabular-nums">
          Commit {money(r.commit_cents)} · Best case {money(r.best_case_cents)}
        </p>
      )}
      {r.deals.length > 0 && (
        <ul className="flex flex-col gap-1">
          {r.deals.map((d) => (
            <li key={d.deal_id}>
              <button
                type="button"
                onClick={() => onOpen(d.deal_id)}
                className="flex w-full flex-wrap items-baseline gap-x-2 text-left text-sm hover:underline"
              >
                <span className="font-medium">{d.company || d.name}</span>
                <span className="tabular-nums">{money(d.value_cents)}</span>
                <span className={d.category === "commit" ? "text-brand-700" : "text-muted-foreground"}>
                  {d.category === "commit" ? "Commit" : "Best case"}
                </span>
                {d.why && <span className="text-xs text-muted-foreground">{d.why}</span>}
              </button>
            </li>
          ))}
        </ul>
      )}
    </li>
  );
}

/** The pipeline's Targets tab: the team's month, then each teammate against their target with the deals that close the gap. */
export function TargetsPanel({ onOpen }: { onOpen: (dealId: string) => void }) {
  const { me } = useMe();
  const { data: v, error, mutate } = useSWR<Targets>("/targets", fetcher);
  const [prepFor, setPrepFor] = useState<TargetRow | null>(null);
  if (error) {
    return (
      <Card>
        <LoadError error={error} retry={() => mutate()} what="the targets" />
      </Card>
    );
  }
  if (!v) return <Skeleton className="h-64 rounded-2xl" />;

  const admin = me?.role === "owner" || me?.role === "admin";
  const money = (cents: number) => dealMoney(cents / 100, v.currency.toUpperCase());
  const people = v.rows.filter((r) => r.user_id);
  const target = people.reduce((n, r) => n + (r.target_cents ?? 0), 0);
  const won = v.rows.reduce((n, r) => n + r.won_cents, 0);
  const commit = v.rows.reduce((n, r) => n + r.commit_cents, 0);
  const forecasted = v.rows.some((r) => r.commit_cents || r.best_case_cents);

  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
        {[
          {
            label: "Team target",
            value: target ? money(target) : "–",
            hint: `${people.filter((r) => r.target_cents).length} of ${people.length} with a target`,
          },
          { label: "Due by today", value: target ? money((target * v.day) / v.days) : "–", hint: `day ${v.day} of ${v.days}, ${money(won)} won` },
          { label: "Won and commit", value: money(won + commit), hint: "what the forecast says you'll close" },
          { label: "Still to find", value: target ? money(Math.max(target - won - commit, 0)) : "–", hint: "beyond what's in commit" },
        ].map((k) => (
          <Card key={k.label} className="p-4">
            <MicroLabel>{k.label}</MicroLabel>
            <p className="mt-1 truncate text-2xl font-semibold tabular-nums">{k.value}</p>
            <p className="text-xs text-muted-foreground">{k.hint}</p>
          </Card>
        ))}
      </div>
      {admin && <TeamBriefCard onPrep={(id) => setPrepFor(v.rows.find((r) => r.user_id === id) ?? null)} />}
      {!target && !admin && (
        <p className="rounded-xl bg-muted/60 px-4 py-3 text-sm text-pretty text-muted-foreground">
          No targets yet. An owner or admin sets each teammate&apos;s monthly target here, and then everyone sees what they won against it.
        </p>
      )}
      <Card className="px-4 py-0">
        <ul className="flex flex-col divide-y divide-border">
          {v.rows.map((r) => (
            <Row
              key={r.user_id ?? "none"}
              r={r}
              v={v}
              admin={admin}
              onOpen={onOpen}
              onPrep={r.user_id && (admin || r.user_id === me?.user.id) ? () => setPrepFor(r) : undefined}
              onSaved={() => mutate()}
            />
          ))}
        </ul>
      </Card>
      <ContestsCard admin={admin} />
      <CommissionsCard admin={admin} />
      {admin && <CapacityCard />}
      <p className="text-xs text-pretty text-muted-foreground">
        A deal counts for whoever owns its lead, in {v.currency.toUpperCase()} only. Commit and best case come from each project&apos;s latest
        forecast
        {forecasted ? "" : ", and there's none for this month yet. Write one on the Forecast tab"}. The thin line is where the month says they should
        be by today, and the dark one the target.
      </p>
      {prepFor?.user_id && <OneOnOneDialog userId={prepFor.user_id} name={prepFor.name} onClose={() => setPrepFor(null)} onOpen={onOpen} />}
    </div>
  );
}
