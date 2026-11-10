"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Building2, Mail, Phone, Search, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { Input, Select } from "@/components/ui/form";
import { Card, EmptyState, Favicon, Host } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import { countryName } from "@/lib/countries";
import { SOURCE_URL } from "@/lib/source";
import type { DirectoryCompany, DirectoryCountry, DirectoryPage, FoundPeople, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { cn, compact, num, plural } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";
import { CountryLoader } from "./country-loader";
import { HiringBadge, useHiringByDomain } from "./hiring";
import { SearchField } from "./search-field";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced, useLastPageGuard } from "./states";

const CATEGORY = "hidden sm:table-cell";
const CITY = "hidden md:table-cell";

/** "dental_clinic" -> "Dental clinic" */
const categoryWords = (category: string | null) => (category ? category[0].toUpperCase() + category.slice(1).replaceAll("_", " ") : "");

/** Many listings shout their city, like LEEDS. Those read as Leeds, the rest stay as written, like St Leonards-on-Sea. */
const cityName = (city: string) => (city === city.toUpperCase() ? city.toLowerCase().replace(/(^|[\s-])\p{L}/gu, (m) => m.toUpperCase()) : city);

/** LinkedIn search takes a few seconds a company, so the backend takes this many at a time. */
const MAX_PEOPLE_COMPANIES = 10;

/** Search over the shared company database, and adding the ticked companies, or the people LinkedIn finds at them, to one of the
 * project's campaigns. Instance operators also get the control that loads countries into it, above the search. */
export function DirectoryView({ projectId }: { projectId: string }) {
  const { me } = useMe();
  const { data: countries, mutate: retryCountries } = useSWR<DirectoryCountry[]>("/directory/countries", fetcher);
  const { mutate } = useSWRConfig();
  // The search key doesn't change when a country finishes loading, so its results are refetched here.
  const loaded = () => {
    retryCountries();
    mutate((key) => typeof key === "string" && key.startsWith("/directory/companies"));
  };
  return (
    <>
      {me?.operator && countries && <CountryLoader countries={countries} onLoaded={loaded} />}
      <DirectorySearch projectId={projectId} operator={me?.operator ?? false} />
    </>
  );
}

function DirectorySearch({ projectId, operator }: { projectId: string; operator: boolean }) {
  const [q, setQ] = useState("");
  const [country, setCountry] = useState("");
  const [city, setCity] = useState("");
  const [category, setCategory] = useState("");
  const [hasEmail, setHasEmail] = useState(false);
  const [hasPhone, setHasPhone] = useState(false);
  const [page, setPage] = useState(1);
  // Domains ticked on this page. Changing a filter or the page clears them, so nothing off-screen is ever added.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [found, setFound] = useState<(FoundPeople & { segmentId: string; segment: string }) | null>(null);
  const filters = { q: useDebounced(q), city: useDebounced(city), category: useDebounced(category) };

  const { data: countries, error: countriesError, mutate: retryCountries } = useSWR<DirectoryCountry[]>("/directory/countries", fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const hiring = useHiringByDomain(projectId);
  const empty = countries?.length === 0;
  const key = empty
    ? null
    : `/directory/companies${qs({ ...filters, country, has_email: hasEmail || null, has_phone: hasPhone || null, page, per_page: PER_PAGE })}`;
  const { data, error, mutate, isLoading } = useSWR<DirectoryPage>(key, fetcher, { keepPreviousData: true });
  // keepPreviousData shows the last results while typing or loading. They no longer match the filters, so they can't be ticked.
  const stale = isLoading || q !== filters.q || city !== filters.city || category !== filters.category;
  const turnPage = (p: number) => {
    setPage(p);
    setPicked(new Set());
  };
  useLastPageGuard(data?.total, page, turnPage);

  const filter =
    <T,>(set: (v: T) => void) =>
    (v: T) => {
      set(v);
      turnPage(1);
    };
  const toggle = (domain: string) =>
    !stale &&
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(domain)) next.add(domain);
      return next;
    });
  const pageDomains = data?.items.map((c) => c.domain) ?? [];
  const allPicked = pageDomains.length > 0 && pageDomains.every((d) => picked.has(d));

  if (countriesError) return <Card><LoadError error={countriesError} retry={() => retryCountries()} what="the company database" /></Card>;
  if (empty) {
    return (
      <Card>
        {operator ? (
          <EmptyState icon={<Building2 className="size-5" />} title="The company database is empty">
            Pick a country above and press Load. Its businesses show up here once it&apos;s loaded.
          </EmptyState>
        ) : (
          <EmptyState
            icon={<Building2 className="size-5" />}
            title="The company database is empty"
            action={
              <LinkButton href={`${SOURCE_URL}#running-it-for-real`} target="_blank" rel="noreferrer" variant="outline" size="sm">
                How to load it
              </LinkButton>
            }
          >
            Whoever runs this Splee can load a country here, or by running{" "}
            <code className="font-mono text-xs whitespace-nowrap">splee import-companies GB</code> on the server.
          </EmptyState>
        )}
      </Card>
    );
  }

  return (
    <Card className="overflow-hidden">
      {picked.size > 0 ? (
        <PickedBar
          projectId={projectId}
          domains={[...picked]}
          segments={segments ?? []}
          linkedin={settings?.unipile_connected}
          onDone={() => setPicked(new Set())}
          onFound={setFound}
        />
      ) : (
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          <SearchField
            value={q}
            onChange={filter(setQ)}
            placeholder="Search names and categories"
            label="Search companies"
            className="min-w-64 flex-[999_1_0%] md:min-w-72"
          />
          <Select value={country} onChange={(e) => filter(setCountry)(e.target.value)} className="h-9 w-auto flex-auto" aria-label="Country">
            <option value="">Every country</option>
            {countries?.map((c) => (
              <option key={c.country_code} value={c.country_code}>
                {countryName(c.country_code)} ({compact(c.companies)})
              </option>
            ))}
          </Select>
          <Input value={city} onChange={(e) => filter(setCity)(e.target.value)} placeholder="City" aria-label="City" className="h-9 w-32 flex-auto" />
          <Input
            value={category}
            onChange={(e) => filter(setCategory)(e.target.value)}
            placeholder="Category, like dental clinic"
            aria-label="Category"
            className="h-9 w-48 flex-auto"
          />
        </div>
      )}
      {found && <FoundPanel projectId={projectId} found={found} onClose={() => setFound(null)} />}
      {/* The checkboxes sit beside the count, since next to the four filters they took a row of their own on a laptop. */}
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border px-4 py-2.5 text-sm">
        <p className="text-muted-foreground tabular-nums">
          {data && `${num(data.total)}${data.capped ? "+" : ""} ${data.total === 1 && !data.capped ? "company" : "companies"}`}
        </p>
        <div className="flex items-center gap-4 whitespace-nowrap">
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={hasEmail} onChange={(e) => filter(setHasEmail)(e.target.checked)} className="accent-black" />
            Has email
          </label>
          <label className="flex items-center gap-2">
            <input type="checkbox" checked={hasPhone} onChange={(e) => filter(setHasPhone)(e.target.checked)} className="accent-black" />
            Has phone
          </label>
        </div>
      </div>

      {error ? (
        <LoadError error={error} retry={() => mutate()} what="companies" />
      ) : !data ? (
        <ListSkeleton />
      ) : data.items.length === 0 ? (
        <EmptyState icon={<Search className="size-5" />} title="No companies match these filters">
          A city or category has to match in full, like London or dental clinic.
        </EmptyState>
      ) : (
        <>
          <div className="overflow-x-auto">
            <table className={cn("w-full text-sm transition-opacity", stale && "opacity-60")}>
              <thead>
                <tr>
                  <Th className="w-[1%] pr-0">
                    <input
                      type="checkbox"
                      checked={allPicked}
                      ref={(box) => {
                        if (box) box.indeterminate = picked.size > 0 && !allPicked;
                      }}
                      onChange={() => setPicked(allPicked ? new Set() : new Set(pageDomains))}
                      disabled={stale}
                      aria-label="Select every company on this page"
                      className="block accent-black"
                    />
                  </Th>
                  <Th>Company</Th>
                  <Th className={CATEGORY}>Category</Th>
                  <Th className={CITY}>City</Th>
                  <Th className="w-[1%]">Contact</Th>
                </tr>
              </thead>
              <tbody>
                {data.items.map((c) => (
                  <Row
                    key={c.domain}
                    company={c}
                    hiring={hiring.get(c.domain.toLowerCase()) ?? []}
                    picked={picked.has(c.domain)}
                    disabled={stale}
                    onToggle={() => toggle(c.domain)}
                  />
                ))}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} capped={data.capped} onChange={turnPage} />
        </>
      )}
    </Card>
  );
}

function Row({
  company: c,
  hiring,
  picked,
  disabled,
  onToggle,
}: {
  company: DirectoryCompany;
  hiring: string[];
  picked: boolean;
  disabled: boolean;
  onToggle: () => void;
}) {
  const category = categoryWords(c.category);
  return (
    <Tr onClick={onToggle}>
      <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
        <input type="checkbox" checked={picked} onChange={onToggle} disabled={disabled} aria-label={`Select ${c.name}`} className="block accent-black" />
      </Td>
      <Td className="max-w-0 w-[45%]">
        <span className="flex items-center gap-2">
          <Favicon domain={c.domain} />
          <span className="min-w-0">
            <span className="block truncate font-medium">{c.name}</span>
            <a href={c.website} target="_blank" rel="noreferrer" className="block truncate text-xs text-muted-foreground hover:underline">
              <Host host={c.domain} />
            </a>
            {/* The category column only shows from sm up. */}
            {category && <span className="block truncate text-xs text-muted-foreground sm:hidden">{category}</span>}
            <HiringBadge roles={hiring} className="mt-1" />
          </span>
        </span>
      </Td>
      <Td className={cn(CATEGORY, "max-w-0 w-[30%] truncate text-muted-foreground")}>{category || "—"}</Td>
      <Td className={cn(CITY, "max-w-0 w-[25%] truncate text-muted-foreground")}>{c.city ? cityName(c.city) : "—"}</Td>
      <Td>
        <span className="flex items-center gap-2.5">
          <Mail className={cn("size-4", c.email ? "text-foreground" : "text-black/15")} aria-label={c.email ? "Has an email address" : "No email address"} />
          <Phone className={cn("size-4", c.phone ? "text-foreground" : "text-black/15")} aria-label={c.phone ? "Has a phone number" : "No phone number"} />
        </span>
      </Td>
    </Tr>
  );
}

/** Replaces the filters while companies are ticked. */
function PickedBar({
  projectId,
  domains,
  segments,
  linkedin,
  onDone,
  onFound,
}: {
  projectId: string;
  domains: string[];
  segments: SegmentWithStats[];
  /** Undefined while the settings load. */
  linkedin: boolean | undefined;
  onDone: () => void;
  onFound: (found: FoundPeople & { segmentId: string; segment: string }) => void;
}) {
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const [searching, setSearching] = useState(false);
  const tooMany = domains.length > MAX_PEOPLE_COMPANIES;

  const add = async (segmentId: string) => {
    const segment = segments.find((s) => s.id === segmentId);
    if (!segment) return;
    setBusy(true);
    try {
      const res = await api<{ added: number; skipped: number }>(`/projects/${projectId}/directory/add`, {
        body: { segment_id: segment.id, domains },
      });
      const skipped = res.skipped ? `. Skipped ${num(res.skipped)} already in this project or on the do-not-contact list` : "";
      toast.success(`Added ${plural(res.added, "company", "companies")} to ${segment.name}${skipped}`, {
        action: { label: "Open campaign", onClick: () => router.push(`/app/p/${projectId}/segments/${segment.id}`) },
      });
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const findPeople = async (segmentId: string) => {
    const segment = segments.find((s) => s.id === segmentId);
    if (!segment) return;
    setSearching(true);
    try {
      const res = await api<FoundPeople>(`/projects/${projectId}/directory/people`, { body: { segment_id: segment.id, domains } });
      onFound({ ...res, segmentId: segment.id, segment: segment.name });
      onDone();
    } catch (e) {
      toastError(e);
    } finally {
      setSearching(false);
    }
  };

  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
      <Button variant="ghost" size="icon" onClick={onDone} aria-label="Clear the selection" className="size-9">
        <X className="size-4" />
      </Button>
      <p className="mr-auto text-sm font-medium">{plural(domains.length, "company", "companies")} selected</p>
      {segments.length > 0 ? (
        <>
          <Select value="" onChange={(e) => add(e.target.value)} disabled={busy || searching} className="h-9 w-auto flex-auto sm:w-56 sm:flex-none" aria-label="Add to campaign">
            <option value="">Add to campaign…</option>
            {segments.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
          {linkedin ? (
            <Select
              value=""
              onChange={(e) => findPeople(e.target.value)}
              disabled={busy || searching || tooMany}
              className="h-9 w-auto flex-auto sm:w-56 sm:flex-none"
              aria-label="Find people on LinkedIn for a campaign"
            >
              <option value="">{searching ? "Searching LinkedIn…" : tooMany ? `Find people at ${MAX_PEOPLE_COMPANIES} or fewer` : "Find people for…"}</option>
              {segments.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </Select>
          ) : (
            linkedin === false && (
              <LinkButton href={`/app/p/${projectId}/integrations`} variant="outline" size="sm" className="h-9 flex-auto sm:flex-none">
                Connect LinkedIn to find people
              </LinkButton>
            )
          )}
        </>
      ) : (
        <LinkButton href={`/app/p/${projectId}/new`} variant="outline" size="sm" className="h-9">
          Create a campaign first
        </LinkButton>
      )}
    </div>
  );
}

/** What "Find people" added, until it's closed. LinkedIn doesn't share emails, so the leads have none yet. */
function FoundPanel({
  projectId,
  found,
  onClose,
}: {
  projectId: string;
  found: FoundPeople & { segmentId: string; segment: string };
  onClose: () => void;
}) {
  const companies = found.skipped.filter((s) => !s.name);
  const people = found.skipped.filter((s) => s.name);
  return (
    <section aria-label="People found on LinkedIn" className="border-b border-border bg-muted/40 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <div className="mr-auto min-w-0">
          <p className="font-medium text-pretty">
            {found.added.length > 0
              ? `Added ${plural(found.added.length, "person", "people")} from LinkedIn to ${found.segment}`
              : `Added nobody to ${found.segment}`}
          </p>
          {found.added.length > 0 && <p className="text-muted-foreground text-pretty">LinkedIn doesn&apos;t share emails, so they have none yet.</p>}
        </div>
        <Button variant="ghost" size="icon" onClick={onClose} aria-label="Close" className="-mt-1 -mr-2 size-8 shrink-0">
          <X className="size-4" />
        </Button>
      </div>
      {found.added.length > 0 && (
        <ul className="mt-2 space-y-1">
          {found.added.map((p) => (
            // On phones the title and company go under the name, since beside it they'd be cut off.
            <li key={p.linkedin_url} className="min-w-0 sm:flex sm:items-baseline sm:gap-2">
              <a href={p.linkedin_url} target="_blank" rel="noreferrer" className="font-medium hover:underline sm:shrink-0">
                {p.name}
              </a>
              <span className="block text-muted-foreground sm:truncate">{[p.title, p.company].filter(Boolean).join(" · ")}</span>
            </li>
          ))}
        </ul>
      )}
      {found.skipped.length > 0 && (
        <>
          <p className="mt-3 font-medium">Skipped {num(found.skipped.length)}</p>
          <ul className="mt-1 space-y-1 text-muted-foreground">
            {companies.map((s) => (
              <li key={`c-${s.company}`} className="text-pretty">
                <span className="text-foreground">{s.company}</span>: {s.reason}
              </li>
            ))}
            {people.map((s) => (
              <li key={`p-${s.name}-${s.company}`} className="text-pretty">
                <span className="text-foreground">{s.name}</span> at {s.company}: {s.reason}
              </li>
            ))}
          </ul>
        </>
      )}
      {found.added.length > 0 && (
        <LinkButton href={`/app/p/${projectId}/leads`} variant="outline" size="sm" className="mt-3">
          Open leads
        </LinkButton>
      )}
    </section>
  );
}
