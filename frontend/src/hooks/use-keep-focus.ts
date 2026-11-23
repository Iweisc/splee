import { useLayoutEffect, useRef } from "react";

const carried = new Map<string, string>();
const nameOf = (e: Element) => e.getAttribute("aria-label") ?? e.textContent?.trim();

/**
 * For a card the page keys on what it saves, so a save mounts a new card in its place. Keyboard focus on one of the old
 * card's `names` buttons, like its own switch or its Save, would drop to the top of the page, so this hands it to the button
 * of that name in the new card. Only those: after a change, another control in the same place, like a step's Remove, may
 * not be the same one. `key` names the card and campaign. Put the returned ref on the card's root.
 */
export function useKeepFocus<T extends HTMLElement>(key: string, names: string[]) {
  const ref = useRef<T>(null);
  // After every commit, not once, since a card can render nothing at first (Follow-ups while its steps load).
  useLayoutEffect(() => {
    const root = ref.current;
    const find = (name: string) => [...(root?.querySelectorAll<HTMLElement>("button") ?? [])].find((e) => nameOf(e) === name);
    const was = carried.get(key);
    carried.delete(key);
    if (was && document.activeElement === document.body) find(was)?.focus();
    return () => {
      const name = names.find((n) => find(n) === document.activeElement);
      if (!name) return;
      carried.set(key, name);
      // The new card mounts in the same commit. Anything later is some other visit, which shouldn't get focus moved.
      queueMicrotask(() => carried.delete(key));
    };
  });
  return ref;
}
