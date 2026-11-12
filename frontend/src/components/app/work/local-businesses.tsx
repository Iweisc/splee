"use client";

import { useEffect, useState } from "react";
import { Globe, MapPin, Phone, Search, Star, Store, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button, LinkButton } from "@/components/ui/button";
import { Input, Select, Textarea } from "@/components/ui/form";
import { Badge, Card, EmptyState, Host } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { LocalAdded, LocalSearch, LocalUsage, SegmentWithStats } from "@/lib/types";
import { cn, num, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Td, Th, toastError, Tr } from "./states";

/** Keep in sync with local_businesses::MAX_ADD and MAX_PLACES. */
const MAX_ADD = 50;
const MAX_PLACES = 20;

const RATINGS = [
  ["", "Any rating"],
  ["3.5", "3.5 stars and up"],
  ["4", "4 stars and up"],
  ["4.5", "4.5 stars and up"],
] as const;
const REVIEWS = [
  ["", "Any reviews"],
  ["10", "10 reviews or more"],
  ["50", "50 reviews or more"],
  ["100", "100 reviews or more"],
] as const;

const dollars = (cents: number) => `$${(cents / 100).toFixed(2)}`;

/** "+15125550100" as "+1 512 555 0100" for US and Canadian numbers. Others stay as they are. */
const phoneText = (phone: string) => phone.replace(/^\+1(\d{3})(\d{3})(\d{4})$/, "+1 $1 $2 $3");

const placesIn = (where: string) => where.split(/[\n;]/).map((p) => p.trim()).filter(Boolean);

/** Google Maps searches by what a business is and where, through the organization's own key, and adding the ticked ones to a
 * campaign, where Splee reads each one's website for a contact address. */
export function LocalBusinesses({ projectId }: { projectId: string }) {
  const { data: usage, error: usageError, mutate: retryUsage } = useSWR<LocalUsage>("/local-businesses/key", fetcher);
  if (usageError) return <Card><LoadError error={usageError} retry={() => retryUsage()} what="your Google Maps key" /></Card>;
  if (!usage) return <Card><ListSkeleton rows={3} /></Card>;
  if (!usage.connected) {
    return (
      <Card>
        <EmptyState
          icon={<MapPin className="size-5" />}
          title="Connect a Google Maps key to search"
          action={
            <LinkButton href={`/app/p/${projectId}/integrations#google-maps`} variant="outline" size="sm">
              Open Integrations
            </LinkButton>
          }
        >
          Find dentists, gyms or restaurants in any city with your own Google Maps Platform key. Splee counts every call against
          a monthly cap you set.
        </EmptyState>
      </Card>
    );
  }
  return <LocalSearchCard projectId={projectId} usage={usage} />;
}

function LocalSearchCard({ projectId, usage }: { projectId: string; usage: LocalUsage }) {
  const { mutate } = useSWRConfig();
  const [what, setWhat] = useState("");
  const [where, setWhere] = useState("");
  const [several, setSeveral] = useState(false);
  const [rating, setRating] = useState("");
  const [reviews, setReviews] = useState("");
  const [website, setWebsite] = useState(false);
  // The search runs on submit only, since it can cost money. The filters then refetch it from Splee's copy, which is free.
  const [asked, setAsked] = useState<{ what: string; where: string } | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [added, setAdded] = useState<(LocalAdded & { segment: string }) | null>(null);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);

  const key = asked
    ? `/projects/${projectId}/local-businesses${qs({ what: asked.what, where: asked.where, min_rating: rating || null, min_reviews: reviews || null, with_website: website || null })}`
    : null;
  const { data, error, isLoading, mutate: retry } = useSWR<LocalSearch>(key, fetcher, { keepPreviousData: true, revalidateOnFocus: false });
  useEffect(() => {
    if (data) mutate("/local-businesses/key", data.usage, { revalidate: false });
  }, [data, mutate]);

  const places = placesIn(where);
  const tooMany = places.length > MAX_PLACES;
  const search = (e: React.FormEvent) => {
    e.preventDefault();
    if (!what.trim() || !places.length || tooMany) return;
    setPicked(new Set());
    setAsked({ what: what.trim(), where: places.join("; ") });
  };
  const filter =
    <T,>(set: (v: T) => void) =>
    (v: T) => {
      set(v);
      setPicked(new Set());
    };
  const toggle = (id: string) =>
    setPicked((p) => {
      const next = new Set(p);
      if (!next.delete(id)) next.add(id);
      return next;
    });
  const open = data?.items.filter((b) => !b.known) ?? [];
  const allPicked = open.length > 0 && open.every((b) => picked.has(b.place_id));
  const known = (data?.items.length ?? 0) - open.length;

  return (
    <Card className="overflow-hidden">
      <form onSubmit={search} className="flex flex-col gap-3 border-b border-border p-3 sm:p-4">
        <div className="flex flex-wrap items-start gap-2">
          <Input value={what} onChange={(e) => setWhat(e.target.value)} placeholder="Dentists" aria-label="What" className="h-9 min-w-40 flex-[1_1_12rem]" />
          {several ? (
            <Textarea
              value={where}
              onChange={(e) => setWhere(e.target.value)}
              placeholder={"78701\n78702\nRound Rock, TX"}
              aria-label="Where, one place a line"
              rows={3}
              className="min-h-20 min-w-48 flex-[2_1_16rem]"
            />
          ) : (
            <Input value={where} onChange={(e) => setWhere(e.target.value)} placeholder="Austin, TX" aria-label="Where" className="h-9 min-w-48 flex-[2_1_16rem]" />
          )}
          <Button type="submit" size="sm" className="h-9 max-sm:flex-1" loading={isLoading && !!asked} disabled={!what.trim() || !places.length || tooMany}>
            <Search className="size-4" /> Search
          </Button>
        </div>
        <div className="flex flex-wrap items-center gap-x-4 gap-y-2 text-sm">
          <Select value={rating} onChange={(e) => filter(setRating)(e.target.value)} className="h-9 w-auto flex-auto sm:flex-none" aria-label="Rating">
            {RATINGS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <Select value={reviews} onChange={(e) => filter(setReviews)(e.target.value)} className="h-9 w-auto flex-auto sm:flex-none" aria-label="Reviews">
            {REVIEWS.map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
          <label className="flex items-center gap-2 whitespace-nowrap">
            <input type="checkbox" checked={website} onChange={(e) => filter(setWebsite)(e.target.checked)} className="accent-black" />
            Only with a website
          </label>
          <button
            type="button"
            onClick={() => setSeveral(!several)}
            className="text-sm whitespace-nowrap text-muted-foreground underline-offset-4 hover:text-foreground hover:underline"
          >
            {several ? "Search one place" : "Search several places"}
          </button>
        </div>
        <p className={cn("text-xs text-pretty", tooMany ? "text-danger" : "text-muted-foreground")}>
          {tooMany
            ? `Split a search over at most ${MAX_PLACES} places. This has ${places.length}.`
            : `${num(usage.calls)} of ${num(usage.cap)} Google calls this month, about ${dollars(usage.cost_cents)} at ${dollars(usage.price_per_1000_cents)} per 1,000. Each place costs up to 3 calls, and one searched in the last 30 days is free.`}
        </p>
      </form>

      {picked.size > 0 && data && (
        <PickedBar
          projectId={projectId}
          ids={[...picked]}
          segments={segments ?? []}
          onClear={() => setPicked(new Set())}
          onAdded={(result) => {
            setAdded(result);
            setPicked(new Set());
            retry();
          }}
        />
      )}
      {added && <AddedPanel projectId={projectId} added={added} onClose={() => setAdded(null)} />}

      {!asked ? (
        <EmptyState icon={<Store className="size-5" />} title="Search Google Maps for local businesses">
          Type what they are and where, like dentists in Austin, TX. Split a big area over several cities or ZIP codes.
        </EmptyState>
      ) : error ? (
        <LoadError error={error} retry={() => retry()} what="the search" />
      ) : !data ? (
        <ListSkeleton />
      ) : (
        <>
          <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 border-b border-border px-4 py-2.5 text-sm">
            <p className="text-muted-foreground">
              {plural(data.items.length, "business", "businesses")}
              {data.found > data.items.length && ` of ${num(data.found)} found`}
              {known > 0 && `, ${num(known)} already in this project`}
              {". "}
              {/* Kept whole, so a phone never leaves the number at the end of a line. */}
              <span className="whitespace-nowrap">{data.calls === 0 ? "No Google calls needed." : `${plural(data.calls, "Google call")}.`}</span>
            </p>
            {open.length > 0 && (
              <label className="flex items-center gap-2 whitespace-nowrap">
                <input
                  type="checkbox"
                  checked={allPicked}
                  ref={(box) => {
                    if (box) box.indeterminate = picked.size > 0 && !allPicked;
                  }}
                  onChange={() => setPicked(allPicked ? new Set() : new Set(open.slice(0, MAX_ADD).map((b) => b.place_id)))}
                  className="accent-black"
                />
                Select {open.length > MAX_ADD ? `the first ${MAX_ADD}` : "all new"}
              </label>
            )}
          </div>
          {data.note && <p className="border-b border-border bg-amber-50 px-4 py-2.5 text-sm text-pretty text-amber-800">{data.note}</p>}
          {data.items.length === 0 ? (
            <EmptyState icon={<Search className="size-5" />} title="No businesses match these filters">
              Try a lower rating, fewer reviews or a nearby place.
            </EmptyState>
          ) : (
            <Results data={data} picked={picked} toggle={toggle} stale={isLoading} />
          )}
        </>
      )}
    </Card>
  );
}

function Results({ data, picked, toggle, stale }: { data: LocalSearch; picked: Set<string>; toggle: (id: string) => void; stale: boolean }) {
  return (
    <div className={cn("transition-opacity", stale && "opacity-60")}>
      {/* Cards on phones, where seven columns can't fit. */}
      <ul className="divide-y divide-border sm:hidden">
        {data.items.map((b) => (
          <li key={b.place_id}>
            <label className={cn("flex items-start gap-3 px-4 py-3", !b.known && "cursor-pointer")}>
              <input
                type="checkbox"
                checked={picked.has(b.place_id)}
                onChange={() => toggle(b.place_id)}
                disabled={!!b.known}
                aria-label={`Select ${b.name}`}
                className="mt-1 shrink-0 accent-black"
              />
              <span className="min-w-0 flex-1 space-y-0.5 text-sm">
                <span className="flex items-start justify-between gap-2">
                  <span className="font-medium text-pretty">{b.name}</span>
                  <Known known={b.known} />
                </span>
                <span className="block text-muted-foreground">
                  {[b.category, b.rating !== null && `${b.rating.toFixed(1)} stars, ${plural(b.review_count, "review")}`].filter(Boolean).join(" · ")}
                </span>
                <span className="block text-muted-foreground text-pretty">{b.address}</span>
                <span className="flex flex-wrap gap-x-4 gap-y-0.5">
                  {b.phone && (
                    <a href={`tel:${b.phone}`} className="inline-flex items-center gap-1 hover:underline">
                      <Phone className="size-3.5 text-muted-foreground" /> {phoneText(b.phone)}
                    </a>
                  )}
                  {b.domain && (
                    <a href={b.website} target="_blank" rel="noreferrer" className="inline-flex min-w-0 items-center gap-1 hover:underline">
                      <Globe className="size-3.5 shrink-0 text-muted-foreground" /> <span className="truncate"><Host host={b.domain} /></span>
                    </a>
                  )}
                </span>
              </span>
            </label>
          </li>
        ))}
      </ul>
      <div className="overflow-x-auto max-sm:hidden">
        <table className="w-full text-sm">
          <thead>
            <tr>
              <Th className="w-[1%] pr-0">
                <span className="sr-only">Selected</span>
              </Th>
              <Th>Business</Th>
              <Th className="hidden lg:table-cell">Address</Th>
              <Th className="w-[1%]">Phone</Th>
              <Th className="w-[1%]">Rating</Th>
            </tr>
          </thead>
          <tbody>
            {data.items.map((b) => (
              <Tr key={b.place_id} onClick={b.known ? undefined : () => toggle(b.place_id)}>
                <Td className="pr-0" onClick={(e) => e.stopPropagation()}>
                  <input
                    type="checkbox"
                    checked={picked.has(b.place_id)}
                    onChange={() => toggle(b.place_id)}
                    disabled={!!b.known}
                    aria-label={`Select ${b.name}`}
                    className="block accent-black"
                  />
                </Td>
                <Td className="max-w-0 w-[55%] lg:w-[35%]">
                  <span className="flex items-center gap-2">
                    <span className="block truncate font-medium">{b.name}</span>
                    <Known known={b.known} />
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">
                    {b.category}
                    {b.category && b.domain && " · "}
                    {b.domain ? (
                      <a href={b.website} target="_blank" rel="noreferrer" className="hover:underline">
                        <Host host={b.domain} />
                      </a>
                    ) : (
                      !b.category && "No website"
                    )}
                  </span>
                  {/* The address column only shows from lg up. */}
                  <span className="block truncate text-xs text-muted-foreground lg:hidden">{b.address}</span>
                </Td>
                <Td className="hidden max-w-0 w-[40%] truncate text-muted-foreground lg:table-cell" title={b.address}>
                  {b.address}
                </Td>
                <Td className="whitespace-nowrap tabular-nums">{b.phone ? phoneText(b.phone) : <span className="text-muted-foreground">—</span>}</Td>
                <Td className="whitespace-nowrap tabular-nums">
                  {b.rating !== null ? (
                    <span className="inline-flex items-center gap-1" title={plural(b.review_count, "review")}>
                      <Star className="size-3.5 fill-amber-400 text-amber-400" />
                      {b.rating.toFixed(1)}
                      <span className="text-muted-foreground">({num(b.review_count)})</span>
                    </span>
                  ) : (
                    <span className="text-muted-foreground">—</span>
                  )}
                </Td>
              </Tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

function Known({ known }: { known: LocalSearch["items"][number]["known"] }) {
  if (!known) return null;
  return <Badge className="shrink-0">{known === "company" ? "In your companies" : "In your leads"}</Badge>;
}

/** Shows while businesses are ticked, under the search form. */
function PickedBar({
  projectId,
  ids,
  segments,
  onClear,
  onAdded,
}: {
  projectId: string;
  ids: string[];
  segments: SegmentWithStats[];
  onClear: () => void;
  onAdded: (added: LocalAdded & { segment: string }) => void;
}) {
  const [busy, setBusy] = useState(false);
  const tooMany = ids.length > MAX_ADD;
  const add = async (segmentId: string) => {
    const segment = segments.find((s) => s.id === segmentId);
    if (!segment) return;
    setBusy(true);
    try {
      const res = await api<LocalAdded>(`/projects/${projectId}/local-businesses/add`, { body: { segment_id: segment.id, place_ids: ids } });
      onAdded({ ...res, segment: segment.name });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="flex flex-wrap items-center gap-2 border-b border-border bg-muted/40 p-3">
      <Button variant="ghost" size="icon" onClick={onClear} aria-label="Clear the selection" className="size-9">
        <X className="size-4" />
      </Button>
      <p className="mr-auto text-sm font-medium">
        {tooMany ? `Pick ${MAX_ADD} or fewer, not ${num(ids.length)}` : `${plural(ids.length, "business", "businesses")} selected`}
      </p>
      {segments.length > 0 ? (
        <Select value="" onChange={(e) => add(e.target.value)} disabled={busy || tooMany} className="h-9 w-auto flex-auto sm:w-60 sm:flex-none" aria-label="Add to campaign">
          <option value="">{busy ? "Reading their websites…" : "Add to campaign…"}</option>
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

/** What "Add to campaign" did, until it's closed. */
function AddedPanel({ projectId, added, onClose }: { projectId: string; added: LocalAdded & { segment: string }; onClose: () => void }) {
  const emails = added.added.filter((a) => a.email).length;
  return (
    <section aria-label="Businesses added" className="border-b border-border bg-muted/40 px-4 py-3 text-sm">
      <div className="flex items-start gap-3">
        <div className="mr-auto min-w-0">
          <p className="font-medium text-pretty">
            {added.added.length > 0 ? `Added ${plural(added.added.length, "business", "businesses")} to ${added.segment}` : `Added nothing to ${added.segment}`}
          </p>
          {added.added.length > 0 && (
            <p className="text-muted-foreground text-pretty">
              {emails > 0 ? `Their websites gave ${plural(emails, "contact address", "contact addresses")}.` : "Their websites gave no contact address."} The
              rest keep their phone for calls and texts.
            </p>
          )}
        </div>
        <Button variant="ghost" size="icon" onClick={onClose} aria-label="Close" className="-mt-1 -mr-2 size-8 shrink-0">
          <X className="size-4" />
        </Button>
      </div>
      {added.added.length > 0 && (
        <ul className="mt-2 space-y-1.5">
          {added.added.map((a) => (
            <li key={`${a.name}-${a.domain}`} className="min-w-0 sm:flex sm:items-baseline sm:gap-2">
              <span className="font-medium sm:shrink-0">{a.name}</span>
              {a.email ? (
                <span className="flex min-w-0 items-baseline gap-2">
                  <span className="truncate">{a.email}</span>
                  <Badge tone={a.email_kind === "person" ? "green" : "neutral"} className="shrink-0">
                    {a.email_kind === "person" ? (a.contact_name || "A person") : "Shared inbox"}
                  </Badge>
                </span>
              ) : (
                <span className="block text-muted-foreground text-pretty">{a.note}</span>
              )}
            </li>
          ))}
        </ul>
      )}
      {added.skipped.length > 0 && (
        <>
          <p className="mt-3 font-medium">Skipped {num(added.skipped.length)}</p>
          <ul className="mt-1 space-y-1 text-muted-foreground">
            {added.skipped.map((s) => (
              <li key={s.name} className="text-pretty">
                <span className="text-foreground">{s.name}</span>: {s.reason}
              </li>
            ))}
          </ul>
        </>
      )}
      {added.added.length > 0 && (
        <LinkButton href={`/app/p/${projectId}/leads`} variant="outline" size="sm" className="mt-3">
          Open leads
        </LinkButton>
      )}
    </section>
  );
}
