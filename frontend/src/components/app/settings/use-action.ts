"use client";

import { useRef, useState } from "react";
import { toast } from "sonner";

export function toastError(e: unknown) {
  toast.error(e instanceof Error ? e.message : "Something went wrong");
}

/** Runs a mutation, toasts the outcome, and exposes which keys are in flight so the right buttons can spin. */
export function useAction() {
  // One entry a run, so a key run twice at once stays busy until both finish.
  const [runs, setRuns] = useState<{ id: number; key: string }[]>([]);
  const next = useRef(0);

  const run = async (key: string, fn: () => Promise<unknown>, success?: string) => {
    const id = next.current++;
    setRuns((r) => [...r, { id, key }]);
    try {
      await fn();
      if (success) toast.success(success);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      // Only this run: another row's save can still be in flight, like a second mailbox's warmup.
      setRuns((r) => r.filter((x) => x.id !== id));
    }
  };

  // `pending` is the latest still running, for callers with one thing to save at a time. `busy` is for rows that save apart.
  return { pending: runs.at(-1)?.key ?? null, busy: (key: string) => runs.some((x) => x.key === key), run };
}
