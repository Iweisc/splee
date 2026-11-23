"use client";

import { useEffect, useRef, useState } from "react";
import { useRouter } from "next/navigation";
import { Archive, ArchiveRestore, CalendarClock, Copy, Pause, Play, X } from "lucide-react";
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

const LATER = {
  start: {
    idle: "Start later",
    set: "Starts",
    title: "Start this campaign later",
    action: "Schedule start",
    done: (name: string, at: string) => `${name} starts by itself on ${at}`,
    cancelled: "Scheduled start cancelled",
    cancel: "Cancel the scheduled start",
  },
  pause: {
    idle: "Pause later",
    set: "Pauses",
    title: "Pause this campaign later",
    action: "Schedule pause",
    done: (name: string, at: string) => `${name} pauses by itself on ${at}`,
    cancelled: "Scheduled pause cancelled",
    cancel: "Cancel the scheduled pause",
  },
};

/** Beside Start on a campaign that isn't sending: pick a time for it to start by itself, as Start would then, or change or
 *  cancel the one picked. Start and Archive cancel it too. */
export function StartLater({ segment }: { segment: Segment }) {
  return <Later segment={segment} kind="start" />;
}

/** Beside Pause on a campaign that's sending, or set to start: pick a time for it to pause by itself, as Pause would then.
 *  Pause and Archive cancel it, and a start doesn't, so with Start later it makes a sending window. */
export function PauseLater({ segment }: { segment: Segment }) {
  return <Later segment={segment} kind="pause" />;
}

const DAY = 24 * 60 * 60 * 1000;

function Later({ segment, kind }: { segment: Segment; kind: keyof typeof LATER }) {
  const text = LATER[kind];
  const field = kind === "start" ? "start_at" : "pause_at";
  const at = segment[field];
  const { mutate } = useSWRConfig();
  const [picking, setPicking] = useState(false);
  const [busy, setBusy] = useState(false);
  // Scheduling swaps the Later button for the one with the time and cancelling swaps it back, so the X that had focus, or the
  // button the picker would hand focus back to, is gone. Focus goes to the button in its place, if it was in here (the picker
  // included) just before the swap and hasn't landed anywhere since. `changed` makes this run once after every change.
  const root = useRef<HTMLSpanElement>(null);
  const trigger = useRef<HTMLButtonElement>(null);
  const ours = useRef(false);
  const [changed, setChanged] = useState(0);
  useEffect(() => {
    if (ours.current && document.activeElement === document.body) trigger.current?.focus();
    ours.current = false;
  }, [changed]);
  const set = async (to: string | null) => {
    setBusy(true);
    try {
      await api(`/segments/${segment.id}`, { method: "PATCH", body: { [field]: to } });
      ours.current = !!root.current?.contains(document.activeElement);
      await mutate(() => true);
      toast.success(to ? text.done(segment.name, dayTime(to)) : text.cancelled);
      setPicking(false);
      setChanged((n) => n + 1);
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(false);
    }
  };
  // A pause starts a week after a scheduled start, so the first pick is already after it.
  const initial = at ? new Date(at) : kind === "pause" && segment.start_at ? new Date(new Date(segment.start_at).getTime() + 7 * DAY) : tomorrowMorning();

  return (
    <span ref={root} className="contents">
      {at ? (
        // It comes after Start or Pause, so on a phone it wraps to a full row of its own under the other actions.
        <span className="inline-flex items-center max-sm:basis-full">
          <Button
            ref={trigger}
            variant="outline"
            className="rounded-r-none max-sm:flex-1"
            onClick={() => setPicking(true)}
            aria-label={`${text.set} ${dayTime(at)}. Change it`}
          >
            <CalendarClock className="size-3.5" /> {text.set} {dayTime(at)}
          </Button>
          <Button
            variant="outline"
            size="icon"
            className="size-10 rounded-l-none border-l-0"
            onClick={() => set(null)}
            loading={busy}
            aria-label={text.cancel}
            title={text.cancel}
          >
            <X className="size-4" />
          </Button>
        </span>
      ) : (
        <Button ref={trigger} variant="outline" onClick={() => setPicking(true)}>
          <CalendarClock className="size-3.5" /> {text.idle}
        </Button>
      )}
      {picking && (
        <TimePicker title={text.title} action={text.action} initial={initial} days={365} onPick={set} onClose={() => setPicking(false)} />
      )}
    </span>
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

/** Copies the campaign as a new draft with its brief, targeting and settings, and no leads or emails, and opens it. */
export function DuplicateButton({ segment }: { segment: Segment }) {
  const router = useRouter();
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const duplicate = async () => {
    setBusy(true);
    try {
      const copy = await api<Segment>(`/segments/${segment.id}/duplicate`, { method: "POST" });
      await mutate((key) => typeof key === "string" && key.startsWith(`/projects/${segment.project_id}/segments`));
      toast.success(`${copy.name} is a new draft. Find companies or people for it, then start it.`);
      router.push(`/app/p/${segment.project_id}/segments/${copy.id}`);
    } catch (err) {
      toastError(err);
      setBusy(false);
    }
  };
  return (
    <Button variant="ghost" size="icon" className="size-10" onClick={duplicate} loading={busy} aria-label="Duplicate campaign" title="Duplicate campaign">
      <Copy className="size-4" />
    </Button>
  );
}
