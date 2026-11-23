"use client";

import { useState } from "react";
import { toast } from "sonner";

export function toastError(e: unknown) {
  toast.error(e instanceof Error ? e.message : "Something went wrong");
}

/** Runs a mutation, toasts the outcome, and exposes which keys are in flight so the right buttons can spin. */
export function useAction() {
  const [keys, setKeys] = useState<string[]>([]);

  const run = async (key: string, fn: () => Promise<unknown>, success?: string) => {
    setKeys((k) => [...k, key]);
    try {
      await fn();
      if (success) toast.success(success);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      // Only this run's key: another row's save can still be in flight, like a second mailbox's warmup.
      setKeys((k) => k.filter((_, i) => i !== k.indexOf(key)));
    }
  };

  // `pending` is the latest still running, for callers with one thing to save at a time. `busy` is for rows that save apart.
  return { pending: keys.at(-1) ?? null, busy: (key: string) => keys.includes(key), run };
}
