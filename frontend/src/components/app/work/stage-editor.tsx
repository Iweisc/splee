"use client";

import { useState } from "react";
import { ArrowDown, ArrowUp, Plus, Trash2, Undo2 } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput, Select } from "@/components/ui/form";
import { Modal } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { DealStage, Pipeline, StageKind } from "@/lib/types";
import { plural } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";
import { CURRENCIES } from "./deal-dialog";

// Keep in sync with deals::MAX_STAGES in backend/src/deals.rs.
const MAX_STAGES = 10;

type Row = { key: string; id?: string; name: string; kind: StageKind; probability: number; auto: DealStage["auto"]; deals: number };
type Removed = Row & { id: string; moveTo: string };

const AUTO_HINT = { hot: "Hot replies open a deal here", booked: "Booked meetings move deals here" };

/** Renames, reorders, adds and removes stages, and sets the currency, in one save. */
export function StageEditor({ pipeline, onClose, onSaved }: { pipeline: Pipeline; onClose: () => void; onSaved: (p: Pipeline) => void }) {
  const [rows, setRows] = useState<Row[]>(() => pipeline.stages.map((s) => ({ ...s, key: s.id })));
  const [removed, setRemoved] = useState<Removed[]>([]);
  const [currency, setCurrency] = useState(pipeline.currency);
  const [saving, setSaving] = useState(false);
  const [fresh, setFresh] = useState<string | null>(null);
  const currencies = CURRENCIES.includes(currency) ? CURRENCIES : [currency, ...CURRENCIES];

  const edit = (key: string, changes: Partial<Row>) => setRows((rs) => rs.map((r) => (r.key === key ? { ...r, ...changes } : r)));
  const shift = (i: number, by: number) =>
    setRows((rs) => {
      const next = [...rs];
      [next[i], next[i + by]] = [next[i + by]!, next[i]!];
      return next;
    });
  const remove = (row: Row) => {
    setRows((rs) => rs.filter((r) => r.key !== row.key));
    if (row.id) setRemoved((rs) => [...rs, { ...row, id: row.id!, moveTo: "" }]);
  };
  const restore = (row: Removed) => {
    setRemoved((rs) => rs.filter((r) => r.id !== row.id));
    // Back where it was, as near as the rows left allow.
    const at = pipeline.stages.findIndex((s) => s.id === row.id);
    setRows((rs) => [...rs.slice(0, at), row, ...rs.slice(at)]);
  };
  // New stages go after the last open one, ahead of Won and Lost.
  const add = () => {
    const key = crypto.randomUUID();
    setFresh(key);
    setRows((rs) => {
      const at = rs.findLastIndex((r) => r.kind === "open") + 1;
      return [...rs.slice(0, at), { key, name: "", kind: "open", probability: 50, auto: null, deals: 0 }, ...rs.slice(at)];
    });
  };

  // A removed stage's deals go to a saved stage of the same kind that stays, so a won deal can't end up open.
  const targets = (kind: StageKind) => rows.filter((r) => r.id && r.kind === kind);
  const names = rows.map((r) => r.name.trim().toLowerCase());
  const problem =
    names.some((n) => !n) ? "Give every stage a name."
    : new Set(names).size < names.length ? "Two stages have the same name."
    : (["open", "won", "lost"] as const).find((k) => !rows.some((r) => r.kind === k)) ? "Keep at least one open, one won and one lost stage."
    : removed.some((r) => r.deals > 0 && !r.moveTo) ? "Pick where the deals of each removed stage go."
    : null;

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setSaving(true);
    try {
      const saved = await api<Pipeline>("/pipeline", {
        method: "PUT",
        body: {
          currency,
          stages: rows.map((r) => ({ id: r.id, name: r.name.trim(), kind: r.kind, probability: r.probability })),
          moves: removed.filter((r) => r.deals > 0).map((r) => ({ from: r.id, to: r.moveTo })),
        },
      });
      toast.success("Stages saved");
      onSaved(saved);
    } catch (err) {
      toastError(err);
      setSaving(false);
    }
  };

  return (
    <Modal open onClose={onClose} title="Pipeline stages" className="max-w-xl">
      <form onSubmit={save} className="flex flex-col gap-5">
        <Field label="Currency for new deals" hint="Deals you already have keep theirs." className="sm:max-w-56">
          <Select value={currency} onChange={(e) => setCurrency(e.target.value)}>
            {currencies.map((c) => (
              <option key={c}>{c}</option>
            ))}
          </Select>
        </Field>

        <div>
          <MicroLabel className="mb-2">Stages, in board order</MicroLabel>
          <ol className="flex flex-col gap-2">
            {rows.map((row, i) => {
              const locked = !!row.id && row.deals > 0;
              const note = [row.auto && row.kind === "open" && AUTO_HINT[row.auto], row.deals > 0 && plural(row.deals, "deal")].filter(Boolean).join(" · ");
              return (
                <li key={row.key} className="rounded-xl border border-border p-2">
                  <div className="flex flex-wrap items-center gap-2 sm:flex-nowrap">
                    <Input
                      value={row.name}
                      maxLength={40}
                      onChange={(e) => edit(row.key, { name: e.target.value })}
                      aria-label="Stage name"
                      placeholder="Stage name"
                      autoFocus={row.key === fresh}
                      className="h-9 min-w-0 basis-full sm:flex-1 sm:basis-auto"
                    />
                    <Select
                      value={row.kind}
                      onChange={(e) => edit(row.key, { kind: e.target.value as StageKind })}
                      disabled={locked}
                      title={locked ? "Move its deals out first" : undefined}
                      aria-label={`What ${row.name || "this stage"} means`}
                      className="h-9 w-[6.5rem] shrink-0"
                    >
                      <option value="open">Open</option>
                      <option value="won">Won</option>
                      <option value="lost">Lost</option>
                    </Select>
                    {row.kind === "open" ? (
                      <div className="relative w-[4.5rem] shrink-0" title="How likely a deal here is to close, for the weighted forecast">
                        <NumberInput
                          min={0}
                          max={100}
                          value={row.probability}
                          onValueChange={(v) => edit(row.key, { probability: Math.round(Math.min(100, Math.max(0, v))) })}
                          aria-label={`Chance a deal in ${row.name || "this stage"} closes`}
                          className="h-9 pr-6 tabular-nums"
                        />
                        <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 text-sm text-muted-foreground">%</span>
                      </div>
                    ) : (
                      <span className="w-[4.5rem] shrink-0 px-3 text-sm text-muted-foreground tabular-nums">{row.kind === "won" ? "100%" : "0%"}</span>
                    )}
                    <div className="ml-auto flex shrink-0">
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="size-8"
                        disabled={i === 0}
                        onClick={() => shift(i, -1)}
                        aria-label={`Move ${row.name} up`}
                      >
                        <ArrowUp className="size-4" />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="size-8"
                        disabled={i === rows.length - 1}
                        onClick={() => shift(i, 1)}
                        aria-label={`Move ${row.name} down`}
                      >
                        <ArrowDown className="size-4" />
                      </Button>
                      <Button
                        type="button"
                        variant="ghost"
                        size="icon"
                        className="size-8 text-danger"
                        onClick={() => remove(row)}
                        aria-label={`Remove ${row.name}`}
                      >
                        <Trash2 className="size-4" />
                      </Button>
                    </div>
                  </div>
                  {note && <p className="mt-1.5 px-1 text-xs text-muted-foreground">{note}</p>}
                </li>
              );
            })}
          </ol>
          <Button type="button" variant="outline" size="sm" className="mt-2" onClick={add} disabled={rows.length >= MAX_STAGES}>
            <Plus className="size-3.5" /> {rows.length >= MAX_STAGES ? `${MAX_STAGES} stages is the most` : "Add stage"}
          </Button>
        </div>

        {removed.length > 0 && (
          <div>
            <MicroLabel className="mb-2">Removed when you save</MicroLabel>
            <ul className="flex flex-col gap-2">
              {removed.map((r) => (
                <li key={r.id} className="flex flex-wrap items-center gap-2 rounded-xl bg-muted px-3 py-2 text-sm">
                  <span className="min-w-0 flex-1 basis-40">
                    <span className="font-medium">{r.name}</span>
                    {r.deals > 0 && <span className="text-muted-foreground">: move its {plural(r.deals, "deal")} to</span>}
                  </span>
                  {r.deals > 0 && (
                    <Select
                      value={r.moveTo}
                      onChange={(e) => setRemoved((rs) => rs.map((x) => (x.id === r.id ? { ...x, moveTo: e.target.value } : x)))}
                      aria-label={`Where the deals in ${r.name} go`}
                      className="h-8 w-auto bg-card"
                    >
                      <option value="" disabled>
                        Pick a stage
                      </option>
                      {targets(r.kind).map((t) => (
                        <option key={t.key} value={t.id}>
                          {t.name}
                        </option>
                      ))}
                    </Select>
                  )}
                  <Button type="button" variant="ghost" size="sm" onClick={() => restore(r)}>
                    <Undo2 className="size-3.5" /> Keep
                  </Button>
                </li>
              ))}
            </ul>
          </div>
        )}

        <div className="flex flex-wrap items-center justify-end gap-2">
          {problem && <p className="mr-auto text-xs text-danger">{problem}</p>}
          <Button type="button" variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" loading={saving} disabled={!!problem}>
            Save stages
          </Button>
        </div>
      </form>
    </Modal>
  );
}
