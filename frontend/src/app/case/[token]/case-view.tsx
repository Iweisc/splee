"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { Check, CircleAlert, Hourglass, Printer, RotateCcw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input, NumberInput } from "@/components/ui/form";
import { Badge, Favicon, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import { annual, numbersOf, paybackMonths, total } from "@/lib/business-case";
import type { CaseNumbers, PublicBusinessCase } from "@/lib/types";
import { cn, dealMoney, timeAgo } from "@/lib/utils";
import { Frame } from "../../r/[token]/referral-page-view";

/** The champion's business case: what it's worth line by line, with numbers they can change to their own and print.
 * `preview` is the team looking at it, which doesn't count as a view and can't save numbers. */
export function CaseView({ token, preview }: { token: string; preview: boolean }) {
  const path = `/case-pages/${encodeURIComponent(token)}`;
  const { data, error, mutate } = useSWR<PublicBusinessCase>(path, fetcher, { revalidateOnFocus: false });
  // Reported from the browser, once per load, so link checkers that never run scripts don't count as the buyer.
  const reported = useRef(false);
  useEffect(() => {
    if (!data || reported.current || preview) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [data, path, preview]);

  if (error instanceof ApiError && error.status === 404) {
    return (
      <Frame>
        <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This page isn&apos;t here anymore</h1>
          <p className="text-sm text-muted-foreground">Ask whoever sent it for a new link.</p>
        </div>
      </Frame>
    );
  }
  if (!data) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }
  // The inputs start from the page once, so a case written again or numbers saved elsewhere start them over. Otherwise
  // the old numbers would sit against different lines. A reload that changes neither keeps what the buyer is typing.
  return (
    <Case key={JSON.stringify([data.business_case, data.numbers])} page={data} path={path} preview={preview} saved={() => mutate()} />
  );
}

const same = (a: CaseNumbers[], b: CaseNumbers[]) =>
  a.every((n, i) => n.quantity === b[i].quantity && n.unit_value === b[i].unit_value && n.count === b[i].count);

function Case({ page, path, preview, saved }: { page: PublicBusinessCase; path: string; preview: boolean; saved: () => void }) {
  const c = page.business_case;
  const theirs = numbersOf(c, page.numbers);
  const ours = numbersOf(c, null);
  const [numbers, setNumbers] = useState(theirs);
  const [name, setName] = useState(page.edited_by || page.first_name);
  const [saving, setSaving] = useState(false);
  const [failed, setFailed] = useState("");
  const currency = page.investment?.currency ?? "USD";
  const money = (n: number) => dealMoney(Math.round(n), currency);
  const yearly = total(c, numbers);
  const cost = page.investment?.amount ?? 0;
  const payback = paybackMonths(yearly, cost);
  const changed = !same(numbers, theirs);
  const them = page.company.trim() || page.first_name || "You";

  const set = (i: number, field: keyof CaseNumbers, value: number) =>
    setNumbers(numbers.map((n, j) => (j === i ? { ...n, [field]: Math.max(0, value) } : n)));
  const save = async () => {
    setSaving(true);
    setFailed("");
    try {
      await api(`${path}/numbers`, { body: { name, numbers, revision: page.revision } });
      saved();
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "Something went wrong");
    } finally {
      setSaving(false);
    }
  };

  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {page.from_domain && <Favicon domain={page.from_domain} size={20} />}
          {page.from_name}
        </span>
        <span aria-hidden>×</span>
        <span className="flex items-center gap-2 font-medium text-foreground">
          {page.company_domain && <Favicon domain={page.company_domain} size={20} />}
          {them}
        </span>
      </header>

      <h1 className="mt-8 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">{c.title}</h1>
      <p className="mt-4 max-w-prose text-lg leading-relaxed text-pretty text-muted-foreground">{c.summary}</p>

      <div className="mt-8 grid gap-3 sm:grid-cols-3">
        <Stat
          label="Worth a year"
          value={money(yearly)}
          hint={cost > 0 ? `${(yearly / cost).toFixed(1)}× the first year's cost` : "from the lines below"}
          strong
        />
        <Stat
          label="Pays for itself in"
          value={payback == null ? "–" : payback < 1 ? "under a month" : `${payback.toFixed(1)} months`}
          hint={`then ${money(yearly / 12)} a month`}
        />
        <Stat
          label="First year's cost"
          value={cost > 0 ? money(cost) : "–"}
          hint={page.investment?.from === "proposal" ? "from the proposal" : "the deal as it stands"}
        />
      </div>

      <section className="mt-10">
        <h2 className="font-semibold">The problem</h2>
        <p className="mt-2 leading-relaxed text-pretty">{c.problem}</p>
      </section>

      <section className="mt-10">
        <div className="flex flex-wrap items-end justify-between gap-2">
          <h2 className="font-semibold">Where the value comes from</h2>
          <p className="text-xs text-muted-foreground print:hidden">Change any number to yours. A year is 48 working weeks.</p>
        </div>
        <ol className="mt-3 flex flex-col divide-y divide-border rounded-2xl border border-border bg-card">
          {c.lines.map((l, i) => (
            <li key={i} className="flex flex-col gap-3 p-5 break-inside-avoid">
              <div className="flex flex-wrap items-baseline justify-between gap-x-4 gap-y-1">
                <p className="flex flex-wrap items-center gap-2 font-medium text-balance">
                  {l.label}
                  <Badge tone={l.basis === "said" ? "green" : "amber"}>{l.basis === "said" ? "You told us" : "Our estimate"}</Badge>
                </p>
                <p className="font-semibold tabular-nums">
                  {money(annual(l.per, numbers[i]))}
                  <span className="text-sm font-normal text-muted-foreground"> a year</span>
                </p>
              </div>
              <div className="flex flex-wrap items-center gap-x-2 gap-y-2 text-sm">
                <Num value={numbers[i].quantity} onChange={(v) => set(i, "quantity", v)} label={`${l.label}: ${l.unit}`} />
                <span>{l.unit} at</span>
                <Num
                  value={numbers[i].unit_value}
                  onChange={(v) => set(i, "unit_value", v)}
                  label={`${l.label}: value of each`}
                  prefix={currency.toUpperCase()}
                />
                <span>each, every {l.per}, for</span>
                <Num value={numbers[i].count} onChange={(v) => set(i, "count", v)} label={`${l.label}: ${l.count_unit}`} />
                <span>{l.count_unit}</span>
              </div>
              {l.source && <p className="text-sm text-pretty text-muted-foreground">{l.source}</p>}
            </li>
          ))}
        </ol>
        {!preview && (
          <div className="mt-3 flex flex-wrap items-center justify-end gap-3 print:hidden">
            {page.edited_at && !changed && (
              <p className="mr-auto flex items-center gap-1.5 text-sm text-brand-800">
                <Check className="size-4" /> Numbers from {page.edited_by || "you"}, {timeAgo(page.edited_at)}.
              </p>
            )}
            {failed && <p className="mr-auto text-sm text-danger">{failed}</p>}
            {!same(numbers, ours) && (
              <Button type="button" variant="ghost" size="sm" onClick={() => setNumbers(ours)}>
                <RotateCcw className="size-3.5" /> Our numbers
              </Button>
            )}
            {changed && (
              <>
                <Input
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  maxLength={120}
                  placeholder="Your name"
                  aria-label="Your name"
                  className="h-8 w-40 text-sm"
                />
                <Button type="button" size="sm" onClick={save} loading={saving} disabled={!name.trim()}>
                  Save my numbers
                </Button>
              </>
            )}
          </div>
        )}
      </section>

      {c.outcomes.length > 0 && (
        <section className="mt-10 break-inside-avoid">
          <h2 className="font-semibold">What else changes</h2>
          <ul className="mt-2 flex flex-col gap-1.5">
            {c.outcomes.map((o, i) => (
              <li key={i} className="flex items-start gap-2 text-pretty">
                <Check className="mt-1 size-4 shrink-0 text-brand-700" /> {o}
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="mt-10 flex items-start gap-3 rounded-2xl bg-amber-50 p-5 text-amber-900 break-inside-avoid">
        <Hourglass className="mt-0.5 size-5 shrink-0" />
        <div>
          <p className="font-semibold">Each month of waiting costs about {money(yearly / 12)}</p>
          {c.waiting && <p className="mt-1 text-sm text-pretty">{c.waiting}</p>}
        </div>
      </section>

      <div className="mt-8 flex justify-end print:hidden">
        <Button type="button" variant="outline" onClick={() => window.print()}>
          <Printer className="size-4" /> Print or save as PDF
        </Button>
      </div>
      {page.sender_name && <p className="mt-6 text-sm text-muted-foreground">Prepared by {page.sender_name}.</p>}
    </Frame>
  );
}

function Stat({ label, value, hint, strong }: { label: string; value: string; hint: string; strong?: boolean }) {
  return (
    <div
      className={cn(
        "rounded-2xl border p-5 break-inside-avoid",
        strong ? "border-transparent bg-primary text-primary-foreground" : "border-border bg-card",
      )}
    >
      <p className={cn("text-sm", strong ? "opacity-75" : "text-muted-foreground")}>{label}</p>
      <p className="mt-1 text-2xl font-semibold tracking-tight tabular-nums">{value}</p>
      <p className={cn("mt-0.5 text-xs", strong ? "opacity-75" : "text-muted-foreground")}>{hint}</p>
    </div>
  );
}

/** A number in a line, as wide as it needs. On paper it prints as plain text. */
function Num({ value, onChange, label, prefix }: { value: number; onChange: (v: number) => void; label: string; prefix?: string }) {
  return (
    <span className="inline-flex items-center gap-1">
      {prefix && <span className="text-xs text-muted-foreground">{prefix}</span>}
      <NumberInput
        value={value}
        onValueChange={onChange}
        min={0}
        step="any"
        aria-label={label}
        className="h-8 w-auto min-w-14 px-2 text-sm font-medium tabular-nums field-sizing-content print:border-0 print:p-0"
      />
    </span>
  );
}
