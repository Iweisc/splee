"use client";

import { useEffect, useState } from "react";
import { Download, Search } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { AgentRun, ExplorerCompany } from "@/lib/types";
import { flag } from "./flag";

const POLL_MS = 2000;

interface ExplorerResult {
  run: AgentRun;
  results: ExplorerCompany[];
}

function toCsv(rows: ExplorerCompany[]): string {
  const header = ["name", "domain", "city", "country_code", "employees", "description", "fit_reason"] as const;
  const cell = (v: string) => (/[",\n]/.test(v) ? `"${v.replace(/"/g, '""')}"` : v);
  return [header.join(","), ...rows.map((r) => header.map((h) => cell(r[h] ?? "")).join(","))].join("\n");
}

function downloadCsv(rows: ExplorerCompany[], query: string) {
  const url = URL.createObjectURL(new Blob([toCsv(rows)], { type: "text/csv;charset=utf-8" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = `splee-${query.toLowerCase().replace(/[^a-z0-9]+/g, "-").slice(0, 40) || "companies"}.csv`;
  a.click();
  URL.revokeObjectURL(url);
}

function statusLine(query: string, data: ExplorerResult | null): string {
  if (!data || data.run.status === "queued") return "Waiting for a free agent…";
  const n = data.results.length;
  if (data.run.status === "running") return `Searching the web for "${query}"… ${n ? `${n} companies so far` : ""}`;
  if (data.run.status === "succeeded") return `Found ${n} ${n === 1 ? "company" : "companies"}`;
  return data.run.error || "The search stopped before it finished.";
}

/** Segments Explorer: describe companies, the agent searches the web live, results stream in by polling. */
export function Explorer({ defaultCountry = "", placeholder = "e.g. wholesale coffee roasters" }: { defaultCountry?: string; placeholder?: string }) {
  const [query, setQuery] = useState("");
  const [country, setCountry] = useState(defaultCountry);
  const [runId, setRunId] = useState<string | null>(null);
  const [submitted, setSubmitted] = useState("");
  const [data, setData] = useState<ExplorerResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const done = !!data && !["queued", "running"].includes(data.run.status);

  useEffect(() => {
    if (!runId || done) return;
    let cancelled = false;
    let timer: ReturnType<typeof setTimeout>;
    const poll = async () => {
      try {
        const res = await api<ExplorerResult>(`/explorer/${runId}`);
        if (cancelled) return;
        setData(res);
        setError(null);
      } catch (e) {
        if (!cancelled) setError(e instanceof Error ? e.message : "Lost connection, retrying…");
      }
      if (!cancelled) timer = setTimeout(poll, POLL_MS);
    };
    poll();
    return () => {
      cancelled = true;
      clearTimeout(timer);
    };
  }, [runId, done]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const q = query.trim();
    if (!q) return;
    setStarting(true);
    setError(null);
    setData(null);
    setRunId(null);
    try {
      const res = await api<{ run_id: string }>("/explorer", { body: { query: q, country: country.trim() || undefined } });
      setSubmitted(q);
      setRunId(res.run_id);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Couldn't start the search.");
    } finally {
      setStarting(false);
    }
  };

  const results = data?.results ?? [];
  const running = !!runId && !done;

  return (
    <div className="flex flex-col gap-4">
      <form onSubmit={submit} className="flex flex-col gap-2 rounded-2xl bg-card p-2 shadow-plaque sm:flex-row">
        <div className="flex flex-1 items-center gap-2 pl-3">
          <Search className="size-4 shrink-0 text-muted-foreground" />
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder={placeholder}
            aria-label="Describe the companies you want"
            className="h-10 min-w-0 flex-1 bg-transparent outline-none placeholder:text-muted-foreground"
          />
        </div>
        <Input value={country} onChange={(e) => setCountry(e.target.value)} placeholder="Any country" aria-label="Country or region" className="sm:w-44" />
        <Button type="submit" loading={starting || running} disabled={!query.trim()}>
          Search
        </Button>
      </form>

      {error && <p className="text-sm text-danger">{error}</p>}

      {runId && (
        <Card className="overflow-hidden">
          <div className="flex items-center justify-between gap-4 border-b border-border px-4 py-3">
            <p className="truncate font-mono text-[13px]">
              <span className={running ? "text-brand-600" : "text-muted-foreground"}>{running ? "›" : "✓"}</span> {statusLine(submitted, data)}
              {running && <span className="ml-0.5 inline-block h-3.5 w-[7px] translate-y-0.5 animate-blink bg-foreground" />}
            </p>
            <Button variant="outline" size="sm" disabled={results.length === 0} onClick={() => downloadCsv(results, submitted)}>
              <Download className="size-3.5" />
              Export CSV
            </Button>
          </div>
          {results.length > 0 ? (
            <div className="overflow-x-auto">
              <table className="w-full text-left text-sm">
                <thead className="bg-accent text-xs tracking-wide text-muted-foreground uppercase">
                  <tr>
                    <th className="px-4 py-2.5 font-medium">Company</th>
                    <th className="px-4 py-2.5 font-medium">Location</th>
                    <th className="px-4 py-2.5 font-medium">Employees</th>
                    <th className="px-4 py-2.5 font-medium">Why it fits</th>
                  </tr>
                </thead>
                <tbody>
                  {results.map((c) => (
                    <tr key={c.domain} className="border-t border-border align-top">
                      <td className="px-4 py-3">
                        <div className="flex items-center gap-2.5">
                          <Favicon domain={c.domain} size={18} />
                          <div className="min-w-0">
                            <p className="font-medium">{c.name}</p>
                            <a href={`https://${c.domain}`} target="_blank" rel="noreferrer" className="text-muted-foreground hover:underline">
                              {c.domain}
                            </a>
                          </div>
                        </div>
                      </td>
                      <td className="px-4 py-3 whitespace-nowrap text-muted-foreground">
                        {flag(c.country_code)} {c.city || c.country_code.toUpperCase()}
                      </td>
                      <td className="px-4 py-3 whitespace-nowrap text-muted-foreground">{c.employees || "–"}</td>
                      <td className="max-w-md px-4 py-3 text-muted-foreground">{c.fit_reason}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : running ? (
            <div className="flex flex-col gap-3 p-4">
              {Array.from({ length: 5 }, (_, i) => (
                <div key={i} className="flex items-center gap-3">
                  <Skeleton className="size-5 rounded-full" />
                  <Skeleton className="h-4 w-40" />
                  <Skeleton className="h-4 flex-1" />
                </div>
              ))}
            </div>
          ) : (
            <EmptyState title="No companies found">Try a broader description or remove the country.</EmptyState>
          )}
        </Card>
      )}
    </div>
  );
}
