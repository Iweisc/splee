"use client";

import { useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import { ArrowLeft, CalendarCheck, CalendarX2, CircleAlert } from "lucide-react";
import { BookingShell, Missing } from "@/components/booking/shell";
import { browserZone, SlotPicker, when, ZonePicker } from "@/components/booking/slot-picker";
import { Button } from "@/components/ui/button";
import { Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { ManagedBooking, Ts } from "@/lib/types";

type Step = "view" | "move" | "cancel";

// Opening this page changes nothing. Mail scanners open the links in emails, so moving and cancelling take a button press.
export function ManageBooking({ token, cancel }: { token: string; cancel: boolean }) {
  const path = `/booking/${encodeURIComponent(token)}`;
  const { data, error, mutate } = useSWR<ManagedBooking>(path, fetcher, { revalidateOnFocus: false });
  const [step, setStep] = useState<Step>(cancel ? "cancel" : "view");
  const [zone, setZone] = useState(browserZone);
  const [picked, setPicked] = useState<Ts | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [moved, setMoved] = useState(false);

  if (error instanceof ApiError && error.status === 404) return <Missing />;
  if (error || !data) {
    return (
      <BookingShell>
        <div className="flex min-h-64 flex-col items-center justify-center gap-3 text-center">
          {error ? (
            <>
              <p className="text-sm text-muted-foreground">{error.message}</p>
              <Button variant="outline" onClick={() => mutate()}>
                Try again
              </Button>
            </>
          ) : (
            <Spinner />
          )}
        </div>
      </BookingShell>
    );
  }

  const act = async (action: "cancel" | "move") => {
    setBusy(true);
    setProblem(null);
    try {
      const next = await api<ManagedBooking>(`${path}/${action}`, action === "move" ? { body: { start: picked } } : { method: "POST" });
      await mutate(next, { revalidate: false });
      setMoved(action === "move");
      setStep("view");
      setPicked(null);
    } catch (e) {
      setProblem(e instanceof Error ? e.message : "Something went wrong. Try again.");
      if (e instanceof ApiError && e.status === 409) {
        setPicked(null);
        mutate();
      }
    } finally {
      setBusy(false);
    }
  };

  const { page } = data;
  const at = when(data.starts_at, data.ends_at, zone);
  const end = (start: Ts) => new Date(new Date(start).getTime() + page.duration_minutes * 60_000).toISOString();
  const problemNote = problem && (
    <p className="flex items-start gap-2 rounded-xl bg-red-50 px-4 py-3 text-sm text-red-700">
      <CircleAlert className="mt-0.5 size-4 shrink-0" />
      {problem}
    </p>
  );

  if (data.cancelled) {
    return (
      <BookingShell page={page}>
        <div className="flex flex-col items-start gap-3">
          <CalendarX2 className="size-7 text-muted-foreground" />
          <h2 className="text-xl font-semibold">This call is cancelled</h2>
          <p className="text-sm text-muted-foreground line-through">{at}</p>
          <Link href={`/book/${page.slug}`} className="text-sm font-medium underline underline-offset-4">
            Book another time
          </Link>
        </div>
      </BookingShell>
    );
  }

  if (step === "move" && !data.started) {
    return (
      <BookingShell page={page}>
        <div className="flex flex-col gap-5">
          <button
            type="button"
            onClick={() => setStep("view")}
            className="flex items-center gap-1 self-start text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-4" /> Back
          </button>
          <div>
            <h2 className="text-lg font-semibold">Pick a new time</h2>
            <p className="mt-1 text-sm text-muted-foreground">Now {at}</p>
          </div>
          {problemNote}
          <SlotPicker slots={page.slots} zone={zone} picked={picked} near={data.starts_at} onPick={setPicked} />
          <ZonePicker zone={zone} onChange={setZone} />
          {picked && (
            <Button size="lg" loading={busy} onClick={() => act("move")} className="w-full sm:w-auto sm:self-start">
              Move to {when(picked, end(picked), zone)}
            </Button>
          )}
        </div>
      </BookingShell>
    );
  }

  return (
    <BookingShell page={page}>
      <div className="flex flex-col items-start gap-3">
        <CalendarCheck className="size-7 text-brand-600" />
        <h2 className="text-xl font-semibold">{moved ? "Your call has moved" : data.started ? "This call has started" : "Your call is booked"}</h2>
        <p className="font-medium">{at}</p>
        {moved && <p className="text-sm text-muted-foreground">An updated invite is on its way, so your calendar moves the call too.</p>}
        {problemNote}
        {data.started ? null : step === "cancel" ? (
          <div className="mt-2 flex w-full flex-col gap-3 rounded-xl bg-muted p-4">
            <p className="text-sm font-medium">Cancel this call?</p>
            <div className="flex flex-wrap gap-2">
              <Button variant="danger" loading={busy} onClick={() => act("cancel")}>
                Cancel the call
              </Button>
              <Button variant="outline" onClick={() => setStep("view")} disabled={busy}>
                Keep it
              </Button>
            </div>
          </div>
        ) : (
          <div className="mt-2 flex flex-wrap gap-2">
            <Button variant="outline" onClick={() => setStep("move")}>
              Pick a new time
            </Button>
            <Button variant="ghost" onClick={() => setStep("cancel")}>
              Cancel the call
            </Button>
          </div>
        )}
      </div>
    </BookingShell>
  );
}
