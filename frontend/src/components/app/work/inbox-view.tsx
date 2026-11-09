"use client";

import { useState } from "react";
import { ArrowLeft, Bell, CalendarCheck, Check, Flame, Inbox, Pencil, Search, Send, Sparkles, ThumbsDown, X } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, buttonClass } from "@/components/ui/button";
import { Input, Select, Textarea } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState, Spinner, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { EmailMessage, InboxThread, LeadDetail, LeadStatus, Paginated, ProjectSummary } from "@/lib/types";
import { cn, dateTime, isPast, timeAgo } from "@/lib/utils";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, MicroLabel, Pager, PER_PAGE, toastError, useDebounced } from "./states";
import { LeadNotes } from "./lead-modal";
import { ClassificationBadge, EmailStatusBadge, LeadStatusBadge } from "./status";

type Filter = "all" | "hot" | "needs_reply" | "auto_replied" | "other";

/** Two-pane inbox. Without `projectId` it covers every project in the org. */
export function InboxView({ projectId, initialLead }: { projectId?: string; initialLead?: string }) {
  const [filter, setFilter] = useState<Filter>("all");
  const [project, setProject] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);
  const [selectedLead, setSelectedLead] = useState<string | null>(initialLead ?? null);
  const query = useDebounced(q);

  const { data: projects } = useSWR<ProjectSummary[]>(projectId ? null : "/projects", fetcher);
  const key = `/inbox${qs({ project_id: projectId ?? project, filter, q: query, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<InboxThread>>(key, fetcher, { keepPreviousData: true, refreshInterval: 60_000 });
  // A lead an alert linked to stays open even when it isn't on this page of the list. Otherwise a selection the
  // filter hides falls back to the first thread.
  const linked = selectedLead !== null && selectedLead === initialLead;
  const selectedId = data?.items.find((t) => t.lead.id === selectedLead)?.lead.id ?? (linked ? selectedLead : data?.items[0]?.lead.id);

  return (
    <Card className="grid min-h-[520px] flex-1 grid-cols-[minmax(0,1fr)] overflow-hidden md:grid-cols-[320px_minmax(0,1fr)] xl:grid-cols-[360px_minmax(0,1fr)]">
      {/* Phones show one pane at a time: the list, or the thread you tapped. */}
      <div className={cn("min-h-0 flex-col border-border md:flex md:border-r", selectedLead ? "hidden" : "flex")}>
        <div className="flex flex-col gap-2 border-b border-border p-3">
          <Tabs
            value={filter}
            onChange={(f) => {
              setFilter(f);
              setPage(1);
              setSelectedLead(null);
            }}
            options={[
              { value: "all", label: "All" },
              { value: "hot", label: "Hot" },
              { value: "needs_reply", label: "Needs reply" },
              { value: "auto_replied", label: "Auto-replied" },
              { value: "other", label: "Other" },
            ]}
            className="w-full [&>button]:flex-auto [&>button]:px-1.5 [&>button]:text-xs"
          />
          <div className="relative">
            <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
            <Input
              value={q}
              onChange={(e) => {
                setQ(e.target.value);
                setPage(1);
                setSelectedLead(null);
              }}
              placeholder="Search name, email or company"
              aria-label="Search replies"
              className="h-9 pl-9"
            />
          </div>
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
        <div className="min-h-0 flex-1 overflow-y-auto">
          {error ? (
            <LoadError error={error} retry={() => mutate()} />
          ) : !data ? (
            <ListSkeleton />
          ) : data.items.length === 0 ? (
            <EmptyState
              icon={query ? <Search className="size-5" /> : <Inbox className="size-5" />}
              title={query ? "No replies match your search" : filter === "all" ? "No replies yet" : "Nothing here"}
            >
              {!query && filter === "all" && "Replies to your emails land here. Splee checks your mailboxes every minute."}
            </EmptyState>
          ) : (
            <ul>
              {data.items.map((t) => (
                <li key={t.lead.id} className="border-b border-border">
                  <button
                    onClick={() => setSelectedLead(t.lead.id)}
                    className={cn("flex w-full gap-3 px-4 py-3 text-left hover:bg-accent", selectedId === t.lead.id && "bg-accent")}
                  >
                    <Avatar name={t.lead.full_name || t.lead.email || "?"} />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center justify-between gap-2">
                        <span className="truncate text-sm font-medium">{t.lead.full_name || t.lead.email}</span>
                        <span className="shrink-0 text-xs text-muted-foreground">{timeAgo(t.last_message.sent_at ?? t.last_message.created_at)}</span>
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">
                        {[t.lead.company_name, !projectId && t.project_name].filter(Boolean).join(" · ")}
                      </span>
                      <span className="mt-1 block truncate text-sm text-foreground/75">{t.last_message.body}</span>
                      <span className="mt-1.5 flex flex-wrap gap-1">
                        {t.classification && <ClassificationBadge value={t.classification} />}
                        {t.pending_auto_reply && <Badge tone="amber">Reply draft</Badge>}
                        {t.lead.remind_at && <ReminderBadge at={t.lead.remind_at} />}
                      </span>
                    </span>
                  </button>
                </li>
              ))}
            </ul>
          )}
        </div>
        {data && <Pager page={page} total={data.total} onChange={setPage} />}
      </div>
      <div className={cn("min-h-0 flex-col md:flex", selectedLead ? "flex" : "hidden")}>
        <button
          onClick={() => setSelectedLead(null)}
          className="flex items-center gap-1.5 border-b border-border px-4 py-2.5 text-sm text-muted-foreground hover:text-foreground md:hidden"
        >
          <ArrowLeft className="size-4" /> All replies
        </button>
        {selectedId ? (
          <ThreadPane key={selectedId} leadId={selectedId} />
        ) : (
          !!data?.items.length && <EmptyState className="flex-1" title="Select a conversation" />
        )}
      </div>
    </Card>
  );
}

function ThreadPane({ leadId }: { leadId: string }) {
  const { mutate: mutateAll } = useSWRConfig();
  const { data, error, mutate } = useSWR<{ lead: LeadDetail; messages: EmailMessage[]; draft_run: string | null }>(
    `/inbox/threads/${leadId}`,
    fetcher,
  );
  const [reply, setReply] = useState("");
  const [sending, setSending] = useState(false);
  const [draftRun, setDraftRun] = useState<string | null>(null);
  const [requesting, setRequesting] = useState(false);
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

  if (error) return <LoadError error={error} retry={() => mutate()} className="flex-1" />;
  if (!data) return <ListSkeleton className="flex-1" />;

  const { lead, messages } = data;
  const pendingDraft = messages.find((m) => m.kind === "auto_reply" && m.status === "draft");
  const conversation = messages.filter((m) => m !== pendingDraft && m.status !== "cancelled");
  // The lead wrote last and nobody has dealt with it yet.
  const last = conversation.at(-1);
  const reminderDue = !!lead.remind_at && isPast(lead.remind_at);
  const awaiting = (last?.direction === "inbound" && !last.handled_at) || reminderDue;

  const markDone = async () => {
    try {
      await api(`/inbox/threads/${leadId}/done`, { method: "POST" });
      await refresh();
    } catch (e) {
      toastError(e);
    }
  };

  const remind = async (days: number | null) => {
    try {
      await api(`/inbox/threads/${leadId}/remind`, { body: { days } });
      await refresh();
      toast.success(days ? "We'll bring this back to Needs reply then" : "Reminder cleared");
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
  const canDraft = !pendingDraft && !activeRun && messages.some((m) => m.direction === "inbound" && m.classification !== "bounce");

  const send = async () => {
    setSending(true);
    try {
      await api(`/inbox/threads/${leadId}/reply`, { body: { body: reply.trim() } });
      setReply("");
      await refresh();
      toast.success("Reply queued");
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
      {/* The status buttons drop under the name rather than squeezing it. When they wrap among themselves, each row fills. */}
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-5 py-3">
        <div className="min-w-0 flex-1 basis-60">
          <p className="flex items-center gap-2 font-medium">
            {lead.full_name || lead.email} <LeadStatusBadge status={lead.status} />
          </p>
          <p className="truncate text-xs text-muted-foreground">
            {[lead.title, lead.company_name, lead.email].filter(Boolean).join(" · ")}
          </p>
          {lead.remind_at && (
            <p className="mt-1 flex items-center gap-1 text-xs text-amber-700">
              <Bell className="size-3" /> {reminderDue ? "Reminder: follow up now" : `Reminder ${dateTime(lead.remind_at)}`}
            </p>
          )}
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
            <Button size="sm" variant="outline" onClick={markDone} title="Needs no reply. Takes it out of Needs reply until they write again.">
              <Check className="size-3.5" /> Done
            </Button>
          )}
          {/* A bell button that opens a native menu: the select covers it unseen and resets after each pick. A bell keeps
              it on the row with the status buttons. */}
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
              onChange={(e) => remind(e.target.value === "clear" ? null : Number(e.target.value))}
              className="absolute inset-0 cursor-pointer opacity-0"
            >
              <option value="" disabled>
                Remind me
              </option>
              <option value="1">Tomorrow</option>
              <option value="3">In 3 days</option>
              <option value="7">In a week</option>
              {lead.remind_at && <option value="clear">Clear reminder</option>}
            </select>
          </label>
        </div>
      </div>
      <div className="border-b border-border px-5 py-2">
        <LeadNotes key={lead.id} lead={lead} onSaved={refresh} />
      </div>

      <ol className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto bg-muted/40 px-5 py-5">
        {conversation.map((m) => (
          <Bubble key={m.id} message={m} />
        ))}
        {pendingDraft && <PendingReply key={pendingDraft.id} draft={pendingDraft} onDone={refresh} />}
        {activeRun && (
          <li className="flex items-center gap-2 self-end rounded-2xl border border-dashed border-border px-4 py-3 text-sm text-muted-foreground">
            <Spinner /> Writing a reply…
          </li>
        )}
      </ol>

      {/* The buttons drop their labels when the row is narrow, not just on phones: on a tablet or small laptop the thread
          sits beside the list, and the labels squeezed the reply box to a third of the row. */}
      <div className="@container flex items-end gap-2 border-t border-border p-3">
        {/* Grows with the reply from two lines up to eight whole lines, then scrolls. 1.125rem is the padding plus border. */}
        <Textarea
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && reply.trim()) send();
          }}
          placeholder={`Reply to ${lead.first_name || lead.full_name || "lead"}…`}
          className="max-h-[calc(8lh+1.125rem)] min-h-[calc(2lh+1.125rem)] resize-none field-sizing-content"
        />
        {canDraft && !reply.trim() && (
          <Button variant="outline" onClick={draftReply} loading={requesting} aria-label="Draft reply" title="Have the agent write a reply for you to check and send">
            {!requesting && <Sparkles className="size-4" />} <span className="@max-lg:hidden">Draft reply</span>
          </Button>
        )}
        <Button onClick={send} loading={sending} disabled={!reply.trim()} aria-label="Send reply">
          {!sending && <Send className="size-4" />} <span className="@max-lg:hidden">Send</span>
        </Button>
      </div>
    </div>
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

function Bubble({ message: m }: { message: EmailMessage }) {
  const inbound = m.direction === "inbound";
  return (
    // 36rem caps a line at about 85 characters. At 80% alone, a wide screen ran lines to 150 and more.
    <li className={cn("flex max-w-[min(80%,36rem)] flex-col gap-1", inbound ? "self-start" : "self-end items-end")}>
      {/* max-w-full: aligned to an edge, the bubble sizes to its longest word, so a long link pushed it past the thread's
          other edge. Capped, the link wraps instead. */}
      <div className={cn("max-w-full rounded-2xl px-4 py-2.5 text-sm leading-relaxed whitespace-pre-wrap", inbound ? "bg-card shadow-card" : "bg-black/[0.06]")}>
        {m.body}
      </div>
      <span className="flex items-center gap-1.5 px-1 text-[11px] text-muted-foreground">
        {inbound && m.classification && <ClassificationBadge value={m.classification} />}
        {!inbound && m.kind === "auto_reply" && "Auto-reply · "}
        {!inbound && m.status !== "sent" && <EmailStatusBadge status={m.status} />}
        {dateTime(m.sent_at ?? m.scheduled_for ?? m.created_at)}
      </span>
    </li>
  );
}

function PendingReply({ draft, onDone }: { draft: EmailMessage; onDone: () => Promise<unknown> }) {
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(draft.body);
  const [busy, setBusy] = useState<"approve" | "discard" | null>(null);

  const approve = async () => {
    setBusy("approve");
    try {
      await api(`/inbox/messages/${draft.id}/approve`, { body: editing ? { body } : {} });
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
        // No max height: like the draft text it replaces, it scrolls with the conversation instead of inside itself.
        <Textarea value={body} onChange={(e) => setBody(e.target.value)} className="min-h-32 field-sizing-content" autoFocus />
      ) : (
        <p className="text-sm leading-relaxed whitespace-pre-wrap">{draft.body}</p>
      )}
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
