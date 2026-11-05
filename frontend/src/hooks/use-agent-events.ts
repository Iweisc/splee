"use client";

import { useEffect, useEffectEvent, useState } from "react";
import type { AgentEvent } from "@/lib/types";

interface Stream {
  url: string | null;
  events: AgentEvent[];
  finished: boolean;
}

/**
 * Subscribes to an agent SSE stream (/api/demo/:id/events or /api/runs/:id/events).
 * Reconnects with `after=<last id>` so no event is shown twice. Pass null to stay idle.
 * `onEvent` fires for every new event, e.g. to refetch state on `phase` events.
 */
export function useAgentEvents(url: string | null, onEvent?: (e: AgentEvent) => void) {
  const [stream, setStream] = useState<Stream>({ url, events: [], finished: false });
  const notify = useEffectEvent((e: AgentEvent) => onEvent?.(e));

  useEffect(() => {
    if (!url) return;

    let lastId = 0;
    let source: EventSource | null = null;
    let retry: ReturnType<typeof setTimeout> | undefined;
    let closed = false;

    const connect = () => {
      const sep = url.includes("?") ? "&" : "?";
      source = new EventSource(`${url}${sep}after=${lastId}`);
      source.onmessage = (msg) => {
        const e = JSON.parse(msg.data) as AgentEvent;
        if (e.id <= lastId) return;
        lastId = e.id;
        const done = e.kind === "done" || e.kind === "error";
        // State from a previous url is dropped here instead of resetting it inside the effect.
        setStream((prev) => (prev.url === url ? { url, events: [...prev.events, e], finished: done } : { url, events: [e], finished: done }));
        notify(e);
        if (done) {
          closed = true;
          source?.close();
        }
      };
      source.onerror = () => {
        source?.close();
        if (!closed) retry = setTimeout(connect, 2000);
      };
    };
    connect();

    return () => {
      closed = true;
      clearTimeout(retry);
      source?.close();
    };
  }, [url]);

  const current = stream.url === url ? stream : { events: [], finished: false };
  return { events: current.events, finished: current.finished };
}
