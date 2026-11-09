"use client";

import { useEffect, useRef } from "react";

/**
 * j and k move the selection through a list like Gmail, unless you're typing or in a dialog. Below `minWidth` the
 * list and the open item don't sit side by side, so the keys do nothing there. Put the returned ref on the list and
 * `data-key={id}` on each row so the new row scrolls into view.
 */
export function useListKeys(ids: string[] | undefined, selectedId: string | undefined, select: (id: string) => void, minWidth: string) {
  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    const key = (e: KeyboardEvent) => {
      if ((e.key !== "j" && e.key !== "k") || e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return;
      if (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable], dialog, [role=dialog]")) return;
      if (!ids?.length || !matchMedia(`(min-width: ${minWidth})`).matches) return;
      const at = ids.indexOf(selectedId ?? "");
      const next = ids[Math.min(ids.length - 1, Math.max(0, at + (e.key === "j" ? 1 : -1)))];
      select(next);
      listRef.current?.querySelector(`[data-key="${next}"]`)?.scrollIntoView({ block: "nearest" });
    };
    document.addEventListener("keydown", key);
    return () => document.removeEventListener("keydown", key);
  }, [ids, selectedId, select, minWidth]);
  return listRef;
}

/** The line under a list that tells people about j and k. Hide it where useListKeys does nothing. */
export function ListKeysHint({ what, className }: { what: string; className: string }) {
  return (
    <p className={className}>
      Press <Kbd>j</Kbd> and <Kbd>k</Kbd> to move between {what}
    </p>
  );
}

function Kbd({ children }: { children: React.ReactNode }) {
  return <kbd className="rounded border border-border bg-muted px-1 font-mono text-[11px] text-foreground">{children}</kbd>;
}
