"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import {
  ArrowLeft,
  Bell,
  CalendarCheck,
  Check,
  Clock,
  FileText,
  Flame,
  Inbox,
  MessageSquareText,
  Paperclip,
  Pencil,
  Search,
  Send,
  Sparkles,
  ThumbsDown,
  X,
} from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, buttonClass } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState, Message, Modal, Skeleton, Spinner, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type {
  Call,
  EmailAttachment,
  EmailMessage,
  InboxThread,
  Lead,
  LeadDetail,
  LeadStatus,
  Meeting,
  OwnerFilter,
  Paginated,
  ProjectSummary,
  SavedReply,
  SegmentWithStats,
  TextMessage,
  ThreadMessage,
} from "@/lib/types";
import { cn, dateTime, fileSize, isPast, timeAgo } from "@/lib/utils";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { fetcher } from "@/hooks/use-me";
import { LoadError, MicroLabel, Pager, PER_PAGE, toastError, useDebounced, useLastPageGuard } from "./states";
import { CallCard } from "./calls";
import { MeetingCard } from "./meetings";
import { LeadDeal } from "./deal-dialog";
import { LeadNotes } from "./lead-modal";
import { OwnerFilterSelect, OwnerMenu, useOwnerFilter } from "./owner-menu";
import { ListKeysHint, useListKeys, useShortcut } from "./list-keys";
import { SearchField } from "./search-field";
import { ClassificationBadge, EmailStatusBadge, LeadStatusBadge } from "./status";

type Filter = "all" | "hot" | "needs_reply" | "auto_replied" | "other";

/** Two-pane inbox. Without `projectId` it covers every project in the org. */
export function InboxView({ projectId, initialLead }: { projectId?: string; initialLead?: string }) {
  const [filter, setFilter] = useState<Filter>("all");
  const [project, setProject] = useState("");
  const [campaign, setCampaign] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);
  const [selectedLead, setSelectedLead] = useState<string | null>(initialLead ?? null);
  const query = useDebounced(q);
  const [owner, setOwner] = useOwnerFilter("inbox");

  const { data: projects } = useSWR<ProjectSummary[]>(projectId ? null : "/projects", fetcher);
  // Archived campaigns too, since their leads can still reply.
  const { data: campaigns } = useSWR<SegmentWithStats[]>(projectId ? `/projects/${projectId}/segments?archived=true` : null, fetcher);
  const ownerParam = owner === "everyone" ? "" : owner;
  const params = { project_id: projectId ?? project, segment_id: campaign, filter, q: query, owner: ownerParam, page, per_page: PER_PAGE };
  const key = `/inbox${qs(params)}`;
  const { data, error, isLoading, mutate } = useSWR<Paginated<InboxThread>>(key, fetcher, { keepPreviousData: true, refreshInterval: 60_000 });
  useLastPageGuard(data?.total, page, setPage);
  // A lead an alert linked to stays open even when it isn't on this page of the list. Otherwise a selection the
  // filter hides falls back to the first thread.
  const linked = selectedLead !== null && selectedLead === initialLead;
  const selectedId = data?.items.find((t) => t.lead.id === selectedLead)?.lead.id ?? (linked ? selectedLead : data?.items[0]?.lead.id);
  // Phones show one pane at a time, so the keys start at md, where the list and the thread sit side by side.
  const listRef = useListKeys(data?.items.map((t) => t.lead.id), selectedId ?? undefined, setSelectedLead, "48rem");
  const pickFilter = (f: Filter) => {
    setFilter(f);
    setPage(1);
    setSelectedLead(null);
  };
  const pickOwner = (o: OwnerFilter) => {
    setOwner(o);
    setPage(1);
    setSelectedLead(null);
  };
  const pickCampaign = (id: string) => {
    setCampaign(id);
    setPage(1);
    setSelectedLead(null);
  };
  // After Done or e marks a thread done, open the one below it, or the one above when it was last. In Needs reply the
  // done thread leaves the list, so staying put would jump to the top.
  const selectNext = () => {
    const items = data?.items ?? [];
    const at = items.findIndex((t) => t.lead.id === selectedId);
    const next = items[at + 1] ?? items[at - 1];
    if (next) setSelectedLead(next.lead.id);
  };

  return (
    <Card className="grid min-h-[520px] flex-1 grid-cols-[minmax(0,1fr)] overflow-hidden md:grid-cols-[320px_minmax(0,1fr)] xl:grid-cols-[360px_minmax(0,1fr)]">
      {/* Phones show one pane at a time: the list, or the thread you tapped. */}
      <div className={cn("min-h-0 flex-col border-border md:flex md:border-r", selectedLead ? "hidden" : "flex")}>
        <div className="flex flex-col gap-2 border-b border-border p-3">
          <Tabs
            value={filter}
            onChange={pickFilter}
            options={[
              { value: "all", label: "All" },
              { value: "hot", label: "Hot" },
              { value: "needs_reply", label: "Needs reply" },
              { value: "auto_replied", label: "Auto-replied" },
              { value: "other", label: "Other" },
            ]}
            className="w-full [&>button]:flex-auto [&>button]:px-1.5 [&>button]:text-xs"
          />
          <SearchField
            value={q}
            onChange={(v) => {
              setQ(v);
              setPage(1);
              setSelectedLead(null);
            }}
            placeholder="Search name, email or company"
            label="Search replies"
          />
          {/* Side by side across the org, where the project picker shares the row. Alone, the owner filter takes it. */}
          <div className="flex gap-2 [&>*]:min-w-0 [&>*]:flex-1">
            <OwnerFilterSelect value={owner} onChange={pickOwner} />
            {!projectId && (
              <Select
                value={project}
                onChange={(e) => {
                  setProject(e.target.value);
                  setPage(1);
                  setSelectedLead(null);
                }}
                className="h-9"
                aria-label="Project"
              >
                <option value="">All projects</option>
                {projects?.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name || p.domain}
                  </option>
                ))}
              </Select>
            )}
          </div>
          {/* A row of its own, since campaign names run long and a select can't end one with an ellipsis. */}
          {projectId && (
            <Select value={campaign} onChange={(e) => pickCampaign(e.target.value)} className="h-9" aria-label="Campaign">
              <option value="">All campaigns</option>
              {campaigns?.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </Select>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {error ? (
            <LoadError error={error} retry={() => mutate()} />
          ) : !data ? (
            <RowsSkeleton />
          ) : data.items.length === 0 ? (
            query ? (
              <EmptyState icon={<Search className="size-5" />} title="No replies match your search" />
            ) : filter !== "all" ? (
              <EmptyState
                icon={<Inbox className="size-5" />}
                title={EMPTY[filter]}
                action={
                  <Button variant="outline" size="sm" onClick={() => pickFilter("all")}>
                    Show all replies
                  </Button>
                }
              />
            ) : owner !== "everyone" ? (
              // Before the campaign's, since "No replies in this campaign" would be wrong while teammates' leads are hidden.
              <EmptyState
                icon={<Inbox className="size-5" />}
                title={owner === "mine" ? "No replies from your leads" : "No replies from unassigned leads"}
                action={
                  <Button variant="outline" size="sm" onClick={() => pickOwner("everyone")}>
                    Show everyone&apos;s
                  </Button>
                }
              />
            ) : campaign ? (
              <EmptyState
                icon={<Inbox className="size-5" />}
                title="No replies in this campaign"
                action={
                  <Button variant="outline" size="sm" onClick={() => pickCampaign("")}>
                    Show all campaigns
                  </Button>
                }
              />
            ) : (
              <EmptyState icon={<Inbox className="size-5" />} title="No replies yet">
                Replies to your emails land here. Splee checks your mailboxes every minute.
              </EmptyState>
            )
          ) : (
            // keepPreviousData leaves the last filter's rows up until the new ones arrive, so they dim to show it's working.
            <ul ref={listRef} aria-busy={isLoading} className={cn("transition-opacity", isLoading && "opacity-50")}>
              {data.items.map((t) => (
                <li key={t.lead.id} data-key={t.lead.id} className="border-b border-border">
                  <ThreadRow thread={t} selected={selectedId === t.lead.id} showProject={!projectId} onSelect={() => setSelectedLead(t.lead.id)} />
                </li>
              ))}
            </ul>
          )}
        </div>
        {data && <Pager page={page} total={data.total} onChange={setPage} />}
        {!!data?.items.length && (
          <ListKeysHint className="hidden border-t border-border px-4 py-2 text-xs text-muted-foreground md:block" />
        )}
      </div>
      <div className={cn("min-h-0 flex-col md:flex", selectedLead ? "flex" : "hidden")} data-thread-open={selectedLead ? "" : undefined}>
        <button
          onClick={() => setSelectedLead(null)}
          className="flex items-center gap-1.5 border-b border-border px-4 py-2.5 text-sm text-muted-foreground hover:text-foreground md:hidden"
        >
          <ArrowLeft className="size-4" /> All replies
        </button>
        {selectedId ? (
          <ThreadPane key={selectedId} leadId={selectedId} onDone={selectNext} />
        ) : (
          !!data?.items.length && <EmptyState className="flex-1" title="Select a conversation" />
        )}
      </div>
    </Card>
  );
}

const EMPTY: Record<Exclude<Filter, "all">, string> = {
  hot: "No hot replies",
  needs_reply: "Nothing needs a reply",
  auto_replied: "No auto-replied threads",
  other: "Nothing here",
};

// Keep in sync with is_needs_reply in backend/src/routes/inbox.rs, so the dot marks the same threads as Needs reply.
const ANSWERED_FOR_YOU = ["not_interested", "ooo", "unsubscribe", "bounce"];
const CLOSED: LeadStatus[] = ["meeting_booked", "not_interested", "bounced", "unsubscribed", "do_not_contact"];

function needsReply({ last_message: m, lead, classification }: InboxThread) {
  const unanswered = m.direction === "inbound" && !m.handled_at && !ANSWERED_FOR_YOU.includes(classification ?? "") && !CLOSED.includes(lead.status);
  return unanswered || (!!lead.remind_at && isPast(lead.remind_at));
}

/**
 * A thread waiting on you has a bold name and a green dot, so the ones to answer stand out from the ones that are dealt
 * with. Hot leads get a flame by the name. The preview says when the last word was yours, not theirs.
 */
function ThreadRow({ thread: t, selected, showProject, onSelect }: { thread: InboxThread; selected: boolean; showProject: boolean; onSelect: () => void }) {
  const waiting = needsReply(t);
  const hot = t.classification === "hot" || t.lead.status === "hot";
  const m = t.last_message;
  const sentAt = m.sent_at ?? m.created_at;
  const from = m.direction === "inbound" ? "" : m.kind === "auto_reply" ? "Auto-reply: " : "You: ";
  return (
    <button
      onClick={onSelect}
      aria-current={selected || undefined}
      // Hover is lighter than the selection, so j and k's row still shows with the pointer resting on another.
      className={cn("flex w-full gap-3 px-4 py-3 text-left focus-visible:-outline-offset-2", selected ? "bg-accent" : "hover:bg-black/[0.02]")}
    >
      <Avatar name={t.lead.full_name || t.lead.email || "?"} />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          {hot && (
            <>
              <Flame className="size-3.5 shrink-0 text-brand-600" />
              <span className="sr-only">Hot lead:</span>
            </>
          )}
          <span className={cn("min-w-0 flex-1 truncate text-sm", waiting ? "font-semibold" : "font-medium")}>{t.lead.full_name || t.lead.email}</span>
          <span title={dateTime(sentAt)} className={cn("shrink-0 text-xs", waiting ? "font-medium text-brand-700" : "text-muted-foreground")}>
            {timeAgo(sentAt)}
          </span>
          {waiting && (
            <span className="size-2 shrink-0 rounded-full bg-brand-500">
              <span className="sr-only">Needs reply</span>
            </span>
          )}
        </span>
        <span className="block truncate text-xs text-muted-foreground">{[t.lead.company_name, showProject && t.project_name].filter(Boolean).join(" · ")}</span>
        <span className={cn("mt-1 block truncate text-sm", waiting ? "text-foreground" : "text-foreground/65")}>
          {from && <span className="font-medium">{from}</span>}
          {m.body}
        </span>
        <span className="mt-1.5 flex flex-wrap gap-1">
          {t.classification && <ClassificationBadge value={t.classification} />}
          {t.pending_auto_reply && <Badge tone="amber">Reply draft</Badge>}
          {t.lead.remind_at && <ReminderBadge at={t.lead.remind_at} />}
        </span>
      </span>
    </button>
  );
}

/** The shape of the rows it stands in for, so the list doesn't jump when they arrive. */
function RowsSkeleton() {
  return (
    <div aria-busy className="flex flex-col">
      {Array.from({ length: 6 }, (_, i) => (
        <div key={i} className="flex gap-3 border-b border-border px-4 py-3">
          <Skeleton className="size-8 shrink-0 rounded-full" />
          <div className="flex min-w-0 flex-1 flex-col gap-2 pt-0.5">
            <Skeleton className="h-3.5 w-2/5" />
            <Skeleton className="h-3 w-1/3" />
            <Skeleton className="h-3.5 w-11/12" />
          </div>
        </div>
      ))}
    </div>
  );
}

/** A header and a few messages, where the thread will be. */
function ThreadSkeleton() {
  return (
    <div aria-busy className="flex min-h-0 flex-1 flex-col">
      <div className="flex flex-col gap-2 border-b border-border px-5 py-4">
        <Skeleton className="h-4 w-40" />
        <Skeleton className="h-3 w-64 max-w-full" />
      </div>
      <div className="flex flex-1 flex-col gap-3 bg-muted/40 px-5 py-5">
        <Skeleton className="h-16 w-[min(80%,36rem)] rounded-2xl" />
        <Skeleton className="h-12 w-[min(60%,28rem)] self-end rounded-2xl" />
        <Skeleton className="h-20 w-[min(80%,36rem)] rounded-2xl" />
      </div>
    </div>
  );
}

/** `onDone` runs when e marks the thread done, to move on to the next one. */
function ThreadPane({ leadId, onDone }: { leadId: string; onDone: () => void }) {
  const { mutate: mutateAll } = useSWRConfig();
  const { data, error, mutate } = useSWR<{
    lead: LeadDetail;
    messages: ThreadMessage[];
    draft_run: string | null;
    reply_cc: string[];
    send_later: SendLaterOffer;
  }>(`/inbox/threads/${leadId}`, fetcher);
  // An unsent reply stays with its lead in this tab, so opening another thread or leaving the inbox doesn't throw it away.
  const draftKey = `splee:reply:${leadId}`;
  const [reply, setReply] = useState(() => (typeof window === "undefined" ? "" : (sessionStorage.getItem(draftKey) ?? "")));
  useEffect(() => {
    if (reply) sessionStorage.setItem(draftKey, reply);
    else sessionStorage.removeItem(draftKey);
  }, [draftKey, reply]);
  const replyBox = useRef<HTMLTextAreaElement>(null);
  // Reply all: the lead's CC list from their last message until you change it.
  const [ccEdit, setCc] = useState<string[] | null>(null);
  const cc = ccEdit ?? data?.reply_cc ?? [];
  const uploads = useUploads();
  const [sending, setSending] = useState(false);
  const [draftRun, setDraftRun] = useState<string | null>(null);
  const [requesting, setRequesting] = useState(false);
  const [picking, setPicking] = useState(false);
  const [pickingSend, setPickingSend] = useState(false);
  const refresh = () => mutateAll((k) => typeof k === "string" && (k.startsWith("/inbox") || k.includes("/leads")));

  // A run started before this thread was opened, by the button or by a reply coming in, is picked up from the thread.
  const activeRun = draftRun ?? data?.draft_run ?? null;
  useAgentEvents(activeRun ? `/api/runs/${activeRun}/events` : null, (e) => {
    if (e.kind === "done" || e.kind === "error") {
      setDraftRun(null);
      if (e.kind === "error") toast.error(e.message || "Couldn't draft a reply");
      refresh();
    }
  });

  const messages = data?.messages ?? [];
  const pendingDraft = messages.find((m) => m.kind === "auto_reply" && m.status === "draft");
  // Texts Splee sent sit among the emails by time. Drafts and cancelled ones are on the Texts page.
  const texts = (data?.lead.texts ?? []).filter((t) => !["draft", "cancelled"].includes(t.status));
  // So are the calls someone made. Open ones are on the Calls page.
  const calls = (data?.lead.calls ?? []).filter((c) => c.status === "done");
  // And the sales meetings, with the agent's notes on them.
  const meetings = data?.lead.meetings ?? [];
  const conversation: (ThreadMessage | TextMessage | Call | Meeting)[] = [
    ...messages.filter((m) => m !== pendingDraft && m.status !== "cancelled"),
    ...texts,
    ...calls,
    ...meetings,
  ].sort(
    (a, b) => new Date(at(a)).getTime() - new Date(at(b)).getTime(),
  );
  // They last wrote by text, so the reply box answers on that channel.
  const textChannel = messages.findLast((m) => m.direction === "inbound")?.channel;
  const byText = textChannel === "sms" || textChannel === "whatsapp" ? textChannel : null;
  // The lead wrote last and nobody has dealt with it yet.
  const last = conversation.at(-1);
  const reminderDue = !!data?.lead.remind_at && isPast(data.lead.remind_at);
  const awaiting = (!!last && "direction" in last && last.direction === "inbound" && !last.handled_at) || reminderDue;

  const [marking, setMarking] = useState(false);
  const markDone = async () => {
    setMarking(true);
    try {
      await api(`/inbox/threads/${leadId}/done`, { method: "POST" });
      await refresh();
    } catch (e) {
      toastError(e);
    } finally {
      setMarking(false);
    }
  };
  // The button moves on like e does.
  const finish = () => {
    onDone();
    markDone();
  };
  // Only while the Done button shows: Done also clears a reminder, which e on a thread that isn't waiting shouldn't.
  useShortcut("e", awaiting ? finish : null);
  useShortcut("r", () => replyBox.current?.focus());
  // Opens at the latest message, the one to answer, and follows new ones, like a chat. The first email is at the top.
  // Someone who scrolled up to read an older one stays there.
  const thread = useRef<HTMLOListElement>(null);
  const atLatest = useRef(true);
  const shown = conversation.length + (pendingDraft ? 1 : 0) + (activeRun ? 1 : 0);
  useEffect(() => {
    if (thread.current && atLatest.current) thread.current.scrollTop = thread.current.scrollHeight;
  }, [shown]);
  // On a phone the thread mounts hidden behind the list and only gets a height once it's shown, so it scrolls then too.
  const observeThread = useCallback((el: HTMLOListElement | null) => {
    thread.current = el;
    if (!el) return;
    const observer = new ResizeObserver(() => {
      if (atLatest.current) el.scrollTop = el.scrollHeight;
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, []);

  if (error) return <LoadError error={error} retry={() => mutate()} className="flex-1" />;
  if (!data) return <ThreadSkeleton />;

  const { lead } = data;

  // An empty body clears the reminder.
  const remind = async (when: { days: number } | { at: string } | Record<string, never>) => {
    try {
      await api(`/inbox/threads/${leadId}/remind`, { body: when });
      await refresh();
      setPicking(false);
      toast.success(Object.keys(when).length ? "We'll bring this back to Needs reply then" : "Reminder cleared");
    } catch (e) {
      toastError(e);
    }
  };

  const setStatus = async (status: LeadStatus) => {
    try {
      await api(`/inbox/threads/${leadId}/status`, { body: { status } });
      await refresh();
    } catch (e) {
      toastError(e);
    }
  };

  const draftReply = async () => {
    setRequesting(true);
    try {
      const { run_id } = await api<{ run_id: string }>(`/inbox/threads/${leadId}/draft`, { method: "POST" });
      setDraftRun(run_id);
      // The cached thread learns about the run, so leaving and coming back keeps following it.
      await mutate();
    } catch (e) {
      toastError(e);
    } finally {
      setRequesting(false);
    }
  };
  // Bounces aren't replies, so there's nothing to answer.
  const canDraft = !byText && !pendingDraft && !activeRun && messages.some((m) => m.direction === "inbound" && m.classification !== "bounce");

  const canSend = !!reply.trim() && !uploads.uploading && !sending;
  /** `at` sends an email reply then instead of right away. */
  const send = async (at?: string) => {
    if (!canSend) return;
    setSending(true);
    try {
      const body = byText
        ? { body: reply.trim() }
        : { body: reply.trim(), cc_emails: cc, attachment_ids: uploads.files.flatMap((f) => (f.id ? [f.id] : [])), send_at: at };
      await api(`/inbox/threads/${leadId}/${byText ? "text" : "reply"}`, { body });
      // Cleared here too: if you opened another thread while it sent, this pane is gone and its effect won't run.
      sessionStorage.removeItem(draftKey);
      setReply("");
      setCc(null);
      uploads.clear();
      setPickingSend(false);
      await refresh();
      toast.success(at ? `Reply scheduled for ${dateTime(at)}` : "Reply queued");
    } catch (e) {
      toastError(e);
    } finally {
      setSending(false);
    }
  };

  const statusActions: { status: LeadStatus; label: string; icon: React.ReactNode }[] = [
    { status: "hot", label: "Hot", icon: <Flame className="size-3.5" /> },
    { status: "meeting_booked", label: "Meeting booked", icon: <CalendarCheck className="size-3.5" /> },
    { status: "not_interested", label: "Not interested", icon: <ThumbsDown className="size-3.5" /> },
  ];

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      {/* The status buttons drop under the name rather than squeezing it. When they wrap among themselves, each row fills. The
          basis is the name's 15rem plus the owner menu and bell beside it, so the name keeps its room before they drop. */}
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-5 py-3">
        <div className="flex min-w-0 flex-1 basis-112 items-center gap-3">
          <div className="min-w-0 flex-1">
            <p className="flex items-center gap-2 font-medium">
              {lead.full_name || lead.email} <LeadStatusBadge status={lead.status} />
            </p>
            <p className="truncate text-xs text-muted-foreground">
              {[lead.title, lead.company_name, lead.email].filter(Boolean).join(" · ")}
            </p>
            <LeadDeal leadId={lead.id} deal={lead.deal} onChange={refresh} className="mt-1 text-xs" />
            {lead.remind_at && (
              <p className="mt-1 flex items-center gap-1 text-xs text-amber-700">
                <Bell className="size-3" /> {reminderDue ? "Reminder: follow up now" : `Reminder ${dateTime(lead.remind_at)}`}
              </p>
            )}
          </div>
          <OwnerMenu
            ownerId={lead.owner_id}
            ownerName={lead.owner_name}
            onPick={async (owner_id) => {
              try {
                await api(`/leads/${lead.id}/owner`, { method: "PUT", body: { owner_id } });
                mutate();
                refresh();
              } catch (e) {
                toastError(e);
              }
            }}
            // A phone shows just the avatar, so the name beside it keeps its room. The button's label still says who.
            className="max-w-36 max-sm:[&_[data-owner-name]]:hidden"
          />
          {/* A bell button that opens a native menu: the select covers it unseen and resets after each pick. It sits beside the
              name, not with the status buttons, because one more button there took a row of its own on a phone. */}
          <label
            title="Remind me"
            className={buttonClass(
              "outline",
              "sm",
              cn("relative px-2.5 has-[select:focus-visible]:outline-2 has-[select:focus-visible]:outline-offset-2 has-[select:focus-visible]:outline-brand-600", lead.remind_at && "text-amber-700"),
            )}
          >
            <Bell className="size-3.5" />
            <select
              aria-label="Remind me"
              value=""
              onChange={(e) => {
                const v = e.target.value;
                if (v === "pick") setPicking(true);
                else remind(v === "clear" ? {} : { days: Number(v) });
              }}
              className="absolute inset-0 cursor-pointer opacity-0"
            >
              <option value="" disabled>
                Remind me
              </option>
              <option value="1">Tomorrow</option>
              <option value="3">In 3 days</option>
              <option value="7">In a week</option>
              <option value="pick">Pick a date and time…</option>
              {lead.remind_at && <option value="clear">Clear reminder</option>}
            </select>
          </label>
        </div>
        <div className="flex flex-wrap gap-1 [&>*]:grow">
          {statusActions.map((a) => (
            <Button
              key={a.status}
              size="sm"
              variant={lead.status === a.status ? "primary" : "outline"}
              onClick={() => setStatus(a.status)}
              disabled={lead.status === a.status}
            >
              {a.icon} {a.label}
            </Button>
          ))}
          {awaiting && (
            <Button size="sm" variant="outline" onClick={finish} loading={marking} title="Needs no reply. Takes it out of Needs reply until they write again.">
              <Check className="size-3.5" /> Done
            </Button>
          )}
        </div>
      </div>
      {picking && (
        <TimePicker
          title="Remind me"
          action="Set reminder"
          initial={lead.remind_at && !isPast(lead.remind_at) ? new Date(lead.remind_at) : tomorrowMorning()}
          days={365}
          onPick={(at) => remind({ at })}
          onClose={() => setPicking(false)}
        />
      )}
      {pickingSend && (
        <TimePicker
          title="Send later"
          action="Schedule reply"
          initial={new Date(data.send_later.at)}
          days={SEND_LATER_DAYS}
          onPick={(at) => send(at)}
          onClose={() => setPickingSend(false)}
        />
      )}
      <div className="border-b border-border px-5 py-2">
        <LeadNotes key={lead.id} lead={lead} onSaved={refresh} />
      </div>

      <ol
        ref={observeThread}
        onScroll={(e) => {
          const el = e.currentTarget;
          atLatest.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
        }}
        className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto bg-muted/40 px-5 py-5 focus-visible:-outline-offset-2"
      >
        {conversation.map((m) =>
          "direction" in m ? (
            <Bubble key={m.id} message={m} onChange={refresh} />
          ) : "outcome" in m ? (
            <li key={m.id} className="w-[min(80%,36rem)] self-end">
              <CallCard call={m} />
            </li>
          ) : "attendees" in m ? (
            <li key={m.id} className="w-[min(80%,36rem)] self-end">
              <MeetingCard meeting={m} onChange={refresh} />
            </li>
          ) : (
            <TextBubble key={m.id} text={m} />
          ),
        )}
        {pendingDraft && <PendingReply key={pendingDraft.id} draft={pendingDraft} onDone={refresh} />}
        {activeRun && (
          <li className="flex items-center gap-2 self-end rounded-2xl border border-dashed border-border px-4 py-3 text-sm text-muted-foreground">
            <Spinner /> Writing a reply…
          </li>
        )}
      </ol>

      {!byText && (
        <div className="flex flex-col gap-2 border-t border-border px-3 pt-2.5">
          <CcField value={cc} onChange={setCc} />
          {uploads.files.length > 0 && <UploadList uploads={uploads} />}
        </div>
      )}
      {/* The buttons drop their labels when the row is narrow, not just on phones: on a tablet or small laptop the thread
          sits beside the list, and the labels squeezed the reply box to a third of the row. On a small phone even the icons
          left it 100px wide, and with Send later too a 390px phone left it 130px, so up to 24rem the box takes the whole row and the
          buttons go under it. */}
      <div className={cn("@container flex flex-wrap items-end justify-end gap-2 p-3", byText ? "border-t border-border" : "pt-2")}>
        {/* Grows with the reply from two lines up to eight whole lines, then scrolls. 1.125rem is the padding plus border. */}
        <Textarea
          ref={replyBox}
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          // Read-only while it sends: what you typed meanwhile was thrown away with the sent reply.
          readOnly={sending}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) send();
            // Out of the box, so j, k and e work again without reaching for the mouse.
            if (e.key === "Escape") e.currentTarget.blur();
          }}
          placeholder={`${byText === "whatsapp" ? "Reply on WhatsApp to" : byText ? "Text" : "Reply to"} ${lead.first_name || lead.full_name || "lead"}…`}
          className="max-h-[calc(8lh+1.125rem)] min-h-[calc(2lh+1.125rem)] resize-none field-sizing-content @sm:flex-1"
        />
        <SavedReplyMenu
          lead={lead}
          onPick={(text) => {
            setReply((r) => (r.trim() ? `${r.trimEnd()}\n\n${text}` : text));
            replyBox.current?.focus();
          }}
        />
        {!byText && <AttachButton onPick={uploads.add} disabled={uploads.files.length >= MAX_FILES} />}
        {canDraft && !reply.trim() && (
          <Button variant="outline" onClick={draftReply} loading={requesting} aria-label="Draft reply" title="Have the agent write a reply for you to check and send">
            <Sparkles className="size-4" /> <span className="@max-lg:hidden">Draft reply</span>
          </Button>
        )}
        {!byText && <SendLaterMenu offer={data.send_later} disabled={!canSend} onPick={(at) => (at ? send(at) : setPickingSend(true))} />}
        <Button onClick={() => send()} loading={sending} disabled={!canSend} aria-label="Send reply">
          <Send className="size-4" /> <span className="@max-lg:hidden">Send</span>
        </Button>
      </div>
    </div>
  );
}

/** Fills a saved reply's {first_name} and {company} with the lead's, or a neutral word when the lead has none. */
function fillReply(body: string, lead: Lead) {
  return body.replaceAll("{first_name}", lead.first_name || "there").replaceAll("{company}", lead.company_name || "your company");
}

/** A button that opens a native menu of saved replies, like the reminder bell. */
function SavedReplyMenu({ lead, onPick }: { lead: Lead; onPick: (text: string) => void }) {
  const router = useRouter();
  const manage = `/app/p/${lead.project_id}/saved-replies`;
  const { data, error } = useSWR<SavedReply[]>(`/projects/${lead.project_id}/saved-replies`, fetcher);
  // A failed load mustn't read as "none yet", or people add the replies they already have again.
  const none = !data?.length && !error;
  const pick = (value: string) => {
    if (value === "manage") return router.push(manage);
    const saved = data?.find((r) => r.id === value);
    if (saved) onPick(fillReply(saved.body, lead));
  };
  return (
    <label
      title="Saved replies"
      className={buttonClass(
        "outline",
        "md",
        "relative shrink-0 px-3 has-[select:focus-visible]:outline-2 has-[select:focus-visible]:outline-offset-2 has-[select:focus-visible]:outline-brand-600",
      )}
    >
      <MessageSquareText className="size-4" />
      <select aria-label="Saved replies" value="" onChange={(e) => pick(e.target.value)} className="absolute inset-0 cursor-pointer opacity-0">
        <option value="" disabled>
          {error ? "Couldn't load your saved replies" : none ? "No saved replies yet" : "Insert a saved reply"}
        </option>
        {data?.map((r) => (
          <option key={r.id} value={r.id}>
            {r.title}
          </option>
        ))}
        <option value="manage">{none ? "Add saved replies…" : "Manage saved replies…"}</option>
      </select>
    </label>
  );
}

/** How far ahead a reply can wait, as SEND_LATER_DAYS in routes/inbox.rs. */
const SEND_LATER_DAYS = 30;

/** The time Send later offers first: 8:00 on the next weekday in the lead's time zone, or the project's. */
type SendLaterOffer = { at: string; time_zone: string; theirs: boolean };

/** "Tomorrow at 8:00 AM their time", or the weekday when it's further, read in the offer's time zone. */
function offerLabel({ at, time_zone, theirs }: SendLaterOffer) {
  const day = (d: Date) => d.toLocaleDateString("en-CA", { timeZone: time_zone });
  const when = new Date(at);
  const now = new Date();
  const name =
    day(when) === day(now)
      ? "Today"
      : day(when) === day(new Date(now.getTime() + 86_400_000))
        ? "Tomorrow"
        : when.toLocaleDateString("en-US", { timeZone: time_zone, weekday: "long" });
  const time = when.toLocaleTimeString("en-US", { timeZone: time_zone, hour: "numeric", minute: "2-digit" });
  const zone = theirs ? "their time" : `${time_zone.split("/").pop()?.replaceAll("_", " ")} time`;
  return `${name} at ${time} ${zone}`;
}

/** A clock button that opens a native menu like the saved replies: the offered time, or a picker for another. */
function SendLaterMenu({ offer, disabled, onPick }: { offer: SendLaterOffer; disabled: boolean; onPick: (at: string | null) => void }) {
  return (
    <label
      title="Send later"
      className={buttonClass(
        "outline",
        "md",
        "relative shrink-0 px-3 has-[select:disabled]:opacity-50 has-[select:focus-visible]:outline-2 has-[select:focus-visible]:outline-offset-2 has-[select:focus-visible]:outline-brand-600",
      )}
    >
      <Clock className="size-4" />
      <select
        aria-label="Send later"
        value=""
        disabled={disabled}
        onChange={(e) => onPick(e.target.value === "pick" ? null : e.target.value)}
        className="absolute inset-0 cursor-pointer opacity-0 disabled:cursor-default"
      >
        <option value="" disabled>
          Send later
        </option>
        <option value={offer.at}>{offerLabel(offer)}</option>
        <option value="pick">Pick a date and time…</option>
      </select>
    </label>
  );
}

/** A Date as a datetime-local input's value, in the browser's time zone. */
function localInput(d: Date) {
  return new Date(d.getTime() - d.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

/** Tomorrow at 9 in the morning, where a reminder's picker starts without one set. */
function tomorrowMorning() {
  const tomorrow = new Date();
  tomorrow.setDate(tomorrow.getDate() + 1);
  tomorrow.setHours(9, 0, 0, 0);
  return tomorrow;
}

/** Picks an exact time in the next `days`, for a reminder or a reply sent later. It starts on `initial`. */
function TimePicker({
  title,
  action,
  initial,
  days,
  onPick,
  onClose,
}: {
  title: string;
  action: string;
  initial: Date;
  days: number;
  onPick: (at: string) => Promise<void>;
  onClose: () => void;
}) {
  const [value, setValue] = useState(() => localInput(initial));
  const [saving, setSaving] = useState(false);
  const [left, setLeft] = useState(false);
  // When the picker opened. The server turns away a time that has passed since.
  const [now] = useState(() => Date.now());
  const at = value ? new Date(value) : null;
  const latest = now + days * 86_400_000;
  const valid = !!at && !Number.isNaN(at.getTime()) && at.getTime() > now && at.getTime() <= latest;
  return (
    <Modal open onClose={onClose} title={title} className="max-w-sm">
      <form
        className="grid gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!valid) return;
          setSaving(true);
          await onPick(at.toISOString());
          setSaving(false);
        }}
      >
        {/* Only after leaving the field: typing a date passes through past ones, like 01/10 on the way to 11/10. */}
        <Field label="When" error={left && !valid ? `Pick a time in the next ${days} days` : null}>
          <Input
            type="datetime-local"
            value={value}
            min={localInput(new Date(now))}
            max={localInput(new Date(latest))}
            onChange={(e) => setValue(e.target.value)}
            onBlur={() => setLeft(true)}
            required
          />
        </Field>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={!valid} loading={saving}>
            {action}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

function ReminderBadge({ at }: { at: string }) {
  return isPast(at) ? (
    <Badge tone="amber">
      <Bell className="size-3" /> Follow up
    </Badge>
  ) : (
    <Badge>
      <Bell className="size-3" /> {dateTime(at)}
    </Badge>
  );
}

/** When a message, text, call or meeting happened, for ordering the thread. */
function at(m: EmailMessage | TextMessage | Call | Meeting) {
  if ("outcome" in m) return m.called_at ?? m.closed_at ?? m.created_at;
  if ("attendees" in m) return m.started_at;
  return m.sent_at ?? ("scheduled_for" in m ? m.scheduled_for : m.due_at) ?? m.created_at;
}

const CHANNEL = { sms: "Text", whatsapp: "WhatsApp" } as const;

function TextBubble({ text: t }: { text: TextMessage }) {
  const failed = t.status === "failed";
  return (
    <li className="flex max-w-[min(80%,36rem)] flex-col items-end gap-1 self-end">
      <div className={cn("max-w-full rounded-2xl px-4 py-2.5 text-sm leading-relaxed whitespace-pre-wrap", failed ? "bg-red-50" : "bg-sky-100/70")}>
        <Message text={t.body} />
      </div>
      <span className="flex flex-wrap items-center justify-end gap-1.5 px-1 text-[11px] text-muted-foreground">
        {t.kind === "missed_call" ? "Missed-call text" : CHANNEL[t.channel]} ·{failed ? <span className="text-red-700">{t.error || "Failed"}</span> : t.status === "approved" || t.status === "sending" ? " Sending ·" : ""}{" "}
        {dateTime(at(t))}
      </span>
    </li>
  );
}

function Bubble({ message: m, onChange }: { message: ThreadMessage; onChange: () => Promise<unknown> }) {
  const inbound = m.direction === "inbound";
  // A reply sent later, still waiting. One sent right away is scheduled for now and goes within the minute.
  const waiting = !inbound && m.status === "scheduled" && !!m.scheduled_for && !isPast(m.scheduled_for);
  const [cancelling, setCancelling] = useState(false);
  const cancel = async () => {
    setCancelling(true);
    try {
      await api(`/emails/${m.id}`, { method: "PATCH", body: { status: "cancelled" } });
      await onChange();
      toast.success("Scheduled reply cancelled");
    } catch (e) {
      toastError(e);
      setCancelling(false);
    }
  };
  return (
    // 36rem caps a line at about 85 characters. At 80% alone, a wide screen ran lines to 150 and more.
    <li className={cn("flex max-w-[min(80%,36rem)] flex-col gap-1", inbound ? "self-start" : "self-end items-end")}>
      {/* max-w-full: aligned to an edge, the bubble sizes to its longest word, so a long link pushed it past the thread's
          other edge. Capped, the link wraps instead. */}
      <div className={cn("max-w-full rounded-2xl px-4 py-2.5 text-sm leading-relaxed whitespace-pre-wrap", inbound ? "bg-card shadow-card" : "bg-black/[0.06]")}>
        <Message text={m.body} />
      </div>
      <Attachments files={m.attachments} className={inbound ? "justify-start" : "justify-end"} />
      {m.cc_emails.length > 0 && <CcLine emails={m.cc_emails} className={cn("px-1", !inbound && "text-right")} />}
      <span className="flex items-center gap-1.5 px-1 text-[11px] text-muted-foreground">
        {inbound && m.classification && <ClassificationBadge value={m.classification} />}
        {m.channel !== "email" && `${CHANNEL[m.channel]} · `}
        {!inbound && m.kind === "auto_reply" && "Auto-reply · "}
        {!inbound && m.status !== "sent" && <EmailStatusBadge status={m.status} />}
        {dateTime(m.sent_at ?? m.scheduled_for ?? m.created_at)}
        {waiting && (
          <button onClick={cancel} disabled={cancelling} className="font-medium text-foreground underline-offset-2 hover:underline disabled:opacity-60">
            Cancel
          </button>
        )}
      </span>
    </li>
  );
}

function PendingReply({ draft, onDone }: { draft: ThreadMessage; onDone: () => Promise<unknown> }) {
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(draft.body);
  const [cc, setCc] = useState(draft.cc_emails);
  const [busy, setBusy] = useState<"approve" | "discard" | null>(null);

  const approve = async () => {
    setBusy("approve");
    try {
      await api(`/inbox/messages/${draft.id}/approve`, { body: editing ? { body, cc_emails: cc } : {} });
      await onDone();
      toast.success("Reply approved and queued");
    } catch (e) {
      toastError(e);
      setBusy(null);
    }
  };

  const discard = async () => {
    setBusy("discard");
    try {
      await api(`/emails/${draft.id}`, { method: "PATCH", body: { status: "cancelled" } });
      await onDone();
      toast.success("Draft discarded");
    } catch (e) {
      toastError(e);
      setBusy(null);
    }
  };

  return (
    <li className="flex w-[min(80%,36rem)] flex-col gap-2 self-end rounded-2xl border border-dashed border-amber-300 bg-amber-50/60 p-3">
      <MicroLabel className="text-amber-700">Auto-reply draft · waiting for approval</MicroLabel>
      {editing ? (
        <>
          {/* No max height: like the draft text it replaces, it scrolls with the conversation instead of inside itself. */}
          <Textarea value={body} onChange={(e) => setBody(e.target.value)} className="min-h-32 field-sizing-content" autoFocus />
          <CcField value={cc} onChange={setCc} />
        </>
      ) : (
        <>
          <p className="text-sm leading-relaxed whitespace-pre-wrap">
            <Message text={draft.body} />
          </p>
          {draft.cc_emails.length > 0 && <CcLine emails={draft.cc_emails} />}
        </>
      )}
      <Attachments files={draft.attachments} />
      {/* Like the email draft editor: on phones the buttons stretch to fill their row and Approve gets a row of its own. */}
      <div className="flex flex-wrap justify-end gap-2 max-sm:[&>*]:grow">
        <Button size="sm" variant="ghost" onClick={discard} loading={busy === "discard"} disabled={!!busy}>
          <X className="size-3.5" /> Discard
        </Button>
        {!editing && (
          <Button size="sm" variant="outline" onClick={() => setEditing(true)}>
            <Pencil className="size-3.5" /> Edit
          </Button>
        )}
        <Button size="sm" className="max-sm:basis-full" onClick={approve} loading={busy === "approve"} disabled={!body.trim() || !!busy}>
          <Send className="size-3.5" /> Approve &amp; send
        </Button>
      </div>
    </li>
  );
}

/** What a reply can carry. Keep in sync with ALLOWED in backend/src/mail/attachments.rs. */
const ATTACHABLE = [".pdf", ".png", ".jpg", ".jpeg", ".gif", ".webp", ".docx", ".pptx", ".xlsx", ".txt", ".csv"];
const MAX_FILES = 10;
const MAX_FILE_BYTES = 25 * 1024 * 1024;
const MAX_CC = 20;
const EMAIL = /^[^\s@,;<>"]+@[a-z0-9-]+(\.[a-z0-9-]+)+$/i;

function CcLine({ emails, className }: { emails: string[]; className?: string }) {
  return <p className={cn("text-[11px] break-words text-muted-foreground", className)}>Cc {emails.join(", ")}</p>;
}

/** A message's files. One Splee didn't keep shows its name and size, without a link. */
function Attachments({ files, className }: { files: EmailAttachment[]; className?: string }) {
  if (!files.length) return null;
  return (
    <ul className={cn("flex max-w-full flex-wrap gap-1.5", className)}>
      {files.map((f) => {
        const inner = (
          <>
            <FileText className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="min-w-0">
              <span className="block truncate">{f.filename}</span>
              <span className="block text-[11px] text-muted-foreground">{f.stored ? fileSize(f.size_bytes) : `${fileSize(f.size_bytes)} · Not kept, over 25 MB`}</span>
            </span>
          </>
        );
        const chip = "flex max-w-64 items-center gap-2 rounded-lg border px-2.5 py-1.5 text-xs";
        return (
          <li key={f.id} className="max-w-full">
            {f.stored ? (
              <a href={`/api/attachments/${f.id}`} download={f.filename} title={`Download ${f.filename}`} className={cn(chip, "border-border bg-card hover:bg-accent")}>
                {inner}
              </a>
            ) : (
              <span className={cn(chip, "border-dashed border-border-strong bg-transparent")}>{inner}</span>
            )}
          </li>
        );
      })}
    </ul>
  );
}

/** Adds what was typed to `list`: one address or several split by commas or spaces. `bad` is the first that isn't an address. */
function withCc(list: string[], typed: string): { list: string[]; bad: string | null } {
  let next = list;
  for (const part of typed.split(/[\s,;]+/).filter(Boolean)) {
    const email = part.toLowerCase();
    if (!EMAIL.test(email)) return { list: next, bad: part };
    if (!next.includes(email) && next.length < MAX_CC) next = [...next, email];
  }
  return { list: next, bad: null };
}

/** Who a reply copies in, each removable, with a box to add more. */
function CcField({ value, onChange }: { value: string[]; onChange: (cc: string[]) => void }) {
  const [typed, setTyped] = useState("");
  const [bad, setBad] = useState<string | null>(null);
  const commit = () => {
    if (!typed.trim()) return;
    const added = withCc(value, typed);
    onChange(added.list);
    setBad(added.bad);
    setTyped(added.bad ?? "");
  };
  return (
    <div>
      <div className="flex flex-wrap items-center gap-1.5 text-xs">
        <span className="font-medium text-muted-foreground">Cc</span>
        {value.map((email) => (
          <span key={email} className="inline-flex max-w-full items-center gap-0.5 rounded-full bg-black/[0.06] py-0.5 pr-0.5 pl-2.5">
            <span className="truncate">{email}</span>
            <button
              type="button"
              onClick={() => onChange(value.filter((e) => e !== email))}
              aria-label={`Don't copy in ${email}`}
              className="rounded-full p-1 text-muted-foreground hover:bg-black/[0.06] hover:text-foreground"
            >
              <X className="size-3" />
            </button>
          </span>
        ))}
        {value.length < MAX_CC && (
          <input
            value={typed}
            onChange={(e) => {
              setTyped(e.target.value);
              setBad(null);
            }}
            onKeyDown={(e) => {
              if (e.key === "Enter" || e.key === "," || e.key === " ") {
                e.preventDefault();
                commit();
              } else if (e.key === "Backspace" && !typed && value.length) {
                onChange(value.slice(0, -1));
              }
            }}
            onBlur={commit}
            type="email"
            aria-label="Copy someone in"
            aria-invalid={!!bad}
            placeholder={value.length ? "Add another" : "Copy someone in"}
            className="h-7 min-w-36 flex-1 rounded-md bg-transparent px-1 text-sm placeholder:text-muted-foreground focus-visible:outline-2 focus-visible:outline-brand-600"
          />
        )}
      </div>
      {bad && <p className="mt-1 text-xs text-red-700">{bad} isn&apos;t an email address</p>}
    </div>
  );
}

type Upload = { key: number; name: string; size: number; id: string | null };

/** Files for the reply, each uploaded as soon as it's picked so Send only passes their ids. */
function useUploads() {
  const [files, setFiles] = useState<Upload[]>([]);
  const next = useRef(0);
  const add = (picked: File[]) => {
    let room = MAX_FILES - files.length;
    for (const file of picked) {
      const dot = file.name.lastIndexOf(".");
      if (room <= 0) {
        toast.error(`Attach at most ${MAX_FILES} files to a reply`);
        break;
      }
      if (dot < 0 || !ATTACHABLE.includes(file.name.slice(dot).toLowerCase())) {
        toast.error(`Splee can't send ${file.name}. Attach a PDF, image, Word, PowerPoint, Excel, TXT or CSV file`);
        continue;
      }
      if (file.size > MAX_FILE_BYTES) {
        toast.error(`${file.name} is over 25 MB. Share a link to it instead`);
        continue;
      }
      room -= 1;
      const key = next.current++;
      setFiles((f) => [...f, { key, name: file.name, size: file.size, id: null }]);
      const form = new FormData();
      form.append("file", file);
      api<EmailAttachment>("/attachments", { body: form })
        .then((a) => setFiles((f) => f.map((u) => (u.key === key ? { ...u, id: a.id } : u))))
        .catch((e) => {
          setFiles((f) => f.filter((u) => u.key !== key));
          toastError(e);
        });
    }
  };
  return {
    files,
    add,
    remove: (key: number) => setFiles((f) => f.filter((u) => u.key !== key)),
    clear: () => setFiles([]),
    uploading: files.some((f) => !f.id),
  };
}

function UploadList({ uploads }: { uploads: ReturnType<typeof useUploads> }) {
  return (
    <ul className="flex flex-wrap gap-1.5">
      {uploads.files.map((f) => (
        <li key={f.key} className="flex max-w-64 items-center gap-2 rounded-lg border border-border bg-card py-1.5 pr-1 pl-2.5 text-xs">
          {f.id ? <FileText className="size-3.5 shrink-0 text-muted-foreground" /> : <Spinner className="size-3.5 shrink-0" />}
          <span className="min-w-0">
            <span className="block truncate">{f.name}</span>
            <span className="block text-[11px] text-muted-foreground">{f.id ? fileSize(f.size) : "Uploading…"}</span>
          </span>
          <button
            type="button"
            onClick={() => uploads.remove(f.key)}
            aria-label={`Remove ${f.name}`}
            className="shrink-0 rounded-md p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
          >
            <X className="size-3.5" />
          </button>
        </li>
      ))}
    </ul>
  );
}

/** A paperclip that opens the file picker, like the saved replies button opens its menu. */
function AttachButton({ onPick, disabled }: { onPick: (files: File[]) => void; disabled: boolean }) {
  return (
    <label
      title={disabled ? `A reply takes at most ${MAX_FILES} files` : "Attach files"}
      className={buttonClass(
        "outline",
        "md",
        cn(
          "relative shrink-0 px-3 has-[input:focus-visible]:outline-2 has-[input:focus-visible]:outline-offset-2 has-[input:focus-visible]:outline-brand-600",
          disabled ? "pointer-events-none opacity-50" : "cursor-pointer",
        ),
      )}
    >
      <Paperclip className="size-4" />
      <input
        type="file"
        multiple
        accept={ATTACHABLE.join(",")}
        aria-label="Attach files"
        disabled={disabled}
        onChange={(e) => {
          onPick(Array.from(e.target.files ?? []));
          e.target.value = "";
        }}
        className="sr-only"
      />
    </label>
  );
}
