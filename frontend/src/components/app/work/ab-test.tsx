"use client";

import { useState } from "react";
import { Plus, Trophy, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Switch, Textarea } from "@/components/ui/form";
import { Badge, Card, Skeleton } from "@/components/ui/misc";
import { useLeaveGuard } from "@/hooks/use-leave-guard";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { AbTest, VariantStats } from "@/lib/types";
import { cn, dateTime, num } from "@/lib/utils";
import { MicroLabel, Td, Th, toastError } from "./states";

/** The backend's limits: up to 4 variants, and auto-pick waits for at least 30 sends each. */
const MAX_VARIANTS = 4;
const MIN_PICK_AFTER = 30;

type Draft = { id?: string; name: string; instructions: string };

/** A campaign's A/B test of email angles: its variants, their results and auto-pick. */
export function AbTestCard({ segmentId }: { segmentId: string }) {
  const key = `/segments/${segmentId}/variants`;
  const { data, mutate } = useSWR<AbTest>(key, fetcher);
  const [editing, setEditing] = useState(false);

  if (!data) return <Skeleton className="h-40 rounded-2xl" />;

  const save = async (body: { variants: Draft[]; pick_after: number | null }) => {
    await mutate(api<AbTest>(key, { method: "PUT", body }), { revalidate: false });
    setEditing(false);
    toast.success("Variants saved");
  };

  if (editing) return <Editor test={data} onSave={save} onCancel={() => setEditing(false)} />;

  if (data.variants.length === 0) {
    return (
      <Card className="flex flex-col gap-3 p-5 sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <MicroLabel>A/B test</MicroLabel>
          <p className="mt-1 text-sm text-muted-foreground">
            Try up to four angles: what the email leads with, how it asks for a reply, and the style of its subject.
          </p>
        </div>
        <Button variant="outline" size="sm" className="shrink-0" onClick={() => setEditing(true)}>
          Add variants
        </Button>
      </Card>
    );
  }

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <MicroLabel>A/B test</MicroLabel>
          <p className="mt-1 text-sm text-muted-foreground">
            {data.picked_at
              ? "New leads only get the variant it kept."
              : data.pick_after
                ? `Keeps the winner after ${num(data.pick_after)} sends per variant.`
                : "Each new lead gets the next variant in turn."}
          </p>
        </div>
        <Button variant="outline" size="sm" className="shrink-0" onClick={() => setEditing(true)}>
          Edit variants
        </Button>
      </div>
      {data.picked_at && <Picked test={data} segmentId={segmentId} onChange={(t) => mutate(t, { revalidate: false })} />}
      <Results test={data} />
      {data.variants.length > 1 && !data.picked_at && (
        <p className={cn("text-sm", data.verdict === "winner" ? "font-medium" : "text-muted-foreground")}>{data.note}</p>
      )}
    </Card>
  );
}

function Picked({ test, segmentId, onChange }: { test: AbTest; segmentId: string; onChange: (t: AbTest) => void }) {
  const [busy, setBusy] = useState(false);
  const winner = test.variants.find((v) => v.id === test.winner_id);
  const resume = async () => {
    setBusy(true);
    try {
      onChange(await api<AbTest>(`/segments/${segmentId}/variants/resume`, { method: "POST" }));
      toast.success("Every variant gets new leads again");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex flex-col gap-3 rounded-xl bg-brand-50 p-4 text-sm sm:flex-row sm:items-start sm:justify-between">
      <div className="min-w-0">
        <p className="flex items-center gap-2 font-medium text-brand-700">
          <Trophy className="size-4 shrink-0" />
          Kept {winner?.name ?? "the winner"} on {dateTime(test.picked_at!)}
        </p>
        <p className="mt-1 text-foreground/80">{test.pick_note}</p>
      </div>
      <Button variant="outline" size="sm" className="shrink-0 self-start bg-card" loading={busy} onClick={resume}>
        Test them all again
      </Button>
    </div>
  );
}

/** Always one decimal, to match the rates in the auto-pick note. */
function rate(hits: number, sent: number) {
  return sent > 0 ? `${((hits / sent) * 100).toFixed(1)}%` : "—";
}

function VariantName({ v, test }: { v: VariantStats; test: AbTest }) {
  const tag =
    v.id === test.winner_id ? (
      <Badge tone="green">Kept</Badge>
    ) : v.paused_at ? (
      <Badge>Paused</Badge>
    ) : v.id === test.leader_id && test.verdict === "winner" ? (
      <Badge tone="green">Winner</Badge>
    ) : v.id === test.leader_id && test.verdict === "unclear" ? (
      <Badge tone="amber">Leading</Badge>
    ) : null;
  return (
    <span className="flex flex-wrap items-center gap-x-2 gap-y-1">
      <span className="font-medium">{v.name}</span>
      {tag}
    </span>
  );
}

const COLUMNS: { label: string; title?: string; value: (v: VariantStats) => string }[] = [
  { label: "Sent", title: "Leads whose first email of this variant went out", value: (v) => num(v.sent) },
  { label: "Replies", value: (v) => num(v.replies) },
  { label: "Reply rate", value: (v) => rate(v.replies, v.sent) },
  { label: "Hot", title: "Leads who turned hot or booked a meeting", value: (v) => num(v.hot) },
  { label: "Hot rate", value: (v) => rate(v.hot, v.sent) },
  { label: "Bounced", value: (v) => num(v.bounced) },
  { label: "Unsubscribed", value: (v) => num(v.unsubscribed) },
];

/** On a phone, five columns with each rate under its count, since eight columns can't fit and seven left one alone on a row. */
const PHONE: { label: string; value: (v: VariantStats) => string; rate?: (v: VariantStats) => string }[] = [
  { label: "Sent", value: (v) => num(v.sent) },
  { label: "Replies", value: (v) => num(v.replies), rate: (v) => rate(v.replies, v.sent) },
  { label: "Hot", value: (v) => num(v.hot), rate: (v) => rate(v.hot, v.sent) },
  { label: "Bounced", value: (v) => num(v.bounced) },
  { label: "Unsub.", value: (v) => num(v.unsubscribed) },
];

/** A table from sm up, and a block of numbers per variant on a phone. */
function Results({ test }: { test: AbTest }) {
  return (
    <>
      <div className="-mx-5 overflow-x-auto max-sm:hidden">
        <table className="w-full text-sm">
          <thead>
            <tr>
              <Th className="pl-5">Variant</Th>
              {COLUMNS.map((c) => (
                <Th key={c.label} title={c.title} className="text-right whitespace-nowrap last:pr-5">
                  {c.label}
                </Th>
              ))}
            </tr>
          </thead>
          <tbody>
            {test.variants.map((v) => (
              <tr key={v.id} className="border-t border-border">
                <Td className="pl-5">
                  <VariantName v={v} test={test} />
                  {v.instructions && <p className="line-clamp-1 text-xs text-muted-foreground">{v.instructions}</p>}
                </Td>
                {COLUMNS.map((c) => (
                  <Td key={c.label} className="text-right whitespace-nowrap tabular-nums last:pr-5">
                    {c.value(v)}
                  </Td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div className="flex flex-col divide-y divide-border sm:hidden">
        {test.variants.map((v) => (
          <div key={v.id} className="py-3 first:pt-0 last:pb-0">
            <VariantName v={v} test={test} />
            <dl className="mt-2 grid grid-cols-5 gap-x-2">
              {PHONE.map((c) => (
                <div key={c.label} className="min-w-0">
                  <dt className="truncate text-xs text-muted-foreground">{c.label}</dt>
                  <dd className="text-sm tabular-nums">{c.value(v)}</dd>
                  {c.rate && <dd className="text-xs text-muted-foreground tabular-nums">{c.rate(v)}</dd>}
                </div>
              ))}
            </dl>
          </div>
        ))}
      </div>
    </>
  );
}

function Editor({
  test,
  onSave,
  onCancel,
}: {
  test: AbTest;
  onSave: (body: { variants: Draft[]; pick_after: number | null }) => Promise<void>;
  onCancel: () => void;
}) {
  const initial: Draft[] = test.variants.map((v) => ({ id: v.id, name: v.name, instructions: v.instructions }));
  const [drafts, setDrafts] = useState<Draft[]>(initial.length ? initial : [blank(), blank()]);
  const [autoPick, setAutoPick] = useState(test.pick_after !== null);
  const [pickAfter, setPickAfter] = useState(test.pick_after ?? 100);
  const [saving, setSaving] = useState(false);
  const pick = autoPick && drafts.length > 1 ? pickAfter : null;
  useLeaveGuard(JSON.stringify([drafts, pick]) !== JSON.stringify([initial, test.pick_after]));

  const set = (i: number, patch: Partial<Draft>) => setDrafts(drafts.map((d, j) => (j === i ? { ...d, ...patch } : d)));
  const remove = (i: number) => {
    const sent = test.variants.find((v) => v.id === drafts[i].id)?.sent ?? 0;
    if (sent > 0 && !confirm(`Remove "${drafts[i].name}"? Its results go with it. Emails already written stay as they are.`)) return;
    setDrafts(drafts.filter((_, j) => j !== i));
  };
  const canPick = drafts.length > 1;
  const valid = drafts.every((d) => d.name.trim()) && (!autoPick || !canPick || pickAfter >= MIN_PICK_AFTER);

  const save = async () => {
    setSaving(true);
    try {
      await onSave({ variants: drafts, pick_after: pick });
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div>
        <MicroLabel>A/B test</MicroLabel>
        <p className="mt-1 text-sm text-muted-foreground">
          Tell the writer what each variant leads with, how it asks for a reply and how its subject reads. Each new lead gets the
          next variant in turn and keeps it for follow-ups. Emails already written don&apos;t change.
        </p>
      </div>
      {drafts.map((d, i) => (
        <div key={d.id ?? `new-${i}`} className="flex flex-col gap-3 rounded-xl border border-border p-4">
          <div className="flex items-end gap-2">
            <Field label={`Variant ${i + 1}`} className="min-w-0 flex-1">
              <Input value={d.name} maxLength={40} onChange={(e) => set(i, { name: e.target.value })} placeholder={PLACEHOLDERS[i % 4].name} />
            </Field>
            <Button variant="ghost" size="icon" className="size-10 shrink-0" onClick={() => remove(i)} aria-label={`Remove variant ${i + 1}`}>
              <X className="size-4" />
            </Button>
          </div>
          <Field label="Instructions for the writer">
            <Textarea
              value={d.instructions}
              maxLength={1000}
              onChange={(e) => set(i, { instructions: e.target.value })}
              placeholder={PLACEHOLDERS[i % 4].instructions}
              className="min-h-16 field-sizing-content"
            />
          </Field>
        </div>
      ))}
      {drafts.length === 0 && <p className="text-sm text-muted-foreground">No variants. Saving turns the test off, and emails are written as before.</p>}
      {drafts.length < MAX_VARIANTS && (
        <Button variant="outline" size="sm" className="self-start" onClick={() => setDrafts([...drafts, blank()])}>
          <Plus className="size-4" />
          Add variant
        </Button>
      )}
      {canPick && (
        <div className="flex flex-col gap-2 border-t border-border pt-4">
          <label className="flex items-center gap-3 text-sm font-medium">
            <Switch checked={autoPick} onChange={setAutoPick} label="Keep the winner automatically" />
            Keep the winner automatically
          </label>
          {autoPick && (
            <div className="flex flex-wrap items-center gap-2 text-sm">
              <span>After</span>
              <NumberInput
                min={MIN_PICK_AFTER}
                max={10000}
                value={pickAfter}
                onValueChange={(n) => setPickAfter(Math.round(n))}
                className="h-8 w-20 px-2 tabular-nums"
                aria-label="Sends per variant"
              />
              <span>sends per variant</span>
            </div>
          )}
          <p className="text-xs text-muted-foreground">
            {autoPick
              ? pickAfter < MIN_PICK_AFTER
                ? `Pick at least ${MIN_PICK_AFTER} sends per variant.`
                : "When one variant clearly beats the rest on hot rate, the others stop getting new leads."
              : "Off: every variant keeps getting new leads until you change them."}
          </p>
        </div>
      )}
      <div className="flex justify-end gap-2">
        <Button variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button onClick={save} loading={saving} disabled={!valid}>
          Save variants
        </Button>
      </div>
    </Card>
  );
}

const PLACEHOLDERS = [
  { name: "Lost revenue", instructions: "Open with the bookings they likely miss after hours. Ask if a two-minute demo is worth it. Subject as a short question." },
  { name: "Social proof", instructions: "Lead with a result from a similar business in our materials. Offer a case study. Subject names their company." },
  { name: "Quick tip", instructions: "Open with one free, specific tip for their site. Ask if they want two more. Plain lowercase subject, three words." },
  { name: "Direct ask", instructions: "Say what we do in one line and ask for 15 minutes this week. Subject: their first name and ours." },
];

function blank(): Draft {
  return { name: "", instructions: "" };
}
