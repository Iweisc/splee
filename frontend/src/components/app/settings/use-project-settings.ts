"use client";

import { useParams } from "next/navigation";
import { useState } from "react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings } from "@/lib/types";
import { toastError } from "./use-action";

export function useProjectSettings() {
  const { projectId } = useParams<{ projectId: string }>();
  const key = `/projects/${projectId}/settings`;
  const { data, error, mutate } = useSWR<ProjectSettings>(key, fetcher);
  const { mutate: mutateKey } = useSWRConfig();
  const [draft, setDraft] = useState<ProjectSettings | null>(null);
  const [saving, setSaving] = useState(false);

  // The backend sets the alert failure fields itself, so they come from the latest fetch even under an unsaved draft.
  const settings =
    draft && data
      ? {
          ...draft,
          webhook_error: data.webhook_error,
          webhook_failed_at: data.webhook_failed_at,
          slack_error: data.slack_error,
          slack_failed_at: data.slack_failed_at,
        }
      : (draft ?? data);
  const dirty = draft !== null && data !== undefined && JSON.stringify(settings) !== JSON.stringify(data);

  const update = (patch: Partial<ProjectSettings>) => {
    if (settings) setDraft((d) => ({ ...(d ?? settings), ...patch }));
  };

  const save = async () => {
    if (!draft) return;
    setSaving(true);
    try {
      const saved = await api<ProjectSettings>(key, { method: "PUT", body: { ...draft } });
      await mutate(saved, { revalidate: false });
      setDraft(null);
      // The overview reads settings through ProjectDetail, so refresh that copy too.
      mutateKey(`/projects/${projectId}`);
      // The Schedule page's weekday stats count days in the saved time zone.
      mutateKey(`/projects/${projectId}/stats/weekdays`);
      toast.success("Settings saved");
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return { projectId, settings, error, retry: () => mutate(), update, dirty, saving, save, reset: () => setDraft(null) };
}

export type ProjectSettingsForm = ReturnType<typeof useProjectSettings>;
