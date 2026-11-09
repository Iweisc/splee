"use client";

import { useEffect, useRef } from "react";

/** The key pressed, or null when it belongs to something else: a field you're typing in, a dialog, or a modifier shortcut. */
function shortcut(e: KeyboardEvent): string | null {
  if (e.metaKey || e.ctrlKey || e.altKey || e.defaultPrevented) return null;
  if (e.target instanceof Element && e.target.closest("input, textarea, select, [contenteditable], dialog, [role=dialog]")) return null;
  return e.key;
}

/** Calls `run` when `key` is pressed, with the same rules as j and k, and keeps the key from doing anything else. Null turns it off. */
export function useShortcut(key: string, run: (() => void) | null) {
  useEffect(() => {
    if (!run) return;
    const listener = (e: KeyboardEvent) => {
      if (shortcut(e) !== key) return;
      e.preventDefault();
      run();
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [key, run]);
}

/**
 * j and k move the selection through a list like Gmail, unless you're typing or in a dialog. Below `minWidth` the
 * list and the open item don't sit side by side, so the keys do nothing there. Put the returned ref on the list and
 * `data-key={id}` on each row so the new row scrolls into view.
 */
export function useListKeys(ids: string[] | undefined, selectedId: string | undefined, select: (id: string) => void, minWidth: string) {
  const listRef = useRef<HTMLUListElement>(null);
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      const key = shortcut(e);
      if ((key !== "j" && key !== "k") || !ids?.length || !matchMedia(`(min-width: ${minWidth})`).matches) return;
      const at = ids.indexOf(selectedId ?? "");
      const next = ids[Math.min(ids.length - 1, Math.max(0, at + (key === "j" ? 1 : -1)))];
      select(next);
      listRef.current?.querySelector(`[data-key="${next}"]`)?.scrollIntoView({ block: "nearest" });
    };
    document.addEventListener("keydown", listener);
    return () => document.removeEventListener("keydown", listener);
  }, [ids, selectedId, select, minWidth]);
  return listRef;
}

/**
 * The line under a list that tells people about j and k, and ? for the other keys. Any more and it wraps in the inbox's
 * list on tablets. Hide it where useListKeys does nothing.
 */
export function ListKeysHint({ className }: { className: string }) {
  return (
    <p className={className}>
      Press <Kbd>j</Kbd> and <Kbd>k</Kbd> to move, <Kbd>?</Kbd> for more
    </p>
  );
}

export function Kbd({ children }: { children: React.ReactNode }) {
  return <kbd className="rounded border border-border bg-muted px-1 font-mono text-[11px] text-foreground">{children}</kbd>;
}
