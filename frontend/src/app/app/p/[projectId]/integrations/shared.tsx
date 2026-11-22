"use client";

import { useState } from "react";
import { toast } from "sonner";
import { useSWRConfig } from "swr";
import { toastError } from "@/components/app/work/states";
import { api } from "@/lib/api";
import type { ProjectSettings } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";

/**
 * Where the URL goes in each app, folded under a line naming the first few, so the list doesn't push the rest of the page
 * down for someone who uses one of them.
 */
export function AppSteps({ steps }: { steps: readonly (readonly [app: string, step: React.ReactNode])[] }) {
  return (
    <details className="group mt-1.5 text-xs text-muted-foreground">
      <summary className="w-fit cursor-pointer select-none hover:text-foreground">
        Where it goes in {steps.slice(0, 3).map(([app]) => app).join(", ")} and {steps.length - 3} more
      </summary>
      <ul className="mt-1.5 flex flex-col gap-1 text-pretty">
        {steps.map(([app, step]) => (
          <li key={app}>
            {app}: {step}
          </li>
        ))}
      </ul>
    </details>
  );
}

/** The backend clears this when an alert or a test to the saved URL goes through, or when the URL changes. */
export function LastFailure({ error, at, what = "alert" }: { error: string | null; at: string | null; what?: "alert" | "sync" | "check" | "send" | "lookup" | "copy" | "post" | "event" }) {
  if (!error || !at) return null;
  return (
    <div className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
      <p className="font-medium">
        The last {what} didn&apos;t go through, <span title={dateTime(at)}>{timeAgo(at)}</span>.
      </p>
      <p className="mt-1 font-mono">{error}</p>
    </div>
  );
}

/** Connects (PUT, with the secret) or disconnects (DELETE) an integration whose secret is write-only, then shows the saved settings. */
export function useConnection(projectId: string, path: string) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const call = async (body: Record<string, string | string[]> | undefined, done: string) => {
    setBusy(true);
    try {
      const fresh = await api<ProjectSettings>(`/projects/${projectId}/integrations/${path}`, { method: body ? "PUT" : "DELETE", body });
      await mutate(`/projects/${projectId}/settings`, fresh, { revalidate: false });
      toast.success(done);
      return true;
    } catch (e) {
      toastError(e);
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { busy, call };
}

/** For a field with its own Add or Connect button: Enter there presses it. A disabled button already stops an empty or repeat submit. */
export const submit = (run: () => void) => (e: React.FormEvent) => {
  e.preventDefault();
  run();
};

export const hostOf = (url: string) => {
  try {
    return new URL(url).hostname;
  } catch {
    return url;
  }
};

export const linkClass = "font-medium text-foreground underline underline-offset-4";
