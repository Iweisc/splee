"use client";

import { useEffect, useRef, useState } from "react";
import { MessageCircle, SendHorizontal, X } from "lucide-react";
import { api, ApiError } from "@/lib/api";
import type { SupportMessage } from "@/lib/types";
import { cn } from "@/lib/utils";

const VISITOR_KEY = "splee-visitor-token";
const CONVERSATION_KEY = "splee-support-conversation";

function visitorToken(): string {
  let token = localStorage.getItem(VISITOR_KEY);
  if (!token) {
    token = crypto.randomUUID();
    localStorage.setItem(VISITOR_KEY, token);
  }
  return token;
}

type Line = Pick<SupportMessage, "id" | "role" | "content">;

const greeting: Line = { id: "greeting", role: "assistant", content: "Hi! Ask me anything about how Splee works or what it costs." };

/** Floating bubble on public pages. `inSidebar` renders a sidebar item instead so it never covers app controls. */
export function SupportChat({ inSidebar = false }: { inSidebar?: boolean }) {
  const [open, setOpen] = useState(false);
  const [messages, setMessages] = useState<Line[]>([]);
  const [draft, setDraft] = useState("");
  const [sending, setSending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [messages, sending, open]);

  useEffect(() => {
    if (open) inputRef.current?.focus();
  }, [open]);

  const send = async () => {
    const content = draft.trim();
    if (!content || sending) return;
    setDraft("");
    setError(null);
    setSending(true);
    setMessages((m) => [...m, { id: `local-${Date.now()}`, role: "user", content }]);
    const post = (conversation_id?: string) =>
      api<{ conversation_id: string; messages: SupportMessage[] }>("/support/messages", {
        body: { conversation_id, visitor_token: visitorToken(), content },
      });
    try {
      const stored = localStorage.getItem(CONVERSATION_KEY) ?? undefined;
      const res = await post(stored).catch((e) => {
        // The stored conversation can be gone or malformed. Start a fresh one instead of failing.
        if (stored && e instanceof ApiError && (e.status === 404 || e.status === 422)) return post();
        throw e;
      });
      localStorage.setItem(CONVERSATION_KEY, res.conversation_id);
      setMessages(res.messages);
    } catch (e) {
      setError(e instanceof Error ? e.message : "Couldn't send. Try again.");
    } finally {
      setSending(false);
    }
  };

  const panel = open && (
    <div
      className={cn(
        "flex h-[min(520px,calc(100dvh-120px))] w-[min(360px,calc(100vw-48px))] animate-fade-up flex-col overflow-hidden rounded-2xl bg-card shadow-pop",
        inSidebar && "fixed bottom-4 left-4 z-50 lg:left-[252px]",
      )}
    >
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <div>
          <p className="font-medium">Splee support</p>
          <p className="text-xs text-muted-foreground">Answers come from an AI assistant</p>
        </div>
        <button onClick={() => setOpen(false)} className="rounded-md p-1 text-muted-foreground hover:bg-accent" aria-label="Close chat">
          <X className="size-4" />
        </button>
      </div>
      <div ref={listRef} className="flex flex-1 flex-col gap-2 overflow-y-auto p-4">
        {[greeting, ...messages].map((m) => (
          <p
            key={m.id}
            className={cn(
              "max-w-[85%] rounded-2xl px-3 py-2 text-sm leading-relaxed whitespace-pre-wrap",
              m.role === "user" ? "self-end bg-primary text-primary-foreground" : "self-start bg-accent",
            )}
          >
            {m.content}
          </p>
        ))}
        {sending && (
          <span className="flex gap-1 self-start rounded-2xl bg-accent px-3 py-3" aria-label="Typing">
            {[0, 150, 300].map((delay) => (
              <span key={delay} className="size-1.5 animate-bounce rounded-full bg-muted-foreground" style={{ animationDelay: `${delay}ms` }} />
            ))}
          </span>
        )}
        {error && <p className="self-start text-xs text-danger">{error}</p>}
      </div>
      <form
        className="flex items-end gap-2 border-t border-border p-3"
        onSubmit={(e) => {
          e.preventDefault();
          send();
        }}
      >
        {/* Grows with the message up to five whole lines, then scrolls. 1.125rem is the padding plus border. */}
        <textarea
          ref={inputRef}
          value={draft}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey) {
              e.preventDefault();
              send();
            }
          }}
          rows={1}
          placeholder="Type a message…"
          className="max-h-[calc(5lh+1.125rem)] min-h-9 flex-1 resize-none field-sizing-content rounded-lg border border-border-strong px-3 py-2 text-sm outline-none focus:border-foreground/40"
        />
        <button
          type="submit"
          disabled={!draft.trim() || sending}
          className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground disabled:opacity-40"
          aria-label="Send"
        >
          <SendHorizontal className="size-4" />
        </button>
      </form>
    </div>
  );

  if (inSidebar) {
    return (
      <>
        <button
          onClick={() => setOpen((o) => !o)}
          className="flex w-full items-center gap-2.5 rounded-lg px-2 py-1.5 text-sm text-foreground/75 hover:bg-accent hover:text-foreground"
          aria-expanded={open}
        >
          <MessageCircle className="size-4" />
          Help & support
        </button>
        {panel}
      </>
    );
  }

  return (
    <div className="fixed right-6 bottom-6 z-50 flex flex-col items-end gap-3">
      {panel}
      <button
        onClick={() => setOpen((o) => !o)}
        className="flex size-14 items-center justify-center rounded-full bg-brand-600 text-white shadow-pop transition-transform hover:scale-105"
        aria-label={open ? "Close support chat" : "Open support chat"}
        aria-expanded={open}
      >
        {open ? <X className="size-6" /> : <MessageCircle className="size-6" />}
      </button>
    </div>
  );
}
