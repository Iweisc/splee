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

  // The backend sets the alert failure fields and the CRM and verifier connections itself, so they come from the latest fetch even
  // under an unsaved draft.
  const settings =
    draft && data
      ? {
          ...draft,
          webhook_error: data.webhook_error,
          webhook_failed_at: data.webhook_failed_at,
          slack_error: data.slack_error,
          slack_failed_at: data.slack_failed_at,
          hubspot_connected: data.hubspot_connected,
          hubspot_error: data.hubspot_error,
          hubspot_failed_at: data.hubspot_failed_at,
          pipedrive_connected: data.pipedrive_connected,
          pipedrive_error: data.pipedrive_error,
          pipedrive_failed_at: data.pipedrive_failed_at,
          attio_connected: data.attio_connected,
          attio_error: data.attio_error,
          attio_failed_at: data.attio_failed_at,
          close_connected: data.close_connected,
          close_error: data.close_error,
          close_failed_at: data.close_failed_at,
          verifier: data.verifier,
          verifier_error: data.verifier_error,
          verifier_failed_at: data.verifier_failed_at,
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
      // The Schedule page's stats count days and hours in the saved time zone.
      mutateKey(`/projects/${projectId}/stats/weekdays`);
      mutateKey(`/projects/${projectId}/stats/hours`);
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
