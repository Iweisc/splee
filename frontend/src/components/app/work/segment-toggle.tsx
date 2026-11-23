"use client";

import { useEffect, useRef, useState } from "react";
import { Archive, ArchiveRestore, CalendarClock, Pause, Play, X } from "lucide-react";
import { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button, type ButtonSize } from "@/components/ui/button";
import { api } from "@/lib/api";
import type { Segment } from "@/lib/types";
import { dayTime } from "@/lib/utils";
import { toastError } from "./states";
import { TimePicker, tomorrowMorning } from "./time-picker";

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

/** Beside Start on a campaign that isn't sending: pick a time for it to start by itself, as Start would then, or change or
 *  cancel the one picked. Start and Archive cancel it too. */
export function StartLater({ segment }: { segment: Segment }) {
  const { mutate } = useSWRConfig();
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  // Scheduling swaps Start later for the Starts button and cancelling swaps it back, so the X that had focus, or the button
  // the picker would hand focus back to, is gone. Focus goes to the button in its place, if it was on the X or in the picker
  // just before the swap and hasn't landed anywhere since. `changed` makes this run once after every change, even a no-op.
  const trigger = useRef<HTMLButtonElement>(null);
  const cancel = useRef<HTMLButtonElement>(null);
  const ours = useRef(false);
  const [changed, setChanged] = useState(0);
  useEffect(() => {
    if (ours.current && document.activeElement === document.body) trigger.current?.focus();
    ours.current = false;
  }, [changed]);
  const set = async (start_at: string | null) => {
    setBusy(true);
    try {
      await api(`/segments/${segment.id}`, { method: "PATCH", body: { start_at } });
      // From the picker `picking` is true, and it's modal, so nothing else can have taken focus.
      ours.current = picking || document.activeElement === cancel.current;
      await mutate(() => true);
      toast.success(start_at ? `${segment.name} starts by itself on ${dayTime(start_at)}` : "Scheduled start cancelled");
      setPicking(false);
      setChanged((n) => n + 1);
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      {segment.start_at ? (
        // It comes after Start, so on a phone it wraps to a full row of its own under the other actions.
        <span className="inline-flex items-center max-sm:basis-full">
          <Button
            ref={trigger}
            variant="outline"
            className="rounded-r-none max-sm:flex-1"
            onClick={() => setPicking(true)}
            aria-label={`Starts ${dayTime(segment.start_at)}. Change it`}
          >
            <CalendarClock className="size-3.5" /> Starts {dayTime(segment.start_at)}
          </Button>
          <Button
            ref={cancel}
            variant="outline"
            size="icon"
            className="size-10 rounded-l-none border-l-0"
            onClick={() => set(null)}
            loading={busy}
            aria-label="Cancel the scheduled start"
            title="Cancel the scheduled start"
          >
            <X className="size-4" />
          </Button>
        </span>
      ) : (
        <Button ref={trigger} variant="outline" onClick={() => setPicking(true)}>
          <CalendarClock className="size-3.5" /> Start later
        </Button>
      )}
      {picking && (
        <TimePicker
          title="Start this campaign later"
          action="Schedule start"
          initial={segment.start_at ? new Date(segment.start_at) : tomorrowMorning()}
          days={365}
          onPick={set}
          onClose={() => setPicking(false)}
        />
      )}
    </>
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
