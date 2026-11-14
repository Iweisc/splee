"use client";

import { useState } from "react";
import Link from "next/link";
import { Loader2, Rocket } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Badge, Card, Modal } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { Member, PracticeAssignment, Ramp, RampPlan, RampsView } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";
import { done, scenarioLabel, tone } from "./practice";
import { MicroLabel, toastError } from "./states";

const passed = (r: Ramp) => r.practice.filter(done).length;

/** On the Playbook page: each teammate's ramp with how far they are, and planning one. Owners and admins plan anyone's. */
export function RampsPanel({ projectId }: { projectId: string }) {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const key = `/projects/${projectId}/ramps`;
  const { data, mutate } = useSWR<RampsView>(key, fetcher, {
    refreshInterval: (v) => (v?.ramps.some((r) => r.status === "writing") ? 2500 : 0),
  });
  const { data: members } = useSWR<Member[]>(admin ? "/organization/users" : null, fetcher);
  const [who, setWho] = useState("");
  const [busy, setBusy] = useState(false);
  const [opened, setOpened] = useState<string | null>(null);
  if (!data || !me) return null;

  const plan = async (userId: string) => {
    setBusy(true);
    try {
      await mutate(api<RampsView>(`${key}/${userId}`, { method: "POST" }), { revalidate: false });
      setOpened(userId);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const ramped = new Set(data.ramps.map((r) => r.user_id));
  const open = data.ramps.find((r) => r.user_id === opened);
  // Members only ever see and plan their own.
  if (!admin && !data.ramps.length) {
    return (
      <Card className="mx-auto flex w-full max-w-3xl flex-wrap items-center gap-3 px-5 py-4">
        <Rocket className="size-4 text-muted-foreground" />
        <p className="min-w-0 flex-1 basis-60 text-sm text-pretty text-muted-foreground">
          New here? The agent can turn this playbook into your first weeks, with practice calls to pass.
        </p>
        <Button type="button" size="sm" variant="outline" onClick={() => plan(me.user.id)} loading={busy}>
          Plan my ramp
        </Button>
      </Card>
    );
  }
  return (
    <Card className="mx-auto w-full max-w-3xl overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 px-5 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-72">
          <p className="flex items-center gap-1.5 text-sm font-semibold">
            <Rocket className="size-4 text-muted-foreground" /> Ramping new teammates
          </p>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            The agent turns this playbook into their first weeks, with practice calls on your open deals that they pass at {data.pass_score}.
          </p>
        </div>
        {admin && (
          <div className="flex items-center gap-2 max-sm:w-full">
            <Select value={who} onChange={(e) => setWho(e.target.value)} className="h-9 w-auto max-sm:flex-1" aria-label="Teammate">
              <option value="">Pick a teammate</option>
              {members
                ?.filter((m) => !ramped.has(m.user_id))
                .map((m) => (
                  <option key={m.user_id} value={m.user_id}>
                    {m.name || m.email}
                  </option>
                ))}
            </Select>
            <Button type="button" size="sm" onClick={() => plan(who)} loading={busy} disabled={!who}>
              Plan a ramp
            </Button>
          </div>
        )}
      </div>
      {data.ramps.length > 0 && (
        <ul className="divide-y divide-border border-t border-border">
          {data.ramps.map((r) => (
            <li key={r.id}>
              <button
                type="button"
                onClick={() => setOpened(r.user_id)}
                className="flex w-full items-center gap-3 px-5 py-3 text-left hover:bg-black/[0.03]"
              >
                <span className="min-w-0 flex-1 truncate text-sm font-medium">{r.name}</span>
                <RampState r={r} />
              </button>
            </li>
          ))}
        </ul>
      )}
      {open && <RampDialog r={open} passScore={data.pass_score} onClose={() => setOpened(null)} onPlan={() => plan(open.user_id)} busy={busy} />}
    </Card>
  );
}

function RampState({ r }: { r: Ramp }) {
  if (r.status === "writing") {
    return (
      <span className="flex items-center gap-1.5 text-xs text-muted-foreground">
        <Loader2 className="size-3.5 animate-spin" /> Planning…
      </span>
    );
  }
  if (r.status === "failed") return <Badge tone="red">Couldn&apos;t plan it</Badge>;
  const total = r.practice.length;
  const n = passed(r);
  if (total > 0 && n === total) return <Badge tone="green">Ready to sell</Badge>;
  return (
    <span className="flex shrink-0 items-center gap-2 text-xs whitespace-nowrap text-muted-foreground tabular-nums">
      <span className="h-1.5 w-24 overflow-hidden rounded-full bg-muted max-sm:hidden">
        <span className="block h-full rounded-full bg-brand-500" style={{ width: `${total ? (n / total) * 100 : 0}%` }} />
      </span>
      {n} of {plural(total, "practice call")} passed
    </span>
  );
}

/** One practice call in the plan, with its best score so far. */
function PracticeItem({ p, a, passScore }: { p?: PracticeAssignment; a: RampPlan["weeks"][number]["practice"][number]; passScore: number }) {
  const status = !p ? (
    <Badge>Not assigned</Badge>
  ) : done(p) ? (
    <Badge tone="green">Passed {p.score!.score}</Badge>
  ) : p.score ? (
    <Badge tone={tone(p.score.score)}>Best {p.score.score}</Badge>
  ) : (
    <Badge>{p.session_id ? "Started" : "To do"}</Badge>
  );
  return (
    <li className="rounded-xl border border-border p-3 text-sm text-pretty">
      <div className="flex items-start gap-2">
        <p className="min-w-0 flex-1">
          <span className="font-medium">{scenarioLabel(a.scenario)}</span>
          <span className="text-muted-foreground"> on </span>
          <Link href={`/app/pipeline${qs({ deal: a.deal_id })}`} className="font-medium hover:underline">
            {a.company}
          </Link>
        </p>
        {status}
      </div>
      <p className="mt-1 text-muted-foreground">
        {a.skill}. {a.drill}
        {p && !done(p) && p.score && ` Passes at ${passScore}.`}
      </p>
    </li>
  );
}

/** A teammate's ramp in its own window: the weeks, their steps and practice calls with how each went, and planning it again. */
function RampDialog({ r, passScore, onClose, onPlan, busy }: { r: Ramp; passScore: number; onClose: () => void; onPlan: () => void; busy: boolean }) {
  const first = r.name.split(" ")[0];
  const find = (dealId: string, scenario: string) => r.practice.find((p) => p.deal_id === dealId && p.scenario === scenario);
  return (
    <Modal open onClose={onClose} className="max-w-2xl" title={`${first}'s ramp`}>
      {r.status === "writing" ? (
        <p className="flex items-center justify-center gap-2 py-10 text-sm text-muted-foreground">
          <Loader2 className="size-4 animate-spin" /> Reading the playbook and your open deals…
        </p>
      ) : !r.plan ? (
        <div className="flex flex-col items-center gap-3 py-8 text-center text-sm">
          <p className="text-pretty text-muted-foreground">{r.error || "The agent couldn't plan this ramp."}</p>
          <Button type="button" size="sm" onClick={onPlan} loading={busy}>
            Try again
          </Button>
        </div>
      ) : (
        <div className="flex flex-col gap-5">
          <div className="flex flex-col items-start gap-2">
            <RampState r={r} />
            <p className="text-sm text-pretty">{r.plan.summary}</p>
          </div>
          {r.plan.weeks.map((w, i) => (
            <section key={w.title} className="flex flex-col gap-2 border-t border-border pt-4">
              <MicroLabel>Week {i + 1}</MicroLabel>
              <div>
                <p className="font-medium">{w.title}</p>
                <p className="text-sm text-pretty text-muted-foreground">{w.goal}</p>
              </div>
              {w.steps.length > 0 && (
                <ul className="flex list-disc flex-col gap-1 pl-5 text-sm text-pretty">
                  {w.steps.map((s) => (
                    <li key={s}>{s}</li>
                  ))}
                </ul>
              )}
              {w.practice.length > 0 && (
                <ul className="flex flex-col gap-2">
                  {w.practice.map((a) => (
                    <PracticeItem key={`${a.deal_id}-${a.scenario}`} p={find(a.deal_id, a.scenario)} a={a} passScore={passScore} />
                  ))}
                </ul>
              )}
            </section>
          ))}
          <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border pt-4">
            <p className="mr-auto text-xs text-muted-foreground">
              {[r.requested_by && `Planned by ${r.requested_by}`, timeAgo(r.created_at)].filter(Boolean).join(" · ")}
            </p>
            <Button
              type="button"
              size="sm"
              variant="outline"
              onClick={() => {
                if (r.practice.some((p) => p.session_id) && !confirm(`Plan ${first}'s ramp again? The new plan's practice calls start over.`)) return;
                onPlan();
              }}
              loading={busy}
            >
              Plan it again
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}
