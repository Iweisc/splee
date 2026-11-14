"use client";

import { useState } from "react";
import { Download, Pencil } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { NumberInput, Select } from "@/components/ui/form";
import { Badge, Card, Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import { downloadCsv, toCsv } from "@/lib/csv";
import type { Commissions } from "@/lib/types";
import { dealMoney } from "@/lib/utils";
import { toastError } from "./states";

type Row = Commissions["rows"][number];

const percent = (bps: number) => `${(bps / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}%`;
const times = (pct: number) => `${(pct / 100).toLocaleString("en-US", { maximumFractionDigits: 2 })}×`;
/** This month and the 11 before it, as 2026-10, newest first. */
const months = () =>
  Array.from({ length: 12 }, (_, i) => {
    const d = new Date();
    d.setUTCDate(1);
    d.setUTCMonth(d.getUTCMonth() - i);
    return d.toISOString().slice(0, 7);
  });
const monthName = (m: string) => new Date(`${m}-01T00:00:00Z`).toLocaleString("en-US", { month: "long", year: "numeric", timeZone: "UTC" });

/** On the Targets tab: what each teammate earned on the month's wins, with each one's statement. Owners and admins see
 * everyone's and set the plan, everyone else sees only their own. */
export function CommissionsCard({ admin }: { admin: boolean }) {
  const [month, setMonth] = useState(months()[0]);
  const { data: v, mutate } = useSWR<Commissions>(`/commissions${qs({ month })}`, fetcher, { keepPreviousData: true });
  const [editing, setEditing] = useState(false);
  const [opened, setOpened] = useState<string | null>(null);
  if (!v) return null;
  const anyRate = v.rows.some((r) => r.rate_bps !== null);
  // Members without a plan have nothing to see.
  if (!admin && !anyRate) return null;
  const money = (cents: number) => dealMoney(cents / 100, v.currency.toUpperCase());
  const open = v.rows.find((r) => r.user_id === opened);

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-3 px-4 pt-4 pb-3">
        <div className="min-w-0 flex-1 basis-64">
          <p className="text-sm font-semibold">Commissions</p>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            {v.commission_bps !== null
              ? `${percent(v.commission_bps)} of what's won${v.accelerator_pct > 100 ? `, and ${times(v.accelerator_pct)} that past the target` : ""}.`
              : anyRate
                ? "Only teammates with their own rate earn commission."
                : "No commission plan yet."}
          </p>
        </div>
        <div className="flex items-center gap-2">
          <Select value={month} onChange={(e) => setMonth(e.target.value)} className="h-9 w-auto" aria-label="Month">
            {months().map((m) => (
              <option key={m} value={m}>
                {monthName(m)}
              </option>
            ))}
          </Select>
          {admin && !editing && (
            <Button type="button" size="sm" variant="outline" onClick={() => setEditing(true)}>
              <Pencil className="size-3.5" /> {v.commission_bps !== null ? "Edit plan" : "Set a plan"}
            </Button>
          )}
        </div>
      </div>
      {editing && (
        <PlanForm
          v={v}
          onDone={async (saved) => {
            if (saved) await mutate();
            setEditing(false);
          }}
        />
      )}
      {(admin || anyRate) && (
        <ul className="divide-y divide-border border-t border-border">
          {v.rows.map((r) => (
            <li key={r.user_id}>
              <button
                type="button"
                onClick={() => setOpened(r.user_id)}
                className="flex w-full flex-wrap items-baseline gap-x-3 gap-y-0.5 px-4 py-3 text-left text-sm hover:bg-black/[0.03]"
              >
                <span className="font-medium">{r.name}</span>
                <span className="text-xs text-muted-foreground">
                  {r.rate_bps === null ? "No rate" : `${percent(r.rate_bps)}${r.own_rate ? ", their own" : ""}`}
                </span>
                <span className="ml-auto text-muted-foreground tabular-nums">{money(r.won_cents)} won</span>
                <span className="w-24 text-right font-semibold tabular-nums">{money(r.earned_cents)}</span>
              </button>
            </li>
          ))}
        </ul>
      )}
      {open && <Statement r={open} v={v} admin={admin} onClose={() => setOpened(null)} onSaved={() => mutate()} />}
    </Card>
  );
}

function PlanForm({ v, onDone }: { v: Commissions; onDone: (saved: boolean) => unknown }) {
  const [rate, setRate] = useState((v.commission_bps ?? 0) / 100);
  const [accel, setAccel] = useState(v.accelerator_pct / 100);
  const [saving, setSaving] = useState(false);
  const save = async () => {
    setSaving(true);
    try {
      await api("/commissions/plan", {
        method: "PUT",
        body: { commission_bps: rate > 0 ? Math.round(rate * 100) : null, accelerator_pct: Math.round(accel * 100) },
      });
      await onDone(true);
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
      <label className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Rate, % of what&apos;s won</span>
        <NumberInput min={0} max={100} step="any" value={rate} onValueChange={setRate} className="h-9 w-28" autoFocus />
      </label>
      <label className="flex flex-col gap-1">
        <span className="text-xs text-muted-foreground">Past the target, × the rate</span>
        <NumberInput min={1} max={5} step="any" value={accel} onValueChange={setAccel} className="h-9 w-28" />
      </label>
      <div className="flex gap-2">
        <Button type="submit" size="sm" loading={saving}>
          Save
        </Button>
        <Button type="button" size="sm" variant="ghost" onClick={() => onDone(false)}>
          Cancel
        </Button>
      </div>
      <p className="basis-full text-xs text-muted-foreground">0% turns commissions off. A teammate can have their own rate from their statement.</p>
    </form>
  );
}

/** A teammate's month: each won deal with what it earned, the total, a CSV for payroll and, for admins, their own rate. */
function Statement({ r, v, admin, onClose, onSaved }: { r: Row; v: Commissions; admin: boolean; onClose: () => void; onSaved: () => unknown }) {
  const money = (cents: number) => dealMoney(cents / 100, v.currency.toUpperCase());
  const month = v.month.slice(0, 7);
  const csv = () =>
    downloadCsv(
      `commission-${r.name.toLowerCase().replace(/\W+/g, "-")}-${month}.csv`,
      toCsv(
        ["Closed", "Company", "Contact", `Value (${v.currency.toUpperCase()})`, `Commission (${v.currency.toUpperCase()})`, "Past target"],
        r.deals.map((d) => [d.closed_at.slice(0, 10), d.company, d.name, d.value_cents / 100, d.earned_cents / 100, d.accelerated ? "yes" : "no"]),
      ),
    );
  return (
    <Modal open onClose={onClose} className="max-w-xl" title={`${r.name}, ${monthName(month)}`}>
      <div className="flex flex-col gap-4">
        <div className="grid grid-cols-3 gap-3 text-sm">
          {[
            ["Won", money(r.won_cents)],
            ["Target", r.target_cents ? money(r.target_cents) : "None"],
            ["Earned", money(r.earned_cents)],
          ].map(([label, value]) => (
            <div key={label} className="rounded-xl bg-muted/60 p-3">
              <p className="text-xs text-muted-foreground">{label}</p>
              <p className="mt-0.5 text-lg font-semibold tabular-nums">{value}</p>
            </div>
          ))}
        </div>
        {r.deals.length ? (
          <ul className="flex flex-col divide-y divide-border text-sm">
            {r.deals.map((d) => (
              <li key={d.deal_id} className="flex flex-wrap items-baseline gap-x-3 gap-y-0.5 py-2">
                <span className="w-14 text-xs text-muted-foreground tabular-nums">
                  {new Date(d.closed_at).toLocaleDateString("en-US", { month: "short", day: "numeric", timeZone: "UTC" })}
                </span>
                <span className="font-medium">{d.company || d.name}</span>
                {d.accelerated && <Badge tone="green">Past target</Badge>}
                <span className="ml-auto text-muted-foreground tabular-nums">{money(d.value_cents)}</span>
                <span className="w-20 text-right font-medium tabular-nums">{money(d.earned_cents)}</span>
              </li>
            ))}
          </ul>
        ) : (
          <p className="text-sm text-muted-foreground">Nothing won in {monthName(month)}.</p>
        )}
        {admin && <OwnRate key={`${r.user_id}-${r.rate_bps}`} r={r} v={v} onSaved={onSaved} />}
        <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border pt-4">
          <p className="mr-auto text-xs text-pretty text-muted-foreground">
            Deals they won, in {v.currency.toUpperCase()}, at today&apos;s rate and target.
          </p>
          <Button type="button" size="sm" variant="outline" onClick={csv} disabled={!r.deals.length}>
            <Download className="size-3.5" /> Download CSV
          </Button>
        </div>
      </div>
    </Modal>
  );
}

function OwnRate({ r, v, onSaved }: { r: Row; v: Commissions; onSaved: () => unknown }) {
  const [own, setOwn] = useState(r.own_rate);
  const [rate, setRate] = useState((r.rate_bps ?? v.commission_bps ?? 0) / 100);
  const [saving, setSaving] = useState(false);
  const save = async (bps: number | null) => {
    setSaving(true);
    try {
      await api(`/commissions/${r.user_id}`, { method: "PUT", body: { commission_bps: bps } });
      await onSaved();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <label className="flex items-center gap-2">
        <input
          type="checkbox"
          checked={own}
          onChange={(e) => {
            setOwn(e.target.checked);
            if (!e.target.checked && r.own_rate) save(null);
          }}
        />
        Their own rate
      </label>
      {own && (
        <>
          <NumberInput min={0} max={100} step="any" value={rate} onValueChange={setRate} className="h-8 w-24" aria-label="Their rate in %" />
          <span className="text-muted-foreground">%</span>
          <Button type="button" size="sm" onClick={() => save(Math.round(rate * 100))} loading={saving}>
            Save
          </Button>
        </>
      )}
      {!own && (
        <span className="text-muted-foreground">{v.commission_bps !== null ? `the team's ${percent(v.commission_bps)}` : "none, like the team"}</span>
      )}
    </div>
  );
}
