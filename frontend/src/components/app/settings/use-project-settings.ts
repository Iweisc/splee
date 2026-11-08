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

  const settings = draft ?? data;
  const dirty = draft !== null && data !== undefined && JSON.stringify(draft) !== JSON.stringify(data);

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
