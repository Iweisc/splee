"use client";

import { useState } from "react";
import { BriefcaseBusiness, ExternalLink, Plus, Upload, UserRoundCheck, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Badge, Card, EmptyState } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { JobChange, JobChanges, Relationship, SegmentWithStats, WatchAdded, WatchedPerson } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { LeadModal } from "./lead-modal";
import { ListSkeleton, LoadError, Td, Th, toastError, Tr } from "./states";

/** What they did at their old company, as a short badge. */
export const RELATIONSHIP: Record<Relationship, string> = { won: "Bought", booked: "Booked a meeting", hot: "Replied hot" };
/** Shorter, for the badge next to the old company, so the company keeps the room. */
const BADGE: Record<Relationship, string> = { won: "Bought", booked: "Booked", hot: "Replied hot" };

/** For the add form, where the choice applies to everyone added at once. */
const RELATIONSHIP_CHOICES: Record<Relationship, string> = {
  won: "They bought from you",
  booked: "They booked a meeting",
  hot: "They replied with interest",
};

const FROM = "hidden md:table-cell";
const SEEN = "hidden sm:table-cell";

/** Moves seen on LinkedIn among past customers and champions, and the watch list behind them. */
export function JobChangesView({ projectId }: { projectId: string }) {
  const { data, error, mutate } = useSWR<JobChanges>(`/projects/${projectId}/job-changes`, fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const [openLead, setOpenLead] = useState<string | null>(null);

  if (error) return <LoadError error={error} retry={() => mutate()} what="job changes" />;
  if (!data) return <ListSkeleton />;
  if (!data.connected) {
    return (
      <Card>
        <EmptyState
          icon={<BriefcaseBusiness className="size-5" />}
          title="Job changes need LinkedIn"
          action={
            <LinkButton href={`/app/p/${projectId}/integrations`} variant="outline" size="sm">
              Open Integrations
            </LinkButton>
          }
        >
          Splee reads your past customers&apos; LinkedIn profiles to see when they start somewhere new. Connect LinkedIn through
          Unipile under Integrations to turn it on.
        </EmptyState>
      </Card>
    );
  }

  const setCampaign = async (segmentId: string) => {
    try {
      await mutate(api<JobChanges>(`/projects/${projectId}/job-changes/campaign`, { method: "PUT", body: { segment_id: segmentId || null } }), {
        revalidate: false,
      });
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <div className="flex flex-col gap-6">
      <Card className="overflow-hidden">
        <div className="flex flex-wrap items-center gap-2 border-b border-border p-3">
          <p className="mr-auto text-sm font-medium tabular-nums">{data.changes.length > 0 ? plural(data.changes.length, "move") : "Moves"}</p>
          <Select
            value={data.segment_id ?? ""}
            onChange={(e) => setCampaign(e.target.value)}
            className="h-9 w-auto flex-auto sm:w-64 sm:flex-none"
            aria-label="Campaign new leads from moves join"
          >
            <option value="">New leads join no campaign</option>
            {(segments ?? []).map((s) => (
              <option key={s.id} value={s.id}>
                New leads join {s.name}
              </option>
            ))}
          </Select>
        </div>
        {data.changes.length === 0 ? (
          <EmptyState icon={<UserRoundCheck className="size-5" />} title="No moves yet">
            Splee checks each person below on LinkedIn once a month. When one starts at a new company, they show up here as a lead there.
          </EmptyState>
        ) : (
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead className="hidden sm:table-header-group">
                <tr>
                  <Th>Person</Th>
                  <Th className={FROM}>Moved from</Th>
                  <Th className={cn(SEEN, "w-[1%] whitespace-nowrap")}>Seen</Th>
                  <Th className="hidden w-[1%] text-right sm:table-cell">
                    <span className="sr-only">Actions</span>
                  </Th>
                </tr>
              </thead>
              <tbody>
                {data.changes.map((c) => (
                  <ChangeRow
                    key={c.id}
                    projectId={projectId}
                    change={c}
                    segments={segments ?? []}
                    onOpen={() => c.new_lead_id && setOpenLead(c.new_lead_id)}
                    onChange={() => mutate()}
                  />
                ))}
              </tbody>
            </table>
          </div>
        )}
      </Card>
      <WatchList projectId={projectId} watching={data.watching} lastChecked={data.last_checked_at} onAdded={() => mutate()} />
      <LeadModal leadId={openLead} onClose={() => setOpenLead(null)} />
    </div>
  );
}

function ChangeRow({
  projectId,
  change: c,
  segments,
  onOpen,
  onChange,
}: {
  projectId: string;
  change: JobChange;
  segments: SegmentWithStats[];
  onOpen: () => void;
  onChange: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const act = async (path: string, body: Record<string, unknown> | undefined, done: string) => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/job-changes/${c.id}/${path}`, { body: body ?? {} });
      toast.success(done);
      onChange();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const addTo = (segmentId: string) => {
    const segment = segments.find((s) => s.id === segmentId);
    if (segment) act("campaign", { segment_id: segment.id }, `Added ${c.full_name} to ${segment.name}`);
  };

  const actions = (
    <>
      {c.segment_name ? (
        <Badge className="max-w-48 min-w-0" title={`In ${c.segment_name}`}>
          <span className="truncate">{c.segment_name}</span>
        </Badge>
      ) : (
        c.new_lead_id &&
        segments.length > 0 && (
          <Select
            value=""
            onChange={(e) => addTo(e.target.value)}
            disabled={busy}
            className="h-8 w-40 text-xs"
            aria-label={`Add ${c.full_name} to a campaign`}
          >
            <option value="">Add to campaign…</option>
            {segments.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        )
      )}
      <Button
        variant="ghost"
        size="icon"
        className="size-8 shrink-0"
        disabled={busy}
        onClick={() => act("dismiss", undefined, `Dismissed ${c.full_name}'s move`)}
        aria-label={`Dismiss ${c.full_name}'s move`}
        title="Dismiss"
      >
        <X className="size-4" />
      </Button>
    </>
  );

  return (
    <Tr onClick={c.new_lead_id ? onOpen : undefined}>
      <Td className="max-w-0 w-[55%]">
        <span className="block truncate font-medium">{c.full_name}</span>
        {/* Wraps on a phone, where the row is narrow and the actions sit underneath. */}
        <span className="block text-xs text-muted-foreground text-pretty sm:truncate">{[c.new_title, c.new_company].filter(Boolean).join(" at ")}</span>
        {/* The from column only shows from md up. */}
        <span className="block text-xs text-muted-foreground text-pretty md:hidden">
          From {c.old_company || "their last company"}, {RELATIONSHIP[c.relationship].toLowerCase()}
        </span>
        <span className="mt-2 flex items-center gap-1 sm:hidden" onClick={(e) => e.stopPropagation()}>
          {actions}
        </span>
      </Td>
      <Td className={cn(FROM, "max-w-0 w-[45%]")}>
        <span className="flex min-w-0 items-center gap-2">
          <span className="truncate">{c.old_company || "Unknown"}</span>
          <Badge tone="green" className="shrink-0" title={RELATIONSHIP[c.relationship]}>
            {BADGE[c.relationship]}
          </Badge>
        </span>
      </Td>
      <Td className={cn(SEEN, "whitespace-nowrap text-muted-foreground")}>{timeAgo(c.seen_at)}</Td>
      <Td className="hidden sm:table-cell" onClick={(e) => e.stopPropagation()}>
        <span className="flex items-center justify-end gap-1">{actions}</span>
      </Td>
    </Tr>
  );
}

const PERSON = "hidden sm:table-cell";
const CHECK = "hidden md:table-cell";

/** Everyone Splee watches for a move, with when each was last read, and the form to add people by hand or from a CSV. */
function WatchList({
  projectId,
  watching,
  lastChecked,
  onAdded,
}: {
  projectId: string;
  watching: number;
  lastChecked: string | null;
  onAdded: () => void;
}) {
  const { data, error, mutate } = useSWR<WatchedPerson[]>(`/projects/${projectId}/watch-list`, fetcher);
  const [adding, setAdding] = useState(false);

  return (
    <Card className="overflow-hidden">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border p-3">
        <div className="mr-auto min-w-0">
          <p className="text-sm font-medium">Watch list</p>
          <p className="text-xs text-muted-foreground tabular-nums">
            {plural(watching, "person", "people")} · {lastChecked ? `last check ${timeAgo(lastChecked)}` : "not checked yet"}
          </p>
        </div>
        {!adding && (
          <Button variant="outline" size="sm" className="h-9" onClick={() => setAdding(true)}>
            <Plus className="size-3.5" /> Add people
          </Button>
        )}
      </div>
      {adding && (
        <AddPeople
          projectId={projectId}
          onClose={() => setAdding(false)}
          onAdded={() => {
            mutate();
            onAdded();
          }}
        />
      )}
      {error ? (
        <LoadError error={error} retry={() => mutate()} what="the watch list" />
      ) : !data ? (
        <ListSkeleton rows={3} />
      ) : data.length === 0 ? (
        <EmptyState icon={<UserRoundCheck className="size-5" />} title="Nobody to watch yet">
          Leads who buy, book a meeting or reply with interest join on their own. Add past customers and champions from before Splee
          here.
        </EmptyState>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead className="hidden sm:table-header-group">
              <tr>
                <Th>Name</Th>
                <Th className={PERSON}>Last seen at</Th>
                <Th className={cn(CHECK, "w-[1%] whitespace-nowrap text-right")}>Checked</Th>
              </tr>
            </thead>
            <tbody>
              {data.map((p) => (
                <Tr key={p.id}>
                  <Td className="max-w-0 w-[45%]">
                    <span className="flex min-w-0 items-center gap-1.5">
                      <span className="truncate font-medium">{p.full_name || p.email || "Unnamed"}</span>
                      {p.linkedin_url && (
                        <a
                          href={p.linkedin_url}
                          target="_blank"
                          rel="noreferrer"
                          className="shrink-0 text-muted-foreground hover:text-foreground"
                          aria-label={`${p.full_name}'s LinkedIn profile`}
                        >
                          <ExternalLink className="size-3" />
                        </a>
                      )}
                    </span>
                    <span className="block truncate text-xs text-muted-foreground">
                      {RELATIONSHIP[p.relationship]}
                      <span className="sm:hidden">{p.company_name && ` · ${p.company_name}`}</span>
                    </span>
                  </Td>
                  <Td className={cn(PERSON, "max-w-0 w-[55%]")}>
                    <span className="block truncate">{p.company_name || "Unknown"}</span>
                    {p.title && <span className="block truncate text-xs text-muted-foreground">{p.title}</span>}
                  </Td>
                  <Td className={cn(CHECK, "text-right whitespace-nowrap text-muted-foreground")} title={p.check_error ?? undefined}>
                    {!p.linkedin_url ? "Needs a LinkedIn link" : p.check_error ? "Couldn't read profile" : p.checked_at ? timeAgo(p.checked_at) : "Not yet"}
                  </Td>
                </Tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}

const NO_PERSON = { name: "", email: "", linkedin_url: "", company: "" };

function AddPeople({ projectId, onClose, onAdded }: { projectId: string; onClose: () => void; onAdded: () => void }) {
  const [relationship, setRelationship] = useState<Relationship>("won");
  const [person, setPerson] = useState(NO_PERSON);
  const [busy, setBusy] = useState(false);

  const send = async (body: Record<string, unknown>) => {
    setBusy(true);
    try {
      const res = await api<WatchAdded>(`/projects/${projectId}/watch-list`, { body: { relationship, ...body } });
      const done = [res.added && `added ${plural(res.added, "person", "people")}`, res.updated && `updated ${res.updated}`].filter(Boolean).join(", ");
      const first = res.errors[0];
      if (done) toast.success(done[0].toUpperCase() + done.slice(1));
      if (first) toast.error(`${plural(res.errors.length, "row")} skipped. Row ${first.row}: ${first.error}`);
      else if (!done) toast.info("They're already on the watch list");
      setPerson(NO_PERSON);
      onAdded();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const upload = async (file: File | undefined) => {
    if (file) await send({ csv: await file.text() });
  };
  const set = (key: keyof typeof NO_PERSON) => (e: React.ChangeEvent<HTMLInputElement>) => setPerson((p) => ({ ...p, [key]: e.target.value }));

  return (
    <form
      className="flex flex-col gap-4 border-b border-border bg-muted/40 p-4"
      onSubmit={(e) => {
        e.preventDefault();
        send({ people: [person] });
      }}
    >
      <div className="flex items-start justify-between gap-3">
        <Field label="What they did with you" className="w-full sm:w-72">
          <Select value={relationship} onChange={(e) => setRelationship(e.target.value as Relationship)}>
            {Object.entries(RELATIONSHIP_CHOICES).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
        </Field>
        <Button type="button" variant="ghost" size="icon" className="size-9 shrink-0" onClick={onClose} aria-label="Close the form">
          <X className="size-4" />
        </Button>
      </div>
      <div className="grid gap-3 sm:grid-cols-2">
        <Field label="Name">
          <Input value={person.name} onChange={set("name")} placeholder="Dana Reyes" autoComplete="off" />
        </Field>
        <Field label="Company">
          <Input value={person.company} onChange={set("company")} placeholder="Where they work now" autoComplete="off" />
        </Field>
        <Field label="LinkedIn URL" hint="Needed to notice a move.">
          <Input value={person.linkedin_url} onChange={set("linkedin_url")} placeholder="https://www.linkedin.com/in/…" autoComplete="off" />
        </Field>
        <Field label="Email">
          <Input type="email" value={person.email} onChange={set("email")} placeholder="dana@company.com" autoComplete="off" />
        </Field>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        <Button type="submit" size="sm" className="h-9" loading={busy} disabled={!person.email.trim() && !person.linkedin_url.trim()}>
          Add person
        </Button>
        <label
          className={cn(
            "inline-flex h-9 cursor-pointer items-center gap-1.5 rounded-lg border border-border bg-card px-3 text-sm font-medium hover:bg-muted",
            busy && "pointer-events-none opacity-50",
          )}
        >
          <Upload className="size-3.5" /> Upload a CSV
          <input
            type="file"
            accept=".csv,text/csv"
            className="sr-only"
            onChange={(e) => {
              upload(e.target.files?.[0]);
              e.target.value = "";
            }}
          />
        </label>
        <p className="w-full text-xs text-muted-foreground text-pretty sm:w-auto sm:flex-1">
          Columns for name, email, LinkedIn URL and company, in any order.
        </p>
      </div>
    </form>
  );
}
