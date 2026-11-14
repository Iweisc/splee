"use client";

import { use, useState } from "react";
import Link from "next/link";
import { ArrowRight, Zap } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ListSkeleton, LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select, Switch } from "@/components/ui/form";
import { Badge, Card, EmptyState, Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Play, PlayAction, PlayFiring, PlaysOverview, PlayTrigger, SegmentWithStats } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";

const TRIGGERS: Record<PlayTrigger, { label: string; lead: boolean }> = {
  funding: { label: "A company raises money", lead: false },
  news: { label: "A company has news", lead: false },
  hiring: { label: "A company posts a job you watch", lead: false },
  visit: { label: "A company visits your website", lead: false },
  job_change: { label: "A past customer moves to a new company", lead: true },
  score: { label: "A lead's score reaches", lead: true },
};

const ACTIONS: Record<PlayAction, string> = {
  add_to_campaign: "Add it to a campaign and find its people",
  write_email: "Write their first email",
  notify: "Tell my chats",
};

/** What each action can follow: companies go into campaigns, leads get emails, and anything can ping the chats. */
function actionsFor(trigger: PlayTrigger): PlayAction[] {
  return TRIGGERS[trigger].lead ? ["write_email", "notify"] : ["add_to_campaign", "notify"];
}

type Draft = {
  name: string;
  trigger: PlayTrigger;
  action: PlayAction;
  segment_id: string;
  min_score: number;
  daily_cap: number;
  backfill_days: number;
};

// Plays most teams start with. A click opens the editor with it filled in.
const TEMPLATES: (Omit<Draft, "segment_id" | "backfill_days"> & { why: string })[] = [
  { name: "Funded companies", trigger: "funding", action: "add_to_campaign", min_score: 0, daily_cap: 20, why: "New money means new budget." },
  { name: "Hiring for my buyers", trigger: "hiring", action: "add_to_campaign", min_score: 40, daily_cap: 20, why: "The job post makes the opener." },
  { name: "Website visitors", trigger: "visit", action: "add_to_campaign", min_score: 30, daily_cap: 10, why: "They already know you." },
  { name: "Past customer moved", trigger: "job_change", action: "write_email", min_score: 0, daily_cap: 20, why: "Champions buy again." },
  { name: "Hot lead", trigger: "score", action: "notify", min_score: 70, daily_cap: 20, why: "Call while they're looking." },
];

const OUTCOME_TONE = { done: "green", skipped: "neutral", failed: "red" } as const;

export default function PlaysPage({ params }: PageProps<"/app/p/[projectId]/plays">) {
  const { projectId } = use(params);
  const key = `/projects/${projectId}/plays`;
  const { data, error, mutate } = useSWR<PlaysOverview>(key, fetcher, { refreshInterval: 60_000 });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const [editing, setEditing] = useState<(Draft & { id?: string }) | null>(null);

  const toggle = async (play: Play, enabled: boolean) => {
    mutate(data && { ...data, plays: data.plays.map((p) => (p.id === play.id ? { ...p, enabled } : p)) }, { revalidate: false });
    try {
      await api(`/plays/${play.id}`, { method: "PATCH", body: { enabled } });
    } catch (e) {
      toastError(e);
    }
    mutate();
  };

  const blank = (t?: (typeof TEMPLATES)[number]): Draft => ({
    name: t?.name ?? "",
    trigger: t?.trigger ?? "funding",
    action: t?.action ?? "add_to_campaign",
    min_score: t?.min_score ?? 0,
    daily_cap: t?.daily_cap ?? 20,
    segment_id: segments?.[0]?.id ?? "",
    backfill_days: 0,
  });

  return (
    <>
      <PageHeader
        title="Plays"
        description="When something happens at a company or to a lead, Splee acts on it within 5 minutes: adds the company to a campaign, writes the email or tells your team."
        actions={
          <Button onClick={() => setEditing(blank())} disabled={!data}>
            <Zap className="size-4" /> New play
          </Button>
        }
      />
      <PageBody className="flex flex-col gap-6">
        <section>
          <MicroLabel className="mb-2">Start from a play</MicroLabel>
          <div className="grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-5">
            {TEMPLATES.map((t) => (
              <button
                key={t.name}
                type="button"
                onClick={() => setEditing(blank(t))}
                className="flex flex-col items-start gap-1 rounded-2xl bg-card p-4 text-left shadow-card transition-colors hover:bg-accent"
              >
                <span className="text-sm font-medium">{t.name}</span>
                <span className="text-xs text-pretty text-muted-foreground">
                  {TRIGGERS[t.trigger].label}
                  {t.trigger === "score" ? ` ${t.min_score}` : ""}, then {ACTIONS[t.action].toLowerCase()}.
                </span>
                <span className="mt-auto pt-1 text-xs text-muted-foreground italic">{t.why}</span>
              </button>
            ))}
          </div>
        </section>

        <Card className="overflow-hidden">
          {error ? (
            <LoadError error={error} retry={() => mutate()} what="plays" />
          ) : !data ? (
            <ListSkeleton rows={3} />
          ) : data.plays.length === 0 ? (
            <EmptyState icon={<Zap className="size-5" />} title="No plays yet">
              Pick one above or make your own. A play acts on what happens from the moment you add it.
            </EmptyState>
          ) : (
            <ul className="divide-y divide-border">
              {data.plays.map((p) => (
                <li key={p.id} className={cn("flex items-center gap-4 px-4 py-3", !p.enabled && "text-muted-foreground")}>
                  <Switch checked={p.enabled} onChange={(on) => toggle(p, on)} label={`${p.name} on`} />
                  <button
                    type="button"
                    onClick={() => setEditing({ ...p, segment_id: p.segment_id ?? "", backfill_days: 0 })}
                    className="min-w-0 flex-1 text-left"
                  >
                    <span className="block truncate font-medium text-foreground hover:underline">{p.name}</span>
                    <span className="flex min-w-0 flex-wrap items-center gap-x-1.5 text-xs text-muted-foreground">
                      <span>
                        {TRIGGERS[p.trigger].label}
                        {p.trigger === "score" ? ` ${p.min_score}` : p.min_score > 0 ? `, scoring ${p.min_score}+` : ""}
                      </span>
                      <ArrowRight className="size-3 shrink-0" />
                      <span>{p.action === "add_to_campaign" ? `Add to ${p.segment_name ?? "a deleted campaign"}` : ACTIONS[p.action]}</span>
                    </span>
                  </button>
                  <span className="hidden shrink-0 text-right text-xs text-muted-foreground tabular-nums sm:block">
                    {plural(p.fired_today, "time")} today
                    <span className="block">{p.fired_month} this month</span>
                  </span>
                </li>
              ))}
            </ul>
          )}
        </Card>

        {data && data.firings.length > 0 && <Firings projectId={projectId} firings={data.firings} />}
      </PageBody>

      <Modal open={!!editing} onClose={() => setEditing(null)} title={editing?.id ? "Edit play" : "New play"}>
        {editing && (
          <PlayForm
            key={editing.id ?? editing.name}
            projectId={projectId}
            draft={editing}
            segments={segments ?? []}
            onDone={() => {
              setEditing(null);
              mutate();
            }}
          />
        )}
      </Modal>
    </>
  );
}

function Firings({ projectId, firings }: { projectId: string; firings: PlayFiring[] }) {
  return (
    <section>
      <MicroLabel className="mb-2">What your plays did</MicroLabel>
      <Card className="overflow-hidden">
        <ol className="divide-y divide-border">
          {firings.map((f) => (
            <li key={f.id} className="flex items-start gap-3 px-4 py-2.5 text-sm">
              <Badge tone={OUTCOME_TONE[f.outcome]} className="mt-0.5 shrink-0 capitalize">
                {f.outcome}
              </Badge>
              <div className="min-w-0 flex-1">
                <p className="text-pretty">
                  <Link
                    href={f.lead_id ? `/app/p/${projectId}/leads?lead=${f.lead_id}` : `/app/p/${projectId}/companies`}
                    className="font-medium hover:underline"
                  >
                    {f.domain ?? "A lead"}
                  </Link>{" "}
                  <span className="text-muted-foreground">{f.what}</span>
                </p>
                <p className="text-xs text-muted-foreground">
                  {f.play_name}
                  {f.note && ` · ${f.note}`}
                </p>
              </div>
              <span className="shrink-0 text-xs text-muted-foreground">{timeAgo(f.created_at)}</span>
            </li>
          ))}
        </ol>
      </Card>
    </section>
  );
}

function PlayForm({
  projectId,
  draft,
  segments,
  onDone,
}: {
  projectId: string;
  draft: Draft & { id?: string };
  segments: SegmentWithStats[];
  onDone: () => void;
}) {
  const [form, setForm] = useState(draft);
  const [busy, setBusy] = useState(false);
  const editing = !!draft.id;
  const actions = actionsFor(form.trigger);

  const setTrigger = (trigger: PlayTrigger) =>
    setForm((f) => ({ ...f, trigger, action: actionsFor(trigger).includes(f.action) ? f.action : actionsFor(trigger)[0] }));

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const body = { ...form, segment_id: form.action === "add_to_campaign" ? form.segment_id : "" };
      if (editing) await api(`/plays/${draft.id}`, { method: "PATCH", body });
      else await api(`/projects/${projectId}/plays`, { body });
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!draft.id || !confirm(`Delete "${draft.name}" and what it did?`)) return;
    setBusy(true);
    try {
      await api(`/plays/${draft.id}`, { method: "DELETE" });
      onDone();
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  return (
    <form onSubmit={save} className="flex flex-col gap-4">
      <Field label="Name">
        <Input value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} maxLength={80} required autoFocus={!editing} />
      </Field>
      <Field label="When" hint={editing ? "A different trigger or action is a new play." : undefined}>
        <Select value={form.trigger} onChange={(e) => setTrigger(e.target.value as PlayTrigger)} disabled={editing}>
          {Object.entries(TRIGGERS).map(([value, t]) => (
            <option key={value} value={value}>
              {t.label}
              {value === "score" ? "…" : ""}
            </option>
          ))}
        </Select>
      </Field>
      <Field
        label={form.trigger === "score" ? "Score" : "Only when the score is at least"}
        hint={form.trigger === "score" ? undefined : "0 lets every one through."}
      >
        <NumberInput value={form.min_score} onValueChange={(min_score) => setForm({ ...form, min_score })} min={0} max={100} />
      </Field>
      <Field label="Then">
        <Select value={form.action} onChange={(e) => setForm({ ...form, action: e.target.value as PlayAction })} disabled={editing}>
          {actions.map((a) => (
            <option key={a} value={a}>
              {ACTIONS[a]}
            </option>
          ))}
        </Select>
      </Field>
      {form.action === "add_to_campaign" && (
        <Field label="Campaign" hint="Companies that already have leads stay where they are.">
          <Select value={form.segment_id} onChange={(e) => setForm({ ...form, segment_id: e.target.value })} required>
            <option value="" disabled>
              Pick a campaign
            </option>
            {segments.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="At most a day" hint="The rest wait for the next day.">
          <NumberInput value={form.daily_cap} onValueChange={(daily_cap) => setForm({ ...form, daily_cap })} min={1} max={200} />
        </Field>
        {!editing && (
          <Field label="Also look back" hint="Act on what happened lately too.">
            <Select value={form.backfill_days} onChange={(e) => setForm({ ...form, backfill_days: Number(e.target.value) })}>
              <option value={0}>Only from now on</option>
              <option value={7}>The last 7 days</option>
              <option value={30}>The last 30 days</option>
            </Select>
          </Field>
        )}
      </div>
      <div className="mt-1 flex items-center gap-2">
        {editing && (
          <Button type="button" variant="ghost" className="text-danger" onClick={remove} disabled={busy}>
            Delete
          </Button>
        )}
        <span className="flex-1" />
        <Button type="button" variant="ghost" onClick={onDone}>
          Cancel
        </Button>
        <Button type="submit" loading={busy}>
          {editing ? "Save" : "Add play"}
        </Button>
      </div>
    </form>
  );
}
