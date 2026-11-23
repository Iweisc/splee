import { useLayoutEffect, useRef } from "react";

const carried = new Set<string>();

/**
 * For a card the page keys on what it saves, so a save mounts a new card in its place. Keyboard focus on the old card's
 * own switch, the one labelled `label`, would drop to the top of the page, so this hands it to that switch in the new card.
 * Only that switch: after a change, another control in the same place, like a step's Remove, may not be the same one.
 * `key` names the card and campaign. Put the returned ref on the card's root.
 */
export function useKeepFocus<T extends HTMLElement>(key: string, label: string) {
  const ref = useRef<T>(null);
  // After every commit, not once, since a card can render nothing at first (Follow-ups while its steps load).
  useLayoutEffect(() => {
    const root = ref.current;
    const target = () => [...(root?.querySelectorAll<HTMLElement>('[role="switch"]') ?? [])].find((e) => e.getAttribute("aria-label") === label);
    if (carried.delete(key) && document.activeElement === document.body) target()?.focus();
    return () => {
      if (target() !== document.activeElement) return;
      carried.add(key);
      // The new card mounts in the same commit. Anything later is some other visit, which shouldn't get focus moved.
      queueMicrotask(() => carried.delete(key));
    };
  });
  return ref;
}
