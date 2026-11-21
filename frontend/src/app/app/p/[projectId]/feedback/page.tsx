"use client";

import { use, useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import {
  Check,
  ChevronDown,
  Lightbulb,
  Loader2,
  Mail,
  MessageCircleQuestion,
  PenLine,
  RefreshCw,
  Rocket,
  ScrollText,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LeadModal } from "@/components/app/work/lead-modal";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Input, Select, Switch, Textarea } from "@/components/ui/form";
import { Badge, type BadgeTone, Card, EmptyState, Skeleton, Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { FeedbackMention, FeedbackRequest, FeedbackSource, FeedbackView } from "@/lib/types";
import { cn, dealMoney, plural, timeAgo } from "@/lib/utils";

type Filter = "asked" | "shipped" | "declined";
type Act = (what: string, call: () => Promise<FeedbackView>) => Promise<boolean>;

const STATUS: Record<FeedbackRequest["status"], { label: string; tone: BadgeTone }> = {
  open: { label: "Open", tone: "neutral" },
  planned: { label: "Planned", tone: "blue" },
  shipped: { label: "Shipped", tone: "green" },
  declined: { label: "Declined", tone: "neutral" },
};

const SOURCE: Record<FeedbackSource, { label: string; icon: typeof Mail }> = {
  email: { label: "an email", icon: Mail },
  meeting: { label: "a meeting", icon: Video },
  review: { label: "the win/loss review", icon: ScrollText },
  room: { label: "the deal room", icon: MessageCircleQuestion },
};

const inFilter = (r: FeedbackRequest, f: Filter) => (f === "asked" ? r.status === "open" || r.status === "planned" : r.status === f);

export default function FeedbackPage({ params }: PageProps<"/app/p/[projectId]/feedback">) {
  const { projectId } = use(params);
  const key = `/projects/${projectId}/feedback`;
  const { data, error, mutate } = useSWR<FeedbackView>(key, fetcher, {
    refreshInterval: (v) => (v?.scan.status === "scanning" || v?.requests.some((r) => r.announcing) ? 3000 : 0),
  });
  const [filter, setFilter] = useState<Filter>("asked");
  const [busy, setBusy] = useState<string | null>(null);
  const [leadId, setLeadId] = useState<string | null>(null);

  const act: Act = async (what, call) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(null);
    }
  };
  const scanning = data?.scan.status === "scanning";
  const scan = () => act("scan", () => api<FeedbackView>(`${key}/scan`, { method: "POST" }));
  const requests = data?.requests ?? [];
  const shown = requests.filter((r) => inFilter(r, filter));
  const count = (f: Filter) => requests.filter((r) => inFilter(r, f)).length;

  return (
    <>
      <PageHeader
        title="Feedback"
        description="What buyers ask your product to do, from their replies, meetings, win/loss reviews and deal room questions, with the deals of the people who asked."
        actions={
          <div className="flex items-center gap-3">
            {data?.scan.scanned_at && !scanning && <span className="text-xs text-muted-foreground">Read {timeAgo(data.scan.scanned_at)}</span>}
            <Button type="button" variant="outline" size="sm" onClick={scan} loading={busy === "scan"} disabled={scanning}>
              {scanning ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}
              {scanning ? "Reading…" : "Read what's new"}
            </Button>
          </div>
        }
      />
      <PageBody>
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <div className="flex flex-col gap-3">
            <Skeleton className="h-10 w-72 rounded-xl" />
            <Skeleton className="h-28 rounded-2xl" />
            <Skeleton className="h-28 rounded-2xl" />
          </div>
        ) : requests.length === 0 ? (
          <Card>
            <EmptyState
              icon={scanning ? <Loader2 className="size-5 animate-spin" /> : <Lightbulb className="size-5" />}
              title={scanning ? "Reading what buyers said" : "No requests yet"}
            >
              {scanning
                ? "The agent is reading your replies, meetings, reviews and deal room questions from the last 90 days."
                : "Every few hours the agent reads what buyers wrote and said and files each thing they ask your product to do here, in their words."}
            </EmptyState>
          </Card>
        ) : (
          <div className="flex flex-col gap-4">
            <Tabs
              value={filter}
              onChange={setFilter}
              options={[
                { value: "asked", label: `Asked for · ${count("asked")}` },
                { value: "shipped", label: `Shipped · ${count("shipped")}` },
                { value: "declined", label: `Declined · ${count("declined")}` },
              ]}
            />
            {shown.length === 0 ? (
              <p className="text-sm text-muted-foreground">Nothing here yet.</p>
            ) : (
              <ol className="flex flex-col gap-3">
                {shown.map((r, i) => (
                  <RequestCard
                    key={r.id}
                    r={r}
                    rank={filter === "asked" ? i + 1 : null}
                    currency={data.currency}
                    others={requests.filter((o) => o.id !== r.id)}
                    busy={busy}
                    act={act}
                    projectId={projectId}
                    openLead={setLeadId}
                    onShipped={() => setFilter("shipped")}
                  />
                ))}
              </ol>
            )}
          </div>
        )}
      </PageBody>
      <LeadModal leadId={leadId} onClose={() => setLeadId(null)} />
    </>
  );
}

function RequestCard({
  r,
  rank,
  currency,
  others,
  busy,
  act,
  projectId,
  openLead,
  onShipped,
}: {
  r: FeedbackRequest;
  rank: number | null;
  currency: string;
  others: FeedbackRequest[];
  busy: string | null;
  act: Act;
  projectId: string;
  openLead: (id: string) => void;
  /** Follows it to the Shipped tab, where the agent's emails show up. */
  onShipped: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [mode, setMode] = useState<"edit" | "ship" | null>(null);
  const money = (cents: number) => dealMoney(Math.round(cents / 100), currency);
  const post = (path: string, body: Record<string, string>) => api<FeedbackView>(`/feedback/${r.id}${path}`, { body });
  const setStatus = (status: string) => act(r.id, () => post("", { status }));
  const merge = (into: string) => act(r.id, () => post("/merge", { into }));
  const remove = () => act(r.id, () => api<FeedbackView>(`/feedback/${r.id}`, { method: "DELETE" }));
  const unfile = (m: FeedbackMention) => act(m.id, () => api<FeedbackView>(`/feedback-mentions/${m.id}`, { method: "DELETE" }));

  return (
    <li>
      <Card className="p-4">
        {/* The toggle wraps under the request on a phone, in line with its text, rather than squeezing the title into a narrow column. */}
        <div className="flex flex-wrap items-start gap-x-3 gap-y-1">
          {rank && <span className="mt-0.5 w-5 shrink-0 text-right text-sm text-muted-foreground tabular-nums">{rank}</span>}
          <div className="min-w-0 flex-1 basis-56">
            <div className="flex flex-wrap items-center gap-2">
              <h2 className="font-medium text-balance">{r.title}</h2>
              {r.status !== "open" && <Badge tone={STATUS[r.status].tone}>{STATUS[r.status].label}</Badge>}
            </div>
            {r.detail && <p className="mt-0.5 text-sm text-pretty text-muted-foreground">{r.detail}</p>}
            <p className="mt-2 flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
              {r.open_cents > 0 && (
                <span>
                  <span className="font-medium tabular-nums">{money(r.open_cents)}</span> <span className="text-muted-foreground">open</span>
                </span>
              )}
              {r.lost_cents > 0 && (
                <span>
                  <span className="font-medium text-red-700 tabular-nums">{money(r.lost_cents)}</span>{" "}
                  <span className="text-muted-foreground">lost</span>
                </span>
              )}
              {r.won_cents > 0 && (
                <span>
                  <span className="font-medium tabular-nums">{money(r.won_cents)}</span> <span className="text-muted-foreground">from customers</span>
                </span>
              )}
              <span className="text-muted-foreground">
                {plural(r.people, "person", "people")}
                {r.companies > 1 && ` at ${plural(r.companies, "company", "companies")}`}
                {r.last_said_at && `, last ${timeAgo(r.last_said_at)}`}
              </span>
            </p>
          </div>
          <button
            type="button"
            onClick={() => setOpen(!open)}
            aria-expanded={open}
            className={cn("inline-flex shrink-0 items-center gap-1 rounded-lg px-2 py-1 text-sm font-medium hover:bg-accent", rank && "max-sm:ml-6")}
          >
            {open ? "Hide" : "What they said"} <ChevronDown className={cn("size-3.5 transition-transform", open && "rotate-180")} />
          </button>
        </div>

        {r.status === "shipped" && <Shipped r={r} projectId={projectId} />}

        {open && (
          <div className={cn("mt-3 flex flex-col gap-3 border-t border-border pt-3", rank && "sm:pl-8")}>
            <ul className="flex flex-col gap-2.5">
              {r.mentions.map((m) => (
                <Quote key={m.id} m={m} openLead={openLead} unfile={() => unfile(m)} busy={busy === m.id} />
              ))}
            </ul>
            {mode === "edit" ? (
              <EditForm r={r} busy={busy === r.id} act={act} done={() => setMode(null)} />
            ) : mode === "ship" ? (
              <ShipForm r={r} busy={busy === r.id} act={act} done={() => setMode(null)} shipped={onShipped} />
            ) : (
              <div className="flex flex-wrap items-center gap-2">
                {r.status !== "shipped" && (
                  <Button type="button" size="sm" onClick={() => setMode("ship")}>
                    <Rocket className="size-3.5" /> It shipped
                  </Button>
                )}
                <Select
                  value={r.status === "shipped" ? "" : r.status}
                  onChange={(e) => setStatus(e.target.value)}
                  disabled={busy === r.id}
                  className="h-8 w-auto text-sm"
                  aria-label="Status"
                >
                  {r.status === "shipped" && (
                    <option value="" disabled>
                      Shipped
                    </option>
                  )}
                  <option value="open">Open</option>
                  <option value="planned">Planned</option>
                  <option value="declined">Declined</option>
                </Select>
                {others.length > 0 && (
                  <Select
                    value=""
                    onChange={(e) => e.target.value && merge(e.target.value)}
                    disabled={busy === r.id}
                    className="h-8 w-auto max-w-56 text-sm"
                    aria-label="Merge into"
                  >
                    <option value="">Merge into…</option>
                    {others.map((o) => (
                      <option key={o.id} value={o.id}>
                        {o.title}
                      </option>
                    ))}
                  </Select>
                )}
                <Button type="button" size="sm" variant="ghost" onClick={() => setMode("edit")}>
                  <PenLine className="size-3.5" /> Rename
                </Button>
                <Button type="button" size="sm" variant="ghost" onClick={remove} disabled={busy === r.id} className="ml-auto text-muted-foreground">
                  <Trash2 className="size-3.5" /> Delete
                </Button>
              </div>
            )}
          </div>
        )}
      </Card>
    </li>
  );
}

function Quote({ m, openLead, unfile, busy }: { m: FeedbackMention; openLead: (id: string) => void; unfile: () => void; busy: boolean }) {
  const Icon = SOURCE[m.source].icon;
  return (
    <li className="group flex items-start gap-2.5">
      <Icon className="mt-1 size-3.5 shrink-0 text-muted-foreground" aria-label={SOURCE[m.source].label} />
      <div className="min-w-0 flex-1">
        <p className="text-sm text-pretty">&ldquo;{m.quote}&rdquo;</p>
        <p className="mt-0.5 text-xs text-muted-foreground">
          <button type="button" onClick={() => openLead(m.lead_id)} className="font-medium text-foreground hover:underline">
            {m.name || "Someone"}
          </button>
          {m.company && ` at ${m.company}`}, in {SOURCE[m.source].label} {timeAgo(m.said_at)}
          {m.deal_status && <span className={cn(m.deal_status === "lost" && "text-red-700")}> · deal {m.deal_status}</span>}
        </p>
      </div>
      <button
        type="button"
        onClick={unfile}
        disabled={busy}
        title="Not about this"
        aria-label="Not about this"
        className="rounded-md p-1 text-muted-foreground opacity-0 group-hover:opacity-100 hover:bg-accent focus-visible:opacity-100"
      >
        <X className="size-3.5" />
      </button>
    </li>
  );
}

function Shipped({ r, projectId }: { r: FeedbackRequest; projectId: string }) {
  const parts = [
    r.drafted > 0 && `${plural(r.drafted, "email")} to approve`,
    r.sent > 0 && `${r.sent} sent`,
    r.skipped > 0 && `${r.skipped} skipped, with no address, opted out or no email written`,
  ].filter(Boolean);
  return (
    <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1 rounded-xl bg-brand-50 px-3 py-2 text-sm text-brand-800">
      <span className="flex items-center gap-1.5 font-medium">
        <Check className="size-3.5" /> Shipped {r.shipped_at ? timeAgo(r.shipped_at) : ""}
      </span>
      {r.announcing ? (
        <span className="flex items-center gap-1.5">
          <Loader2 className="size-3.5 animate-spin" /> The agent is writing to everyone who asked…
        </span>
      ) : (
        parts.length > 0 && <span>{parts.join(", ")}.</span>
      )}
      {r.drafted > 0 && !r.announcing && (
        <Link href={`/app/p/${projectId}/emails`} className="font-medium underline underline-offset-2">
          Review them
        </Link>
      )}
    </div>
  );
}

function EditForm({ r, busy, act, done }: { r: FeedbackRequest; busy: boolean; act: Act; done: () => void }) {
  const [title, setTitle] = useState(r.title);
  const [detail, setDetail] = useState(r.detail);
  const save = async () => {
    if (await act(r.id, () => api<FeedbackView>(`/feedback/${r.id}`, { body: { title, detail } }))) done();
  };
  return (
    <div className="flex flex-col gap-2">
      <Input value={title} onChange={(e) => setTitle(e.target.value)} maxLength={200} aria-label="Title" />
      <Textarea
        value={detail}
        onChange={(e) => setDetail(e.target.value)}
        maxLength={1000}
        rows={2}
        aria-label="What they need it for"
        className="resize-none"
      />
      <div className="flex justify-end gap-2">
        <Button type="button" size="sm" variant="ghost" onClick={done}>
          Cancel
        </Button>
        <Button type="button" size="sm" onClick={save} loading={busy} disabled={!title.trim()}>
          Save
        </Button>
      </div>
    </div>
  );
}

function ShipForm({ r, busy, act, done, shipped }: { r: FeedbackRequest; busy: boolean; act: Act; done: () => void; shipped: () => void }) {
  const [note, setNote] = useState("");
  const [email, setEmail] = useState(r.people > 0);
  const ship = async () => {
    if (await act(r.id, () => api<FeedbackView>(`/feedback/${r.id}/ship`, { body: { note, email } }))) shipped();
  };
  return (
    <div className="flex flex-col gap-3 rounded-xl border border-border p-3">
      <Textarea
        value={note}
        onChange={(e) => setNote(e.target.value)}
        maxLength={2000}
        rows={2}
        autoFocus
        placeholder="What shipped and where to read about it, like a link to the changelog"
        aria-label="What shipped"
        className="min-h-[calc(2lh+1.125rem)] resize-none field-sizing-content"
      />
      <div className="flex flex-wrap items-center gap-3">
        {r.people > 0 && (
          <label className="flex items-center gap-2 text-sm">
            <Switch checked={email} onChange={setEmail} label="Email everyone who asked" />
            Draft an email to the {plural(r.people, "person", "people")} who asked, for you to approve
          </label>
        )}
        <div className="ml-auto flex gap-2">
          <Button type="button" size="sm" variant="ghost" onClick={done}>
            Cancel
          </Button>
          <Button type="button" size="sm" onClick={ship} loading={busy} disabled={email && !note.trim()}>
            <Rocket className="size-3.5" /> Mark shipped
          </Button>
        </div>
      </div>
    </div>
  );
}
