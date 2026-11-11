"use client";

import { useState } from "react";
import { Layers, Settings2 } from "lucide-react";
import useSWR from "swr";
import { LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { Paginated, SegmentWithStats, TechCatalogue, TechCompany, TechPick } from "@/lib/types";
import { cn, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { AddToCampaignBar } from "./hiring";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, Tr, useLastPageGuard } from "./states";

/** The catalogue and the project's picks. Every badge list on a page shares this one request. */
export function useTech(projectId: string) {
  return useSWR<TechCatalogue>(`/projects/${projectId}/tech`, fetcher);
}

/** The project's picks the company uses first, green for a good sign and amber for a competitor, then "+N" for the rest.
 * Without a matching pick the first two tools show, so a row never starts with a bare count. */
export function TechBadges({ techs, picks, className }: { techs: string[]; picks: TechPick[]; className?: string }) {
  const [open, setOpen] = useState(false);
  if (!techs.length) return null;
  const picked = picks.filter((p) => techs.includes(p.tech));
  const lead = picked.length ? picked.map((p) => p.tech) : techs.slice(0, 2);
  const rest = techs.filter((t) => !lead.includes(t));
  const kind = new Map(picked.map((p) => [p.tech, p.kind]));
  const shown = open ? [...lead, ...rest] : lead;
  return (
    <span className={cn("flex flex-wrap gap-1", className)}>
      {shown.map((t) => (
        <Badge key={t} tone={kind.get(t) === "competitor" ? "amber" : kind.get(t) === "uses" ? "green" : "neutral"} title={kind.get(t) === "competitor" ? `${t} is a competitor` : undefined}>
          {t}
        </Badge>
      ))}
      {rest.length > 0 && (
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            setOpen(!open);
          }}
          aria-expanded={open}
          className="rounded-full px-1.5 py-0.5 text-xs font-medium text-muted-foreground hover:bg-black/5 hover:text-foreground"
        >
          {open ? "Less" : `+${rest.length}`}
        </button>
      )}
    </span>
  );
}

// On phones the tools sit under the company's name, since a column of their own cut names to a few letters.
const TOOLS = "hidden sm:table-cell";
const LEADS = "hidden md:table-cell";

/** The project's companies with the tools their sites and DNS show, filtered by a tool, a category or the project's picks,
 * and "Add to campaign" for the ones with no leads yet. */
export function TechSignals({ projectId }: { projectId: string }) {
  const { data: tech } = useTech(projectId);
  // Unset until the user picks, so the default can follow whether the project has picks once they load.
  const [chosen, setChosen] = useState<string | null>(null);
  const [page, setPage] = useState(1);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const picks = tech?.picks ?? [];
  const filter = chosen ?? (picks.length ? "picks" : "");
  const key = tech ? `/projects/${projectId}/tech/companies${qs({ tech: filter || null, page, per_page: PER_PAGE })}` : null;
  const { data, error, mutate } = useSWR<Paginated<TechCompany>>(key, fetcher, { keepPreviousData: true });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const turnPage = (p: number) => {
    setPage(p);
    setPicked(new Set());
  };
  useLastPageGuard(data?.total, page, turnPage);

  const addable = data?.items.filter((c) => c.leads === 0).map((c) => c.id) ?? [];
  const allPicked = addable.length > 0 && addable.every((id) => picked.has(id));
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <AddToCampaignBar
          projectId={projectId}
          ids={[...picked]}
          segments={segments ?? []}
          onClear={() => setPicked(new Set())}
          onDone={() => {
            setPicked(new Set());
            mutate();
          }}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          <Select
            value={filter}
            onChange={(e) => {
              setChosen(e.target.value);
              turnPage(1);
            }}
            className="h-9 w-auto flex-auto sm:w-56 sm:flex-none"
            aria-label="Tool"
          >
            {picks.length > 0 && <option value="picks">Tools you picked</option>}
            <option value="">Any tool</option>
            <optgroup label="Categories">
              {tech?.categories.map((c) => (
                <option key={c.key} value={c.key}>
                  {c.label}
                </option>
              ))}
            </optgroup>
            <optgroup label="Tools">
              {[...(tech?.catalogue ?? [])]
                .sort((a, b) => a.name.localeCompare(b.name))
                .map((t) => (
                  <option key={t.name} value={t.name}>
                    {t.name}
                  </option>
                ))}
            </optgroup>
          </Select>
          <p className="mr-auto text-sm text-muted-foreground tabular-nums">{data && data.total > 0 && plural(data.total, "company", "companies")}</p>
          <LinkButton href={`/app/p/${projectId}/targeting`} variant="outline" size="sm" className="h-9 flex-auto sm:flex-none">
            <Settings2 className="size-3.5" /> Tools to look for
          </LinkButton>
        </div>
      )}

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="companies" />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Layers className="size-5" />} title={filter ? "No companies match yet" : "No tools found yet"}>
          Splee checks each of your companies&apos; homepage and DNS for the tools it uses, up to 300 companies an hour, and again every
          30 days.
        </EmptyState>
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr>
                  <Th className="w-[1%] pr-0">
                    <input
                      type="checkbox"
                      checked={allPicked}
                      ref={(box) => {
                        if (box) box.indeterminate = picked.size > 0 && !allPicked;
                      }}
                      onChange={() => setPicked(allPicked ? new Set() : new Set(addable))}
                      disabled={addable.length === 0}
                      aria-label="Select every company without leads"
                      className="block accent-black"
                    />
                  </Th>
                  <Th>Company</Th>
                  <Th className={TOOLS}>Tools</Th>
                  <Th className={cn(LEADS, "w-[1%]")}>Leads</Th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((c) => {
                  const onToggle = c.leads === 0 ? () => toggle(c.id) : undefined;
                  return (
                    <Tr key={c.id} onClick={onToggle}>
                      <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
                        {onToggle && (
                          <input type="checkbox" checked={picked.has(c.id)} onChange={onToggle} aria-label={`Select ${c.name || c.domain}`} className="block accent-black" />
                        )}
                      </Td>
                      <Td className="max-w-0 sm:w-[40%]">
                        <span className="flex items-center gap-2">
                          <Favicon domain={c.domain} />
                          <span className="min-w-0">
                            <span className="block truncate font-medium">{c.name || c.domain}</span>
                            <span className="block truncate text-xs text-muted-foreground">{c.domain}</span>
                          </span>
                        </span>
                        <TechBadges techs={c.techs} picks={picks} className="mt-1.5 pl-6 sm:hidden" />
                      </Td>
                      <Td className={cn(TOOLS, "w-[60%]")}>
                        <TechBadges techs={c.techs} picks={picks} />
                      </Td>
                      <Td className={cn(LEADS, "whitespace-nowrap text-muted-foreground tabular-nums")}>{c.leads > 0 ? plural(c.leads, "lead") : "None yet"}</Td>
                    </Tr>
                  );
                })}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={turnPage} />
        </>
      )}
    </Card>
  );
}
