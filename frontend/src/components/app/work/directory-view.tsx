"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { Bookmark, Building2, Mail, Pencil, Phone, Search, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Favicon, Host, Modal } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import { countryName } from "@/lib/countries";
import { SOURCE_URL } from "@/lib/source";
import type {
  DirectoryCompany,
  DirectoryCountry,
  DirectoryPage,
  FoundPeople,
  ProjectSettings,
  SavedSearch,
  SavedSearchMatches,
  SegmentWithStats,
  TechPick,
} from "@/lib/types";
import { cn, compact, dateTime, num, plural } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";
import { CountryLoader } from "./country-loader";
import { HiringBadge, useHiringByDomain } from "./hiring";
import { SearchField } from "./search-field";
import { TechBadges, useTech } from "./tech";
import { ListSkeleton, LoadError, Pager, PER_PAGE, Td, Th, toastError, Tr, useDebounced, useLastPageGuard } from "./states";

const CATEGORY = "hidden sm:table-cell";
const CITY = "hidden md:table-cell";

/** "dental_clinic" -> "Dental clinic" */
export const categoryWords = (category: string | null) => (category ? category[0].toUpperCase() + category.slice(1).replaceAll("_", " ") : "");

/** Many listings shout their city, like LEEDS. Those read as Leeds, the rest stay as written, like St Leonards-on-Sea. */
const cityName = (city: string) => (city === city.toUpperCase() ? city.toLowerCase().replace(/(^|[\s-])\p{L}/gu, (m) => m.toUpperCase()) : city);

/** LinkedIn search takes a few seconds a company, so the backend takes this many at a time. */
const MAX_PEOPLE_COMPANIES = 10;

/** A saved search an alert links to, by its ?search and ?since. */
type Linked = { id: string; since: string | null };

const savedSearchesKey = (projectId: string) => `/projects/${projectId}/directory/searches`;

/** Search over the shared company database, and adding the ticked companies, or the people LinkedIn finds at them, to one of the
 * project's campaigns. Instance operators also get the control that loads countries into it, above the search. */
export function DirectoryView({ projectId, linked = null }: { projectId: string; linked?: Linked | null }) {
  const { me } = useMe();
  const { data: countries, mutate: retryCountries } = useSWR<DirectoryCountry[]>("/directory/countries", fetcher);
  const { data: saved, error: savedError } = useSWR<SavedSearch[]>(savedSearchesKey(projectId), fetcher);
  const { mutate } = useSWRConfig();
  // The search key doesn't change when a country finishes loading, so its results are refetched here.
  const loaded = () => {
    retryCountries();
    mutate((key) => typeof key === "string" && key.startsWith("/directory/companies"));
  };
  return (
    <>
      {me?.operator && countries && <CountryLoader countries={countries} onLoaded={loaded} />}
      {/* The search starts from the linked saved search's filters, so it waits for the list. A deleted one opens a blank search. */}
      {linked && !saved && !savedError ? (
        <Card>
          <ListSkeleton />
        </Card>
      ) : (
        <DirectorySearch
          projectId={projectId}
          operator={me?.operator ?? false}
          start={linked && saved?.find((s) => s.id === linked.id)}
          since={linked?.since ?? null}
        />
      )}
    </>
  );
}

type Filters = Pick<SavedSearch, "q" | "country" | "city" | "category" | "has_email" | "has_phone" | "tech">;

const hasFilters = (f: Filters) => Boolean(f.q.trim() || f.country || f.city.trim() || f.category.trim() || f.tech || f.has_email || f.has_phone);

/** "Dental clinic · Leeds · United Kingdom · has email" */
const filterWords = (f: Filters) =>
  [
    f.q.trim() && `“${f.q.trim()}”`,
    f.category.trim() && categoryWords(f.category.trim().replaceAll(" ", "_")),
    f.city.trim() && cityName(f.city.trim()),
    f.country && countryName(f.country),
    f.tech && `uses ${f.tech}`,
    f.has_email && "has email",
    f.has_phone && "has phone",
  ]
    .filter(Boolean)
    .join(" · ");

/** "Dental clinic in Leeds, United Kingdom" */
function suggestedName(f: Filters) {
  const category = f.category.trim() && categoryWords(f.category.trim().replaceAll(" ", "_"));
  const what = category || f.q.trim() || (f.tech ? `Companies using ${f.tech}` : "Companies");
  const where = [f.city.trim() && cityName(f.city.trim()), f.country && countryName(f.country)].filter(Boolean).join(", ");
  return (where ? `${what} in ${where}` : what).slice(0, 120);
}

function DirectorySearch({
  projectId,
  operator,
  start,
  since,
}: {
  projectId: string;
  operator: boolean;
  /** A saved search to open, from an alert's link, and what its new matches are since. */
  start?: SavedSearch | null;
  since: string | null;
}) {
  const [q, setQ] = useState(start?.q ?? "");
  const [country, setCountry] = useState(start?.country ?? "");
  const [city, setCity] = useState(start?.city ?? "");
  const [category, setCategory] = useState(start?.category ?? "");
  const [hasEmail, setHasEmail] = useState(start?.has_email ?? false);
  const [hasPhone, setHasPhone] = useState(start?.has_phone ?? false);
  // The page has no field for it. A search saved through the API can have it, and opening one shows it as a chip.
  const [tech, setTech] = useState(start?.tech ?? "");
  const [page, setPage] = useState(1);
  // The saved search whose filters these are, until one changes.
  const [open, setOpen] = useState<Linked | null>(start ? { id: start.id, since: since ?? start.new_since } : null);
  const [editing, setEditing] = useState<{ search: SavedSearch | null } | null>(null);
  // Domains ticked on this page. Changing a filter or the page clears them, so nothing off-screen is ever added.
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [found, setFound] = useState<(FoundPeople & { segmentId: string; segment: string }) | null>(null);
  const filters = { q: useDebounced(q), city: useDebounced(city), category: useDebounced(category) };

  const { data: countries, error: countriesError, mutate: retryCountries } = useSWR<DirectoryCountry[]>("/directory/countries", fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const hiring = useHiringByDomain(projectId);
  const { data: techs } = useTech(projectId);
  const { data: saved } = useSWR<SavedSearch[]>(savedSearchesKey(projectId), fetcher);
  const opened = open && saved?.find((s) => s.id === open.id);
  const empty = countries?.length === 0;
  const params = { ...filters, country, has_email: hasEmail || null, has_phone: hasPhone || null, tech: tech || null, page, per_page: PER_PAGE };
  const key = empty ? null : `/directory/companies${qs(params)}`;
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
      setOpen(null);
    };
  const flip = (domain: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(domain)) next.add(domain);
      return next;
    });
  const toggle = (domain: string) => !stale && flip(domain);
  const current: Filters = { q, country, city, category, has_email: hasEmail, has_phone: hasPhone, tech };
  const openSearch = (s: SavedSearch) => {
    setQ(s.q);
    setCountry(s.country);
    setCity(s.city);
    setCategory(s.category);
    setHasEmail(s.has_email);
    setHasPhone(s.has_phone);
    setTech(s.tech);
    turnPage(1);
    setOpen({ id: s.id, since: s.new_since });
  };
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
    <>
      {saved && saved.length > 0 && (
        <SavedSearches searches={saved} openId={open?.id} onOpen={openSearch} onEdit={(search) => setEditing({ search })} />
      )}
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
        {open && opened && (
          <NewMatches
            projectId={projectId}
            search={opened}
            since={open.since}
            hiring={hiring}
            picks={techs?.picks ?? []}
            picked={picked}
            onToggle={flip}
            onClose={() => setOpen(null)}
          />
        )}
        {/* The checkboxes sit beside the count, since next to the four filters they took a row of their own on a laptop. */}
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border px-4 py-2.5 text-sm">
          <div className="flex min-w-0 items-center gap-2">
            <p className="text-muted-foreground tabular-nums">
              {data && `${num(data.total)}${data.capped ? "+" : ""} ${data.total === 1 && !data.capped ? "company" : "companies"}`}
            </p>
            {tech && (
              <Badge className="min-w-0">
                <span className="truncate">Uses {tech}</span>
                <button
                  type="button"
                  onClick={() => filter(setTech)("")}
                  aria-label={`Stop filtering by ${tech}`}
                  className="-mr-1 rounded-full hover:bg-black/10"
                >
                  <X className="size-3" />
                </button>
              </Badge>
            )}
          </div>
          <div className="flex items-center gap-4 whitespace-nowrap">
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={hasEmail} onChange={(e) => filter(setHasEmail)(e.target.checked)} className="accent-black" />
              Has email
            </label>
            <label className="flex items-center gap-2">
              <input type="checkbox" checked={hasPhone} onChange={(e) => filter(setHasPhone)(e.target.checked)} className="accent-black" />
              Has phone
            </label>
            <Button
              variant="outline"
              size="sm"
              onClick={() => setEditing({ search: null })}
              disabled={!hasFilters(current)}
              title={hasFilters(current) ? "Save these filters and hear when new companies match" : "Pick a filter first"}
            >
              <Bookmark className="size-4" />
              {/* "Save search" next to both checkboxes is wider than a 390px phone. */}
              <span className="sm:hidden">Save</span>
              <span className="max-sm:hidden">Save search</span>
            </Button>
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
                      picks={techs?.picks ?? []}
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
      <Modal open={editing !== null} onClose={() => setEditing(null)} title={editing?.search ? "Edit saved search" : "Save this search"}>
        {editing && (
          <SearchSettings
            projectId={projectId}
            segments={segments ?? []}
            search={editing.search}
            filters={current}
            onDone={(kept) => {
              setEditing(null);
              if (!kept && editing.search?.id === open?.id) setOpen(null);
            }}
          />
        )}
      </Modal>
    </>
  );
}

/** The project's saved searches, above the search. Opening one sets its filters and shows what it found lately first. */
function SavedSearches({
  searches,
  openId,
  onOpen,
  onEdit,
}: {
  searches: SavedSearch[];
  openId: string | undefined;
  onOpen: (search: SavedSearch) => void;
  onEdit: (search: SavedSearch) => void;
}) {
  return (
    <Card className="mb-4 overflow-hidden">
      <p className="border-b border-border px-4 py-2.5 text-sm font-medium">Saved searches</p>
      <ul className="max-h-72 divide-y divide-border overflow-y-auto">
        {searches.map((s) => (
          <li key={s.id} className={cn("flex items-center pr-2", s.id === openId && "bg-muted/40")}>
            <button
              type="button"
              onClick={() => onOpen(s)}
              className="flex min-w-0 flex-1 items-center gap-3 py-2.5 pr-2 pl-4 text-left text-sm hover:bg-accent"
            >
              <span className="min-w-0 flex-1">
                <span className="block truncate font-medium">{s.name}</span>
                <span className="block truncate text-xs text-muted-foreground">{filterWords(s)}</span>
              </span>
              {s.too_broad ? (
                <Badge tone="amber">Too broad to watch</Badge>
              ) : (
                s.new_matches > 0 && <Badge tone="green">{num(s.new_matches)} new</Badge>
              )}
            </button>
            <Button variant="ghost" size="icon" onClick={() => onEdit(s)} aria-label={`Edit ${s.name}`} className="size-8 shrink-0">
              <Pencil className="size-4" />
            </Button>
          </li>
        ))}
      </ul>
    </Card>
  );
}

/** Saves `filters` as a new search, or renames and sets the alerts of `search`. `onDone` gets the search, or null when it was
 * deleted or nothing was saved. */
function SearchSettings({
  projectId,
  segments,
  search,
  filters,
  onDone,
}: {
  projectId: string;
  segments: SegmentWithStats[];
  search: SavedSearch | null;
  filters: Filters;
  onDone: (kept: SavedSearch | null) => void;
}) {
  const { mutate } = useSWRConfig();
  const [name, setName] = useState(search?.name ?? suggestedName(filters));
  const [chat, setChat] = useState(search?.chat_alerts ?? false);
  const [segmentId, setSegmentId] = useState(search?.add_to_segment_id ?? "");
  const [busy, setBusy] = useState(false);

  const save = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    try {
      const body = { name, chat_alerts: chat, segment_id: segmentId || null };
      const kept = search
        ? await api<SavedSearch>(`/directory/searches/${search.id}`, { method: "PATCH", body })
        : await api<SavedSearch>(savedSearchesKey(projectId), { body: { ...filters, ...body } });
      await mutate(savedSearchesKey(projectId));
      if (search) toast.success("Saved");
      else if (kept.too_broad) toast.success("Saved. It matches over 10,000 companies, too many to watch for new ones");
      else toast.success("Saved. Splee tells you when new companies match");
      onDone(kept);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    if (!search || !confirm(`Delete ${search.name}? Companies it added to a campaign stay there.`)) return;
    setBusy(true);
    try {
      await api(`/directory/searches/${search.id}`, { method: "DELETE" });
      await mutate(savedSearchesKey(projectId));
      toast.success("Search deleted");
      onDone(null);
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  return (
    <form onSubmit={save} className="flex flex-col gap-4 text-sm">
      <p className="text-muted-foreground text-pretty">
        {filterWords(search ?? filters)}. Splee checks it every hour and tells you about new companies in your daily digest and by push.
      </p>
      <Field label="Name">
        <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={120} required />
      </Field>
      <label className="flex items-start gap-2.5">
        <input type="checkbox" checked={chat} onChange={(e) => setChat(e.target.checked)} className="mt-0.5 accent-black" />
        <span className="text-pretty">Post new companies to the project&apos;s chat alerts too</span>
      </label>
      <Field
        label="Add new companies to a campaign"
        hint="Up to 200 each hour. Ones you already have and ones on your do-not-contact list are skipped."
      >
        <Select value={segmentId} onChange={(e) => setSegmentId(e.target.value)}>
          <option value="">Don&apos;t add them</option>
          {segments.map((s) => (
            <option key={s.id} value={s.id}>
              {s.name}
            </option>
          ))}
        </Select>
      </Field>
      <div className="flex flex-wrap items-center justify-end gap-2">
        {search && (
          <Button type="button" variant="ghost" onClick={remove} disabled={busy} className="mr-auto text-danger">
            Delete
          </Button>
        )}
        <Button type="button" variant="outline" onClick={() => onDone(search)}>
          Cancel
        </Button>
        <Button type="submit" loading={busy}>
          {search ? "Save" : "Save search"}
        </Button>
      </div>
    </form>
  );
}

/** What an open saved search found since `since`, newest first, above the rest of its results. */
function NewMatches({
  projectId,
  search,
  since,
  hiring,
  picks,
  picked,
  onToggle,
  onClose,
}: {
  projectId: string;
  search: SavedSearch;
  since: string | null;
  hiring: Map<string, string[]>;
  picks: TechPick[];
  picked: Set<string>;
  onToggle: (domain: string) => void;
  onClose: () => void;
}) {
  const [page, setPage] = useState(1);
  const { mutate } = useSWRConfig();
  const { data, error, mutate: retry } = useSWR<SavedSearchMatches>(
    `/directory/searches/${search.id}/new${qs({ since, page, per_page: PER_PAGE })}`,
    fetcher,
    // Opening it counts its matches as seen, so the list's count is refreshed.
    { onSuccess: () => mutate(savedSearchesKey(projectId)) },
  );
  const status = search.too_broad
    ? "It matches over 10,000 companies, too many to watch for new ones. Save a narrower search to hear about new companies."
    : !data
      ? "Looking for new companies…"
      : data.since && data.total > 0
        ? `New since ${dateTime(data.since)}: ${plural(data.total, "company", "companies")}.`
        : "Nothing new since you last looked. Splee checks it every hour.";
  const into = search.campaign_name && !search.too_broad ? ` New ones go into ${search.campaign_name}.` : "";
  return (
    <section aria-label={`New companies for ${search.name}`} className="border-b border-border bg-muted/40 text-sm">
      <div className="flex items-start gap-3 px-4 py-3">
        <div className="mr-auto min-w-0">
          <p className="font-medium text-pretty">{search.name}</p>
          {!error && (
            <p className="text-muted-foreground text-pretty">
              {status}
              {into}
            </p>
          )}
        </div>
        <Button variant="ghost" size="icon" onClick={onClose} aria-label="Close the saved search" className="-mt-1 -mr-2 size-8 shrink-0">
          <X className="size-4" />
        </Button>
      </div>
      {error && <LoadError error={error} retry={() => retry()} what="the new companies" />}
      {data && data.items.length > 0 && (
        <div className="border-t border-border bg-card">
          <div className="overflow-x-auto">
            <table className="w-full">
              <thead>
                <tr>
                  <Th className="w-[1%] pr-0">
                    <span className="sr-only">Select</span>
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
                    picks={picks}
                    picked={picked.has(c.domain)}
                    disabled={false}
                    onToggle={() => onToggle(c.domain)}
                  />
                ))}
              </tbody>
            </table>
          </div>
          <Pager page={page} total={data.total} onChange={setPage} />
        </div>
      )}
    </section>
  );
}

function Row({
  company: c,
  hiring,
  picks,
  picked,
  disabled,
  onToggle,
}: {
  company: DirectoryCompany;
  hiring: string[];
  picks: TechPick[];
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
            <TechBadges techs={c.tech} picks={picks} className="mt-1" />
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
export function PickedBar({
  projectId,
  addPath = `/projects/${projectId}/directory/add`,
  domains,
  segments,
  linkedin,
  onDone,
  onFound,
}: {
  projectId: string;
  /** Takes {segment_id, domains} and answers {added, skipped}, like the company database's. */
  addPath?: string;
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
      const res = await api<{ added: number; skipped: number }>(addPath, {
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
export function FoundPanel({
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
