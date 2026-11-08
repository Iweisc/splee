"use client";

import { useState } from "react";
import { ArrowLeft, CalendarCheck, Flame, Inbox, Pencil, Send, ThumbsDown } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Select, Textarea } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { EmailMessage, InboxThread, LeadDetail, LeadStatus, Paginated, ProjectSummary } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, MicroLabel, Pager, PER_PAGE, toastError } from "./states";
import { ClassificationBadge, EmailStatusBadge, LeadStatusBadge } from "./status";

type Filter = "all" | "hot" | "needs_reply" | "auto_replied" | "other";

/** Two-pane inbox. Without `projectId` it covers every project in the org. */
export function InboxView({ projectId }: { projectId?: string }) {
  const [filter, setFilter] = useState<Filter>("all");
  const [project, setProject] = useState("");
  const [page, setPage] = useState(1);
  const [selectedLead, setSelectedLead] = useState<string | null>(null);

  const { data: projects } = useSWR<ProjectSummary[]>(projectId ? null : "/projects", fetcher);
  const key = `/inbox${qs({ project_id: projectId ?? project, filter, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<InboxThread>>(key, fetcher, { keepPreviousData: true, refreshInterval: 60_000 });
  const selected = data?.items.find((t) => t.lead.id === selectedLead) ?? data?.items[0];

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
          {!projectId && (
            <Select
              value={project}
              onChange={(e) => {
                setProject(e.target.value);
                setPage(1);
                setSelectedLead(null);
              }}
              className="h-9"
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
            <EmptyState icon={<Inbox className="size-5" />} title={filter === "all" ? "No replies yet" : "Nothing here"}>
              {filter === "all" && "Replies to your emails land here. Splee checks your mailboxes every minute."}
            </EmptyState>
          ) : (
            <ul>
              {data.items.map((t) => (
                <li key={t.lead.id} className="border-b border-border">
                  <button
                    onClick={() => setSelectedLead(t.lead.id)}
                    className={cn("flex w-full gap-3 px-4 py-3 text-left hover:bg-accent", selected?.lead.id === t.lead.id && "bg-accent")}
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
        {selected ? (
          <ThreadPane key={selected.lead.id} leadId={selected.lead.id} />
        ) : (
          !!data?.items.length && <EmptyState className="flex-1" title="Select a conversation" />
        )}
      </div>
    </Card>
  );
}

function ThreadPane({ leadId }: { leadId: string }) {
  const { mutate: mutateAll } = useSWRConfig();
  const { data, error, mutate } = useSWR<{ lead: LeadDetail; messages: EmailMessage[] }>(`/inbox/threads/${leadId}`, fetcher);
  const [reply, setReply] = useState("");
  const [sending, setSending] = useState(false);

  if (error) return <LoadError error={error} retry={() => mutate()} className="flex-1" />;
  if (!data) return <ListSkeleton className="flex-1" />;

  const { lead, messages } = data;
  const pendingDraft = messages.find((m) => m.kind === "auto_reply" && m.status === "draft");
  const conversation = messages.filter((m) => m !== pendingDraft && m.status !== "cancelled");

  const refresh = () => mutateAll((k) => typeof k === "string" && (k.startsWith("/inbox") || k.includes("/leads")));

  const setStatus = async (status: LeadStatus) => {
    try {
      await api(`/inbox/threads/${leadId}/status`, { body: { status } });
      await refresh();
    } catch (e) {
      toastError(e);
    }
  };

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
        </div>
      </div>

      <ol className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto bg-muted/40 px-5 py-5">
        {conversation.map((m) => (
          <Bubble key={m.id} message={m} />
        ))}
        {pendingDraft && <PendingReply key={pendingDraft.id} draft={pendingDraft} onDone={refresh} />}
      </ol>

      <div className="flex items-end gap-2 border-t border-border p-3">
        <Textarea
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey) && reply.trim()) send();
          }}
          placeholder={`Reply to ${lead.first_name || lead.full_name || "lead"}…`}
          className="min-h-12 resize-none"
          rows={2}
        />
        <Button onClick={send} loading={sending} disabled={!reply.trim()} aria-label="Send reply">
          {!sending && <Send className="size-4" />} Send
        </Button>
      </div>
    </div>
  );
}

function Bubble({ message: m }: { message: EmailMessage }) {
  const inbound = m.direction === "inbound";
  return (
    <li className={cn("flex max-w-[80%] flex-col gap-1", inbound ? "self-start" : "self-end items-end")}>
      <div className={cn("rounded-2xl px-4 py-2.5 text-sm leading-relaxed whitespace-pre-wrap", inbound ? "bg-card shadow-card" : "bg-black/[0.06]")}>
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
  const [busy, setBusy] = useState(false);

  const approve = async () => {
    setBusy(true);
    try {
      await api(`/inbox/messages/${draft.id}/approve`, { body: editing ? { body } : {} });
      await onDone();
      toast.success("Reply approved and queued");
    } catch (e) {
      toastError(e);
      setBusy(false);
    }
  };

  return (
    <li className="flex w-[80%] flex-col gap-2 self-end rounded-2xl border border-dashed border-amber-300 bg-amber-50/60 p-3">
      <MicroLabel className="text-amber-700">Auto-reply draft · waiting for approval</MicroLabel>
      {editing ? (
        <Textarea value={body} onChange={(e) => setBody(e.target.value)} className="min-h-32" autoFocus />
      ) : (
        <p className="text-sm leading-relaxed whitespace-pre-wrap">{draft.body}</p>
      )}
      <div className="flex justify-end gap-2">
        {!editing && (
          <Button size="sm" variant="outline" onClick={() => setEditing(true)}>
            <Pencil className="size-3.5" /> Edit
          </Button>
        )}
        <Button size="sm" onClick={approve} loading={busy} disabled={!body.trim()}>
          <Send className="size-3.5" /> Approve &amp; send
        </Button>
      </div>
    </li>
  );
}
