"use client";

import { useParams } from "next/navigation";
import { useState } from "react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { CRM_NAMES, type ProjectSettings } from "@/lib/types";
import { toastError } from "./use-action";

// The backend sets the alert failures and the CRM and verifier connections itself, so these come from the latest fetch even
// under an unsaved draft.
const FROM_SERVER: (keyof ProjectSettings)[] = [
  "webhook_error",
  "webhook_failed_at",
  ...CRM_NAMES.flatMap((crm) => [`${crm}_connected`, `${crm}_error`, `${crm}_failed_at`] as const),
  "verifier",
  "verifier_error",
  "verifier_failed_at",
];

export function useProjectSettings() {
  const { projectId } = useParams<{ projectId: string }>();
  const key = `/projects/${projectId}/settings`;
  const { data, error, mutate } = useSWR<ProjectSettings>(key, fetcher);
  const { mutate: mutateKey } = useSWRConfig();
  const [draft, setDraft] = useState<ProjectSettings | null>(null);
  const [saving, setSaving] = useState(false);

  const settings: ProjectSettings | undefined =
    draft && data ? { ...draft, ...Object.fromEntries(FROM_SERVER.map((field) => [field, data[field]])) } : (draft ?? data);
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
