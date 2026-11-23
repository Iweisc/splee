import { useLayoutEffect, useRef } from "react";

const focusable = "button, input, select, textarea, a[href], [tabindex]:not([tabindex='-1'])";
const carried = new Map<string, { label: string | null; index: number }>();

/**
 * For a card the page keys on what it saves, so a save mounts a new card in its place. Keyboard focus on the old card's
 * control would drop to the top of the page, so this hands it to the same control in the new one: the one with the same
 * aria-label, or else in the same place. Put the returned ref on the card's root.
 */
export function useKeepFocus<T extends HTMLElement>(id: string) {
  const ref = useRef<T>(null);
  // After every commit, not once, since a card can render nothing at first (Follow-ups while its steps load).
  useLayoutEffect(() => {
    const root = ref.current;
    const was = carried.get(id);
    carried.delete(id);
    if (root && was && document.activeElement === document.body) {
      const all = [...root.querySelectorAll<HTMLElement>(focusable)];
      (was.label ? all.find((e) => e.getAttribute("aria-label") === was.label) : all[was.index])?.focus();
    }
    return () => {
      const active = document.activeElement;
      if (!root || !(active instanceof HTMLElement) || !root.contains(active)) return;
      carried.set(id, { label: active.getAttribute("aria-label"), index: [...root.querySelectorAll(focusable)].indexOf(active) });
      // The new card mounts in the same commit. Anything later is some other visit, which shouldn't get focus moved.
      queueMicrotask(() => carried.delete(id));
    };
  });
  return ref;
}
