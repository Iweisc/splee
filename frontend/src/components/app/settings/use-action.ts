"use client";

import { useState } from "react";
import { toast } from "sonner";

export function toastError(e: unknown) {
  toast.error(e instanceof Error ? e.message : "Something went wrong");
}

/** Runs a mutation, toasts the outcome, and exposes which key is in flight so the right button can spin. */
export function useAction() {
  const [pending, setPending] = useState<string | null>(null);

  const run = async (key: string, fn: () => Promise<unknown>, success?: string) => {
    setPending(key);
    try {
      await fn();
      if (success) toast.success(success);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setPending(null);
    }
  };

  return { pending, run };
}
