"use client";

import { useEffect } from "react";

/**
 * Asks before leaving while `dirty`: closing or reloading the tab, and following a link on the page. The App Router
 * has no way to block its own navigation, so links are caught on the document before Next's Link handles them.
 * The browser's back button still leaves without asking.
 */
export function useLeaveGuard(dirty: boolean) {
  useEffect(() => {
    if (!dirty) return;
    const unload = (e: BeforeUnloadEvent) => e.preventDefault();
    const click = (e: MouseEvent) => {
      const link = (e.target as Element).closest("a[href]");
      // A new tab or a download leaves this page and its changes alone.
      if (!link || link.getAttribute("target") === "_blank" || link.hasAttribute("download")) return;
      if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
      if (!window.confirm("Leave without saving your changes?")) {
        e.preventDefault();
        e.stopPropagation();
      }
    };
    window.addEventListener("beforeunload", unload);
    document.addEventListener("click", click, true);
    return () => {
      window.removeEventListener("beforeunload", unload);
      document.removeEventListener("click", click, true);
    };
  }, [dirty]);
}
