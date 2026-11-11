"use client";

import { useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import { Briefcase, Settings2, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { HiringSignal, SegmentWithStats } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Td, Th, toastError, Tr } from "./states";

/** "Hiring" and the newest watched role the company's open posts match, with the rest counted and named in the tooltip.
 * The role wraps onto a second line rather than cut off after a few letters in a narrow column. */
export function HiringBadge({ roles, className }: { roles: string[]; className?: string }) {
  if (!roles.length) return null;
  return (
    <p className={cn("line-clamp-2 text-xs leading-5 text-muted-foreground", className)} title={`Hiring: ${roles.join(", ")}`}>
      <Badge tone="blue" className="mr-1.5 align-middle">
        <Briefcase className="size-3" /> Hiring
      </Badge>
      {roles[0]}
      {roles.length > 1 && <span className="whitespace-nowrap"> +{roles.length - 1}</span>}
    </p>
  );
}

/** Matching roles by domain, for badges on lists whose rows don't carry them, like the company database. */
export function useHiringByDomain(projectId: string): Map<string, string[]> {
  const { data } = useSWR<HiringSignal[]>(`/projects/${projectId}/hiring/openings`, fetcher);
  const map = new Map<string, string[]>();
  for (const s of data ?? []) {
    const roles = map.get(s.domain.toLowerCase()) ?? [];
    if (!roles.includes(s.role)) map.set(s.domain.toLowerCase(), [...roles, s.role]);
  }
  return map;
}

const PERIODS = { "7": "Last 7 days", "30": "Last 30 days", "": "All open roles" } as const;
type Period = keyof typeof PERIODS;

const COMPANY = "hidden sm:table-cell";
const LEADS = "hidden md:table-cell";

/** New openings at the project's companies for the roles it watches, and "Add to campaign" for companies with no leads yet. */
export function HiringSignals({ projectId }: { projectId: string }) {
  const [period, setPeriod] = useState<Period>("7");
  // Company ids, since a company hiring for two roles has two rows that tick together.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const key = `/projects/${projectId}/hiring/openings${qs({ days: period || null })}`;
  const { data, error, mutate } = useSWR<HiringSignal[]>(key, fetcher, { keepPreviousData: true });
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);

  const addable = (s: HiringSignal) => !!s.company_id && s.leads === 0;
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const companies = new Set(data?.filter(addable).map((s) => s.company_id!) ?? []);
  const allPicked = companies.size > 0 && [...companies].every((id) => picked.has(id));
  const companyCount = new Set(data?.map((s) => s.domain)).size;

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
            value={period}
            onChange={(e) => {
              setPeriod(e.target.value as Period);
              setPicked(new Set());
            }}
            className="h-9 w-auto flex-auto sm:w-44 sm:flex-none"
            aria-label="First seen"
          >
            {Object.entries(PERIODS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <p className="mr-auto text-sm text-muted-foreground tabular-nums">
            {data && data.length > 0 && `${plural(data.length, "opening")} at ${plural(companyCount, "company", "companies")}`}
          </p>
          <LinkButton href={`/app/p/${projectId}/targeting`} variant="outline" size="sm" className="h-9 flex-auto sm:flex-none">
            <Settings2 className="size-3.5" /> Roles to watch
          </LinkButton>
        </div>
      )}

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="hiring signals" />
      ) : !data ? (
        <ListSkeleton />
      ) : data.length === 0 ? (
        <EmptyState icon={<Briefcase className="size-5" />} title={period ? `No new openings in the ${PERIODS[period].toLowerCase()}` : "No openings yet"}>
          Every day Splee reads the public job boards of up to 500 of your companies and leads&apos; companies, and lists the roles that
          match <Link href={`/app/p/${projectId}/targeting`} className="underline">the ones you watch</Link>.
        </EmptyState>
      ) : (
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
                    onChange={() => setPicked(allPicked ? new Set() : companies)}
                    disabled={companies.size === 0}
                    aria-label="Select every company without leads"
                    className="block accent-black"
                  />
                </Th>
                <Th>Role</Th>
                <Th className={COMPANY}>Company</Th>
                <Th className={cn(LEADS, "w-[1%]")}>Leads</Th>
                <Th className="w-[1%] text-right whitespace-nowrap">First seen</Th>
              </tr>
            </thead>
            <tbody>
              {data.map((s) => (
                <Row key={s.id} signal={s} picked={!!s.company_id && picked.has(s.company_id)} onToggle={addable(s) ? () => toggle(s.company_id!) : undefined} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

function Row({ signal: s, picked, onToggle }: { signal: HiringSignal; picked: boolean; onToggle?: () => void }) {
  const about = [s.department, s.location].filter(Boolean).join(" · ");
  return (
    <Tr onClick={onToggle}>
      <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
        {onToggle && <input type="checkbox" checked={picked} onChange={onToggle} aria-label={`Select ${s.company_name}`} className="block accent-black" />}
      </Td>
      <Td className="max-w-0 w-[55%]">
        {/* Titles carry a level, team and place, so they get two lines before they're cut. */}
        <a href={s.url} target="_blank" rel="noreferrer" onClick={(e) => e.stopPropagation()} className="line-clamp-2 font-medium hover:underline">
          {s.title}
        </a>
        {/* The company column only shows from sm up. */}
        <span className="block truncate text-xs text-muted-foreground sm:hidden">{[s.company_name, s.location].filter(Boolean).join(" · ")}</span>
        {about && <span className="hidden truncate text-xs text-muted-foreground sm:block">{about}</span>}
      </Td>
      <Td className={cn(COMPANY, "max-w-0 w-[45%]")}>
        <span className="flex items-center gap-2">
          <Favicon domain={s.domain} />
          <span className="min-w-0">
            <span className="block truncate font-medium">{s.company_name}</span>
            <span className="block truncate text-xs text-muted-foreground">{s.domain}</span>
          </span>
        </span>
      </Td>
      <Td className={cn(LEADS, "whitespace-nowrap text-muted-foreground tabular-nums")}>{s.leads > 0 ? plural(s.leads, "lead") : "None yet"}</Td>
      <Td className="text-right whitespace-nowrap text-muted-foreground" title={s.posted_at ? `Posted ${new Date(s.posted_at).toLocaleDateString()}` : undefined}>
        {timeAgo(s.first_seen)}
      </Td>
    </Tr>
  );
}

/** Replaces the filters while companies are ticked. Moves the ones without leads into the campaign and finds people there.
 * The Visitors tab ticks domains, since a visiting company may not be one of the project's companies yet, and posts them to
 * its own route, which makes the missing ones first. */
export function AddToCampaignBar({
  projectId,
  ids,
  segments,
  onClear,
  onDone,
  route = { path: "hiring/add", field: "company_ids" },
}: {
  projectId: string;
  ids: string[];
  segments: SegmentWithStats[];
  onClear: () => void;
  onDone: () => void;
  route?: { path: string; field: string };
}) {
  const router = useRouter();
  const [busy, setBusy] = useState(false);

  const add = async (segmentId: string) => {
    const segment = segments.find((s) => s.id === segmentId);
    if (!segment) return;
    setBusy(true);
    try {
      const res = await api<{ added: number }>(`/projects/${projectId}/${route.path}`, { body: { segment_id: segment.id, [route.field]: ids } });
      toast.success(`Finding people at ${plural(res.added, "company", "companies")} in ${segment.name}`, {
        action: { label: "Open campaign", onClick: () => router.push(`/app/p/${projectId}/segments/${segment.id}`) },
      });
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
      <Button variant="ghost" size="icon" onClick={onClear} aria-label="Clear the selection" className="size-9">
        <X className="size-4" />
      </Button>
      <p className="mr-auto text-sm font-medium">{plural(ids.length, "company", "companies")} selected</p>
      {segments.length > 0 ? (
        <Select value="" onChange={(e) => add(e.target.value)} disabled={busy} className="h-9 w-auto flex-auto sm:w-56 sm:flex-none" aria-label="Add to campaign">
          <option value="">Add to campaign…</option>
          {segments.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </Select>
      ) : (
        <LinkButton href={`/app/p/${projectId}/new`} variant="outline" size="sm" className="h-9">
          Create a campaign first
        </LinkButton>
      )}
    </div>
  );
}
