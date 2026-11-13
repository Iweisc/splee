"use client";

import { useState } from "react";
import { Building2, Users, X } from "lucide-react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Input, Switch } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon, Host } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { FoundPeople, LookalikePart, Lookalikes, LookalikeSettings, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { cn, num, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { categoryWords, FoundPanel, PickedBar } from "./directory-view";
import { ListSkeleton, LoadError, MicroLabel, Td, Th, toastError, Tr } from "./states";

/** Companies in the database like the project's customers: its seeds, what they share with toggles, and the ranked matches. */
export function LookalikesView({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/lookalikes`;
  const { data, error, mutate } = useSWR<Lookalikes>(key, fetcher);
  const [saving, setSaving] = useState(false);

  const save = async (change: Partial<LookalikeSettings>) => {
    if (!data) return;
    setSaving(true);
    try {
      const next = await api<Lookalikes>(`${key}/settings`, { method: "PUT", body: { ...data.settings, ...change } });
      await mutate(next, { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  if (error) return <Card><LoadError error={error} retry={() => mutate()} what="your customers' lookalikes" /></Card>;
  if (!data) return <Card><ListSkeleton /></Card>;
  const { seeds, parts } = data.profile;
  return (
    <div className="flex flex-col gap-4">
      <Seeds data={data} saving={saving} save={save} />
      {seeds.length > 0 && <Profile parts={parts} saving={saving} save={save} off={data.settings.off} />}
      {data.database_empty ? (
        <Card className="px-4 py-3 text-sm text-pretty text-muted-foreground">
          The company database is empty, so there&apos;s nothing to match yet. Whoever runs this Splee can load a country on the Search tab.
        </Card>
      ) : (
        seeds.length > 0 && <Matches projectId={projectId} data={data} onAdded={() => mutate()} />
      )}
    </div>
  );
}

function Seeds({ data, saving, save }: { data: Lookalikes; saving: boolean; save: (change: Partial<LookalikeSettings>) => void }) {
  const [domain, setDomain] = useState("");
  const { seeds } = data.profile;
  const { added, removed } = data.settings;

  const add = (e: React.FormEvent) => {
    e.preventDefault();
    const d = domain.trim().toLowerCase();
    if (!d) return;
    save({ added: [...added, d], removed: removed.filter((r) => r !== d) });
    setDomain("");
  };
  const remove = (d: string) => save({ added: added.filter((a) => a !== d), removed: seeds.find((s) => s.domain === d)?.won ? [...removed, d] : removed });

  return (
    <Card>
      <div className="flex flex-wrap items-end justify-between gap-x-4 gap-y-3 border-b border-border p-4">
        <div className="min-w-0 flex-[999_1_16rem]">
          <h2 className="font-medium">Your customers</h2>
          <p className="text-sm text-pretty text-muted-foreground">The companies you won deals with, plus any you add here.</p>
        </div>
        <form onSubmit={add} className="flex flex-auto gap-2 sm:flex-none">
          <Input
            value={domain}
            onChange={(e) => setDomain(e.target.value)}
            placeholder="customer.com"
            aria-label="A customer's website"
            className="h-9 min-w-0 flex-1 sm:w-56"
          />
          <Button type="submit" size="sm" className="h-9" disabled={saving || !domain.trim()}>
            Add
          </Button>
        </form>
      </div>
      {seeds.length === 0 ? (
        <EmptyState icon={<Users className="size-5" />} title="No customers to match yet">
          Win a deal, or add a customer&apos;s website above, and Splee finds companies like them in the company database.
        </EmptyState>
      ) : (
        <ul className="flex flex-wrap gap-2 p-4">
          {seeds.map((s) => (
            <li key={s.domain} className="flex min-w-0 items-center gap-2 rounded-full border border-border py-1 pr-1 pl-2.5 text-sm">
              <Favicon domain={s.domain} />
              <span className="min-w-0 truncate" title={s.known ? undefined : "Splee knows nothing about it yet"}>
                {s.name || s.domain}
              </span>
              {!s.known && <span className="text-xs whitespace-nowrap text-muted-foreground">nothing known yet</span>}
              {s.won && <Badge tone="green">Won</Badge>}
              <button
                type="button"
                onClick={() => remove(s.domain)}
                disabled={saving}
                aria-label={`Stop matching ${s.domain}`}
                className="rounded-full p-1 text-muted-foreground hover:bg-black/5 hover:text-foreground disabled:opacity-50"
              >
                <X className="size-3.5" />
              </button>
            </li>
          ))}
        </ul>
      )}
      {removed.length > 0 && (
        <p className="border-t border-border px-4 py-2.5 text-sm text-muted-foreground">
          {plural(removed.length, "won deal is", "won deals are")} left out.{" "}
          <button type="button" onClick={() => save({ removed: [] })} disabled={saving} className="font-medium text-foreground hover:underline">
            Put them back
          </button>
        </p>
      )}
    </Card>
  );
}

/** "Accounting firm (2 of 3)", out of the customers Splee has that data for, or why a part has nothing to match on. */
function describe(part: LookalikePart) {
  if (part.values.length === 0) return part.known === 0 ? "Splee doesn't know this about them yet" : "Nothing they have in common";
  return part.values.map((v) => `${v.label} (${v.seeds} of ${part.known})`).join(", ");
}

function Profile({
  parts,
  off,
  saving,
  save,
}: {
  parts: LookalikePart[];
  off: string[];
  saving: boolean;
  save: (change: Partial<LookalikeSettings>) => void;
}) {
  const toggle = (key: string, on: boolean) => save({ off: on ? off.filter((k) => k !== key) : [...off, key] });
  return (
    <Card>
      <div className="border-b border-border p-4">
        <h2 className="font-medium">What they have in common</h2>
        <p className="text-sm text-pretty text-muted-foreground">
          What at least a third of them share. Counts are of the customers Splee has that data for. Switch off what doesn&apos;t matter to
          you.
        </p>
      </div>
      <ul className="divide-y divide-border">
        {parts.map((p) => (
          <li key={p.key} className="flex items-start gap-3 px-4 py-3">
            <Switch checked={p.on} onChange={(on) => toggle(p.key, on)} disabled={saving || p.values.length === 0} label={p.label} />
            <div className="-mt-0.5 min-w-0">
              <p className={cn("text-sm font-medium", !p.on && "text-muted-foreground")}>{p.label}</p>
              <p className="text-sm text-pretty break-words text-muted-foreground">{describe(p)}</p>
            </div>
          </li>
        ))}
      </ul>
    </Card>
  );
}

function Matches({ projectId, data, onAdded }: { projectId: string; data: Lookalikes; onAdded: () => void }) {
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [found, setFound] = useState<(FoundPeople & { segmentId: string; segment: string }) | null>(null);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const results = data.results;
  const all = results.length > 0 && results.every((m) => picked.has(m.domain));
  const toggle = (domain: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(domain)) next.add(domain);
      return next;
    });

  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <PickedBar
          projectId={projectId}
          addPath={`/projects/${projectId}/lookalikes/add`}
          domains={[...picked]}
          segments={segments ?? []}
          linkedin={settings?.unipile_connected}
          onDone={() => {
            setPicked(new Set());
            onAdded();
          }}
          onFound={setFound}
        />
      ) : (
        <div className="border-b border-border px-4 py-3">
          <h2 className="font-medium">Companies like them</h2>
          <p className="text-sm text-pretty text-muted-foreground">
            {results.length > 0
              ? `${plural(results.length, "company", "companies")} from the company database, the closest match first.`
              : "No company in the database shares enough with them yet."}
          </p>
        </div>
      )}
      {found && <FoundPanel projectId={projectId} found={found} onClose={() => setFound(null)} />}
      {results.length === 0 ? (
        <EmptyState icon={<Building2 className="size-5" />} title="No matches">
          Switch on more of what they have in common, or add more customers.
        </EmptyState>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr>
                <Th className="w-[1%] pr-0">
                  <input
                    type="checkbox"
                    checked={all}
                    ref={(box) => {
                      if (box) box.indeterminate = picked.size > 0 && !all;
                    }}
                    onChange={() => setPicked(all ? new Set() : new Set(results.map((m) => m.domain)))}
                    aria-label="Select every match"
                    className="block accent-black"
                  />
                </Th>
                <Th>Company</Th>
                <Th className="hidden md:table-cell">Why it matches</Th>
                <Th className="hidden w-[1%] text-right md:table-cell">Match</Th>
              </tr>
            </thead>
            <tbody>
              {results.map((m) => {
                const place = [m.region, m.country_code].filter(Boolean).join(", ");
                const score = (
                  <Badge tone={m.score >= 60 ? "green" : m.score >= 35 ? "amber" : "neutral"} className="tabular-nums">
                    {num(m.score)}
                  </Badge>
                );
                const reasons = (
                  <ul className="space-y-0.5 text-pretty text-muted-foreground">
                    {m.reasons.map((r) => (
                      <li key={r}>{r}</li>
                    ))}
                  </ul>
                );
                return (
                  <Tr key={m.domain} onClick={() => toggle(m.domain)}>
                    <Td className="pr-0 align-top" onClick={(e) => e.stopPropagation()}>
                      <input
                        type="checkbox"
                        checked={picked.has(m.domain)}
                        onChange={() => toggle(m.domain)}
                        aria-label={`Select ${m.name}`}
                        className="mt-0.5 block accent-black"
                      />
                    </Td>
                    <Td className="max-w-0 align-top md:w-[40%]">
                      <span className="flex items-start gap-2">
                        <Favicon domain={m.domain} className="mt-0.5" />
                        <span className="min-w-0 flex-1">
                          {/* Below md the score sits beside the name, so the reasons under it get the row's whole width. */}
                          <span className="flex items-start justify-between gap-2">
                            <span className="min-w-0 truncate font-medium">{m.name}</span>
                            <span className="shrink-0 md:hidden">{score}</span>
                          </span>
                          <a
                            href={m.website}
                            target="_blank"
                            rel="noreferrer"
                            onClick={(e) => e.stopPropagation()}
                            className="block truncate text-xs text-muted-foreground hover:underline"
                          >
                            <Host host={m.domain} />
                          </a>
                          <span className="block truncate text-xs text-muted-foreground">
                            {[categoryWords(m.category), place].filter(Boolean).join(" · ")}
                          </span>
                          {/* The reasons column only shows from md up. */}
                          <span className="mt-1.5 block text-xs md:hidden">{reasons}</span>
                        </span>
                      </span>
                    </Td>
                    <Td className="hidden align-top md:table-cell">{reasons}</Td>
                    <Td className="hidden text-right align-top md:table-cell">{score}</Td>
                  </Tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
      <MicroLabel className="border-t border-border px-4 py-2.5">Match is out of 100</MicroLabel>
    </Card>
  );
}
