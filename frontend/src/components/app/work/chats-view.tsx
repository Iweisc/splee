"use client";

import Link from "next/link";
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { ArrowLeft, Bot, Hand, MessagesSquare, Send, UserRound } from "lucide-react";
import useSWR from "swr";
import { Button, LinkButton } from "@/components/ui/button";
import { Textarea } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { ChatConversation, ChatMessage, Paginated } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, Pager, PER_PAGE, toastError, useLastPageGuard } from "./states";

type Thread = { chat: ChatConversation; messages: ChatMessage[] };

const who = (c: ChatConversation) => c.name || c.email || (c.company ? `Visitor from ${c.company}` : "Visitor");
const pageName = (path: string) => (path === "/" ? "Home page" : path);

/** Calls `onChange` with the conversation's id, or null when some were missed, each time one changes in this project. */
function useChatEvents(projectId: string, onChange: (conversation: string | null) => void) {
  const notify = useEffectEvent(onChange);
  useEffect(() => {
    let source: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let closed = false;
    const connect = () => {
      source = new EventSource(`/api/projects/${projectId}/chats/events`);
      source.onmessage = (msg) => notify((JSON.parse(msg.data) as { conversation?: string }).conversation ?? null);
      source.onerror = () => {
        source?.close();
        if (!closed) retry = setTimeout(connect, 3000);
      };
    };
    connect();
    return () => {
      closed = true;
      clearTimeout(retry);
      source?.close();
    };
  }, [projectId]);
}

/** The Inbox's Chats tab: conversations from the website chat, live ones updating as they happen. */
export function ChatsView({ projectId, initialChat }: { projectId: string; initialChat?: string }) {
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<string | null>(initialChat ?? null);
  const key = `/projects/${projectId}/chats${qs({ page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<ChatConversation>>(key, fetcher, { keepPreviousData: true });
  const [changed, setChanged] = useState<{ id: string | null; at: number }>({ id: null, at: 0 });
  useLastPageGuard(data?.total, page, setPage);
  useChatEvents(projectId, (id) => {
    mutate();
    setChanged({ id, at: Date.now() });
  });
  const selectedId = selected ?? data?.items[0]?.id;

  return (
    <Card className="grid min-h-[520px] flex-1 grid-cols-[minmax(0,1fr)] overflow-hidden md:grid-cols-[320px_minmax(0,1fr)] xl:grid-cols-[360px_minmax(0,1fr)]">
      <div className={cn("min-h-0 flex-col border-border md:flex md:border-r", selected ? "hidden" : "flex")}>
        <div className="min-h-0 flex-1 overflow-y-auto">
          {error ? (
            <LoadError error={error} retry={() => mutate()} what="chats" />
          ) : !data ? (
            <ListSkeleton />
          ) : data.items.length === 0 ? (
            <EmptyState icon={<MessagesSquare className="size-5" />} title="No chats yet">
              Turn on the <Link href={`/app/p/${projectId}/website-chat`} className="underline">website chat</Link> and add its script to your site.
              Conversations show up here as they happen.
            </EmptyState>
          ) : (
            <ul>
              {data.items.map((c) => (
                <li key={c.id} className="border-b border-border">
                  <button
                    onClick={() => setSelected(c.id)}
                    className={cn("flex w-full gap-3 px-4 py-3 text-left hover:bg-accent focus-visible:-outline-offset-2", selectedId === c.id && "bg-accent")}
                  >
                    <Avatar name={c.name || c.email || "?"} />
                    <span className="min-w-0 flex-1">
                      <span className="flex items-center justify-between gap-2">
                        <span className="truncate text-sm font-medium">{who(c)}</span>
                        <span className="shrink-0 text-xs text-muted-foreground">{timeAgo(c.last_message_at)}</span>
                      </span>
                      <span className="block truncate text-xs text-muted-foreground">{[c.company, c.page && pageName(c.page)].filter(Boolean).join(" · ")}</span>
                      <span className="mt-1 block truncate text-sm text-foreground/75">{c.last_message}</span>
                      <span className="mt-1.5 flex flex-wrap gap-1">
                        {c.live && <Badge tone="green">Live</Badge>}
                        {c.lead_id && <Badge tone="amber">Lead</Badge>}
                        {c.taken_over && <Badge>You&apos;re answering</Badge>}
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
      <div className={cn("min-h-0 flex-col md:flex", selected ? "flex" : "hidden")}>
        <button
          onClick={() => setSelected(null)}
          className="flex items-center gap-1.5 border-b border-border px-4 py-2.5 text-sm text-muted-foreground hover:text-foreground md:hidden"
        >
          <ArrowLeft className="size-4" /> All chats
        </button>
        {selectedId ? (
          <ChatPane
            key={selectedId}
            id={selectedId}
            projectId={projectId}
            changedAt={changed.id === selectedId || changed.id === null ? changed.at : 0}
            onChange={() => mutate()}
          />
        ) : (
          !!data?.items.length && <EmptyState className="flex-1" title="Select a conversation" />
        )}
      </div>
    </Card>
  );
}

/** `changedAt` moves when the events stream says this conversation changed, which refetches it. */
function ChatPane({ id, projectId, changedAt, onChange }: { id: string; projectId: string; changedAt: number; onChange: () => void }) {
  const key = `/chats/${id}`;
  const { data, error, mutate } = useSWR<Thread>(key, fetcher);
  const [reply, setReply] = useState("");
  const [busy, setBusy] = useState<"send" | "takeover" | null>(null);
  useEffect(() => {
    if (changedAt) mutate();
  }, [changedAt, mutate]);
  // Opens at the latest message and follows new ones.
  const list = useRef<HTMLDivElement>(null);
  const count = data?.messages.length;
  useEffect(() => {
    if (list.current) list.current.scrollTop = list.current.scrollHeight;
  }, [count]);

  if (error) return <LoadError error={error} retry={() => mutate()} className="flex-1" />;
  if (!data) return <ListSkeleton className="flex-1" />;
  const { chat, messages } = data;

  const act = async (what: "send" | "takeover", path: string, body: Record<string, unknown>) => {
    setBusy(what);
    try {
      await mutate(await api<Thread>(path, { body }), { revalidate: false });
      if (what === "send") setReply("");
      onChange();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col">
      <div className="flex flex-wrap items-center gap-3 border-b border-border px-5 py-3">
        <div className="min-w-0 flex-1 basis-60">
          <p className="flex items-center gap-2 font-medium">
            <span className="truncate">{who(chat)}</span>
            {chat.live && <Badge tone="green">Live</Badge>}
          </p>
          <p className="truncate text-xs text-muted-foreground">
            {[chat.email, chat.company, chat.domain, chat.page && (chat.page === "/" ? "on the home page" : `on ${chat.page}`)].filter(Boolean).join(" · ")}
          </p>
        </div>
        <div className="flex flex-wrap gap-2">
          {chat.lead_id && (
            <LinkButton href={`/app/p/${projectId}/inbox?lead=${chat.lead_id}`} variant="outline" size="sm">
              <UserRound className="size-3.5" /> Open lead
            </LinkButton>
          )}
          <Button
            variant="outline"
            size="sm"
            loading={busy === "takeover"}
            onClick={() => act("takeover", `/chats/${id}/takeover`, { on: !chat.taken_over })}
            title={chat.taken_over ? "Let the assistant answer again" : "Answer this visitor yourself"}
          >
            {chat.taken_over ? <Bot className="size-3.5" /> : <Hand className="size-3.5" />}
            {chat.taken_over ? "Hand back" : "Take over"}
          </Button>
        </div>
      </div>
      <div ref={list} className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto bg-black/[0.02] px-5 py-4">
        {messages.map((m) => (
          <div key={m.id} className={cn("flex max-w-[80%] flex-col gap-1", m.role === "visitor" ? "self-start" : "self-end items-end")}>
            <span className="text-xs text-muted-foreground">
              {m.role === "visitor" ? "Visitor" : m.role === "team" ? m.who || "Team" : m.role === "bot" ? "Assistant" : "Splee"} · {dateTime(m.created_at)}
            </span>
            <p
              className={cn(
                "rounded-2xl px-3.5 py-2 text-sm whitespace-pre-wrap [overflow-wrap:anywhere]",
                m.role === "visitor" && "rounded-bl-sm border border-border bg-card",
                m.role === "bot" && "rounded-br-sm bg-black/[0.06]",
                m.role === "team" && "rounded-br-sm bg-primary text-primary-foreground",
                m.role === "notice" && "rounded-br-sm border border-dashed border-border-strong text-muted-foreground",
              )}
            >
              {m.body}
            </p>
          </div>
        ))}
      </div>
      <form
        className="flex flex-col gap-2 border-t border-border p-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (reply.trim()) act("send", `/chats/${id}/reply`, { body: reply.trim() });
        }}
      >
        <Textarea
          value={reply}
          onChange={(e) => setReply(e.target.value)}
          rows={2}
          maxLength={2000}
          placeholder="Write a reply"
          aria-label="Reply"
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) e.currentTarget.form?.requestSubmit();
          }}
        />
        <div className="flex flex-wrap items-center justify-between gap-x-3 gap-y-2">
          <p className="flex-1 basis-56 text-xs text-pretty text-muted-foreground">
            {chat.taken_over ? "You're answering. The visitor sees your replies within a few seconds." : "Replying takes over from the assistant."}
          </p>
          <Button type="submit" size="sm" loading={busy === "send"} disabled={!reply.trim()} className="max-sm:flex-auto">
            <Send className="size-3.5" /> Send
          </Button>
        </div>
      </form>
    </div>
  );
}
