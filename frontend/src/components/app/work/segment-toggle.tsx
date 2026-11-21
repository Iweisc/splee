"use client";

import { useState } from "react";
import { Archive, ArchiveRestore, Pause, Play } from "lucide-react";
import { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, type ButtonSize } from "@/components/ui/button";
import { api } from "@/lib/api";
import type { Segment } from "@/lib/types";
import { toastError } from "./states";

export function SegmentToggle({ segment, size = "sm" }: { segment: Segment; size?: ButtonSize }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const running = segment.status === "working" || segment.status === "scaling";

  const toggle = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setBusy(true);
    try {
      await api(`/segments/${segment.id}/${running ? "pause" : "start"}`, { method: "POST" });
      await mutate(() => true);
      toast.success(running ? `${segment.name} paused` : `${segment.name} started. Its drafts are approved for sending.`);
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Button size={size} variant={running ? "outline" : "primary"} onClick={toggle} loading={busy}>
      {running ? <Pause className="size-3.5" /> : <Play className="size-3.5" />}
      {running ? "Pause" : "Start"}
    </Button>
  );
}

/** Archive on a campaign's page, Unarchive on an archived one. Archiving pauses it, and it comes back paused. */
export function ArchiveToggle({ segment, size = "sm" }: { segment: Segment; size?: ButtonSize }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const archived = segment.archived_at !== null;

  const toggle = async (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setBusy(true);
    try {
      await api(`/segments/${segment.id}/${archived ? "unarchive" : "archive"}`, { method: "POST" });
      await mutate(() => true);
      toast.success(archived ? `${segment.name} is back, paused. Start it to send again.` : `${segment.name} archived. Its leads and results stay.`);
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(false);
    }
  };

  if (archived) {
    return (
      <Button size={size} variant="outline" onClick={toggle} loading={busy}>
        <ArchiveRestore className="size-3.5" /> Unarchive
      </Button>
    );
  }
  return (
    <Button variant="ghost" size="icon" className="size-10" onClick={toggle} loading={busy} aria-label="Archive campaign" title="Archive campaign">
      <Archive className="size-4" />
    </Button>
  );
}
