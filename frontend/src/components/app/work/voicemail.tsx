"use client";

import { useEffect, useState } from "react";
import { Mic, Square, Voicemail as VoicemailIcon } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { clock } from "@/components/app/work/calls";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Voicemail } from "@/lib/types";
import { cn } from "@/lib/utils";
import { startRecording, type Recording } from "@/lib/wav-recorder";

/** The backend takes up to a minute. */
const MAX_SECS = 60;
/** 8 kHz, 16-bit, mono, after a 44-byte header. */
const wavSecs = (wav: Blob) => Math.round((wav.size - 44) / 16000);

export function useVoicemail() {
  return useSWR<Voicemail | null>("/voicemail", fetcher);
}

/** Records the voicemail you leave with one click when a call from the Calls page reaches someone's voicemail. */
export function VoicemailCard() {
  const { data, mutate } = useVoicemail();
  const [taking, setTaking] = useState<{ recording: Recording; started: number } | null>(null);
  const [take, setTake] = useState<{ wav: Blob; url: string } | null>(null);
  const [now, setNow] = useState(() => Date.now());
  const [busy, setBusy] = useState(false);
  useEffect(() => () => (take ? URL.revokeObjectURL(take.url) : undefined), [take]);

  /** Runs until Stop, Cancel or the minute is up, then keeps the take to play back. */
  const start = async () => {
    try {
      const recording = await startRecording(MAX_SECS);
      setTake(null);
      setNow(Date.now());
      setTaking({ recording, started: Date.now() });
      const wav = await recording.wav;
      setTaking(null);
      if (wav) setTake({ wav, url: URL.createObjectURL(wav) });
    } catch (e) {
      setTaking(null);
      toastError(e instanceof DOMException && e.name === "NotAllowedError" ? new Error("Allow Splee to use your microphone to record it") : e);
    }
  };

  const elapsed = taking ? Math.min(MAX_SECS, Math.floor((now - taking.started) / 1000)) : 0;
  useEffect(() => {
    if (!taking) return;
    const id = setInterval(() => setNow(Date.now()), 250);
    return () => {
      clearInterval(id);
      // Leaving the page mid-take lets go of the microphone. Once the take has ended, this does nothing.
      taking.recording.cancel();
    };
  }, [taking]);

  const save = async () => {
    if (!take) return;
    setBusy(true);
    try {
      await mutate(await api<Voicemail>("/voicemail", { method: "PUT", body: take.wav }), { revalidate: false });
      setTake(null);
      toast.success("Saved. It's ready for your calls");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      await api("/voicemail", { method: "DELETE" });
      await mutate(null, { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  if (data === undefined) return null;
  const [title, text] = taking
    ? [`Recording · ${clock(elapsed)}`, "Say it the way you'd leave it. Stop when you're done, up to a minute."]
    : take
      ? [`New take · ${clock(wavSecs(take.wav))}`, "Play it back, then save it or record it again."]
      : data
        ? [`Your voicemail · ${clock(data.secs)}`, "Leave it from the call panel when their voicemail picks up."]
        : ["Record your voicemail", "Leave it with one click when a call reaches someone's voicemail."];
  const src = take ? take.url : data && !taking ? `/api/voicemail/audio?v=${encodeURIComponent(data.created_at)}` : null;

  return (
    <Card className="mb-6 flex flex-wrap items-center gap-x-4 gap-y-3 px-4 py-3.5 sm:px-5">
      <div className="flex min-w-0 flex-1 basis-72 items-center gap-3">
        <span className="grid size-9 shrink-0 place-items-center rounded-full bg-muted" aria-hidden>
          {taking ? <span className="size-2.5 animate-pulse rounded-full bg-red-600" /> : <VoicemailIcon className="size-4" />}
        </span>
        <div className="min-w-0">
          <p className={cn("font-medium tabular-nums", taking && "text-red-700")} aria-live="polite">
            {title}
          </p>
          <p className="text-sm text-pretty text-muted-foreground">{text}</p>
        </div>
      </div>
      {src && <audio controls preload="none" src={src} className="h-9 w-full sm:w-64" />}
      <div className="flex flex-wrap items-center gap-1.5">
        {taking ? (
          <>
            <Button size="sm" variant="ghost" onClick={taking.recording.cancel}>
              Cancel
            </Button>
            <Button size="sm" onClick={taking.recording.stop}>
              <Square className="size-3.5" /> Stop
            </Button>
          </>
        ) : take ? (
          <>
            <Button size="sm" variant="outline" onClick={start} disabled={busy}>
              <Mic className="size-3.5" /> Record again
            </Button>
            <Button size="sm" onClick={save} loading={busy}>
              Save
            </Button>
          </>
        ) : data ? (
          <>
            <Button size="sm" variant="ghost" onClick={remove} disabled={busy}>
              Delete
            </Button>
            <Button size="sm" variant="outline" onClick={start} disabled={busy}>
              <Mic className="size-3.5" /> Record again
            </Button>
          </>
        ) : (
          <Button size="sm" onClick={start}>
            <Mic className="size-3.5" /> Record
          </Button>
        )}
      </div>
    </Card>
  );
}
