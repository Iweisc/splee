"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Badge, Card, type BadgeTone } from "@/components/ui/misc";
import { api } from "@/lib/api";
import { COUNTRY_CODES, countryName } from "@/lib/countries";
import type { DirectoryCountry, DirectoryImport } from "@/lib/types";
import { compact, num, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { MicroLabel, toastError } from "./states";

const active = (i: DirectoryImport) => i.status === "queued" || i.status === "running";

const STATUS: Record<DirectoryImport["status"], { tone: BadgeTone; label: string }> = {
  queued: { tone: "neutral", label: "Waiting" },
  running: { tone: "blue", label: "Loading" },
  done: { tone: "green", label: "Loaded" },
  failed: { tone: "red", label: "Failed" },
};

/** How many finished imports stay listed under the ones still going. */
const RECENT = 5;

const byName = (a: string, b: string) => countryName(a).localeCompare(countryName(b));

/** Only shown to instance operators. The company database is shared by every organization on the instance. */
export function CountryLoader({ countries, onLoaded }: { countries: DirectoryCountry[]; onLoaded: () => void }) {
  const [country, setCountry] = useState("");
  const [busy, setBusy] = useState(false);
  // Polled while a country waits or loads, which the backend's worker does one at a time.
  const { data: imports, mutate } = useSWR<DirectoryImport[]>("/directory/imports", fetcher, {
    refreshInterval: (list) => (list?.some(active) ? 2_000 : 0),
  });

  // A country that finishes loading shows up in the search filter and the list below without a reload.
  const going = imports?.filter(active).length;
  const wasGoing = useRef(going);
  useEffect(() => {
    if (going !== undefined && wasGoing.current !== undefined && going < wasGoing.current) onLoaded();
    wasGoing.current = going;
  }, [going, onLoaded]);

  const loaded = new Set(countries.map((c) => c.country_code));
  const pending = new Map(imports?.filter(active).map((i) => [i.country, i.status]));
  const notYet = COUNTRY_CODES.filter((c) => !loaded.has(c)).sort(byName);
  const again = COUNTRY_CODES.filter((c) => loaded.has(c)).sort(byName);
  const option = (code: string) => (
    <option key={code} value={code} disabled={pending.has(code)}>
      {countryName(code)} ({code}){pending.has(code) ? ` – ${STATUS[pending.get(code)!].label.toLowerCase()}` : ""}
    </option>
  );
  const shown = imports ? [...imports.filter(active), ...imports.filter((i) => !active(i)).slice(0, RECENT)] : [];

  const load = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!country) return;
    setBusy(true);
    try {
      await api<DirectoryImport>("/directory/imports", { body: { country } });
      toast.success(`${countryName(country)} is next in line. A small country takes minutes and a large one a few hours.`);
      setCountry("");
      await mutate();
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card className="mb-4 p-5">
      <div className="flex flex-wrap items-end justify-between gap-x-6 gap-y-3">
        <div className="min-w-0 flex-[1_1_22rem]">
          <h2 className="font-semibold">Load a country</h2>
          <p className="mt-0.5 text-sm text-pretty text-muted-foreground">
            Adds every business with a website from Overture Maps&apos; newest release. Every organization on this Splee can search what you load.
          </p>
        </div>
        <form onSubmit={load} className="flex w-full gap-2 sm:w-auto">
          <Select value={country} onChange={(e) => setCountry(e.target.value)} className="h-9 min-w-0 flex-1 sm:w-64" aria-label="Country to load">
            <option value="">Pick a country…</option>
            <optgroup label="Not loaded yet">{notYet.map(option)}</optgroup>
            {again.length > 0 && <optgroup label="Loaded, load again for newer data">{again.map(option)}</optgroup>}
          </Select>
          <Button type="submit" size="sm" className="h-9" disabled={!country} loading={busy}>
            Load
          </Button>
        </form>
      </div>

      {(shown.length > 0 || countries.length > 0) && (
        <div className="mt-4 divide-y divide-border border-t border-border">
          {shown.length > 0 && (
            <ul className="divide-y divide-border">
              {shown.map((i) => (
                <ImportRow key={i.id} item={i} />
              ))}
            </ul>
          )}
          {countries.length > 0 && (
            <div className="pt-4">
              <MicroLabel>Loaded</MicroLabel>
              <ul className="mt-2 flex flex-wrap gap-1.5">
                {countries.map((c) => (
                  <li key={c.country_code} className="rounded-full bg-black/5 px-2.5 py-1 text-xs whitespace-nowrap">
                    {countryName(c.country_code)} <span className="text-muted-foreground tabular-nums">{compact(c.companies)}</span>
                  </li>
                ))}
              </ul>
            </div>
          )}
        </div>
      )}
    </Card>
  );
}

function ImportRow({ item: i }: { item: DirectoryImport }) {
  const s = STATUS[i.status];
  const share = i.groups_total > 0 ? i.groups_done / i.groups_total : 0;
  const detail =
    i.status === "queued"
      ? "Starts when the country before it finishes"
      : i.status === "running"
        ? i.groups_total === 0
          ? "Finding the country in Overture's files"
          : `${num(i.groups_done)} of ${plural(i.groups_total, "part")} read, ${plural(i.rows_kept, "company", "companies")} so far`
        : i.status === "done"
          ? `${plural(i.rows_kept, "company", "companies")}, ${timeAgo(i.finished_at ?? i.created_at)}`
          : i.error;
  return (
    <li className="py-3 text-sm">
      <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
        <span className="font-medium">{countryName(i.country)}</span>
        <Badge tone={s.tone}>{s.label}</Badge>
        {i.requested_by && <span className="ml-auto truncate text-xs text-muted-foreground max-sm:hidden">{i.requested_by}</span>}
      </div>
      {i.status === "running" && (
        <div
          role="progressbar"
          aria-label={`Loading ${countryName(i.country)}`}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={Math.round(share * 100)}
          className="mt-2 h-1.5 overflow-hidden rounded-full bg-black/5"
        >
          <div className="h-full rounded-full bg-sky-500 transition-[width] duration-500" style={{ width: `${Math.max(share * 100, 2)}%` }} />
        </div>
      )}
      <p className={i.status === "failed" ? "mt-1 text-red-700" : "mt-1 text-muted-foreground"}>{detail}</p>
    </li>
  );
}
