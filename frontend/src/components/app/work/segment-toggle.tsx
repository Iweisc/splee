"use client";

import { useState } from "react";
import { Pause, Play } from "lucide-react";
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
