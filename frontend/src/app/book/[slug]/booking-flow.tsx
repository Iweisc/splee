"use client";

import { useState } from "react";
import Link from "next/link";
import useSWR from "swr";
import { ArrowLeft, CalendarCheck, CircleAlert } from "lucide-react";
import { BookingShell, Missing } from "@/components/booking/shell";
import { browserZone, SlotPicker, when, ZonePicker } from "@/components/booking/slot-picker";
import { Button } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { PublicBookingPage, Ts } from "@/lib/types";

interface Confirmed {
  starts_at: Ts;
  ends_at: Ts;
  email: string;
  token: string;
}

export function BookingFlow({ slug }: { slug: string }) {
  const path = `/book/${encodeURIComponent(slug)}`;
  const { data: page, error, mutate } = useSWR<PublicBookingPage>(path, fetcher, { revalidateOnFocus: false });
  const [zone, setZone] = useState(browserZone);
  const [picked, setPicked] = useState<Ts | null>(null);
  const [form, setForm] = useState({ name: "", email: "", note: "" });
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const [done, setDone] = useState<Confirmed | null>(null);

  if (error instanceof ApiError && error.status === 404) return <Missing />;
  if (error || !page) {
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

  const minutes = page.duration_minutes;
  const end = (start: Ts) => new Date(new Date(start).getTime() + minutes * 60_000).toISOString();

  if (done) {
    return (
      <BookingShell page={page}>
        <div className="flex flex-col items-start gap-3">
          <CalendarCheck className="size-7 text-brand-600" />
          <h2 className="text-xl font-semibold">You&apos;re booked</h2>
          <p className="font-medium">{when(done.starts_at, done.ends_at, zone)}</p>
          <p className="text-sm text-pretty text-muted-foreground">
            The invite is on its way to {done.email}, so the call lands in your calendar. The email has links to move or cancel it.
          </p>
          <Link href={`/booking/${done.token}`} className="text-sm font-medium underline underline-offset-4">
            Move or cancel this call
          </Link>
        </div>
      </BookingShell>
    );
  }

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!picked) return;
    setBusy(true);
    setProblem(null);
    try {
      setDone(await api<Confirmed>(path, { body: { start: picked, ...form, timezone: zone } }));
    } catch (err) {
      setProblem(err instanceof Error ? err.message : "Something went wrong. Try again.");
      // Someone else took the time while this form was open, so the visitor goes back to fresh times.
      if (err instanceof ApiError && err.status === 409) {
        setPicked(null);
        mutate();
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <BookingShell page={page}>
      {!picked ? (
        <div className="flex flex-col gap-5">
          <h2 className="text-lg font-semibold">Pick a time</h2>
          {problem && <Problem text={problem} />}
          <SlotPicker slots={page.slots} zone={zone} picked={picked} onPick={setPicked} />
          <ZonePicker zone={zone} onChange={setZone} />
        </div>
      ) : (
        <form onSubmit={submit} className="flex flex-col gap-4">
          <button
            type="button"
            onClick={() => setPicked(null)}
            className="flex items-center gap-1 self-start text-sm text-muted-foreground hover:text-foreground"
          >
            <ArrowLeft className="size-4" /> Other times
          </button>
          <div>
            <h2 className="text-lg font-semibold">Your details</h2>
            <p className="mt-1 text-sm text-muted-foreground">{when(picked, end(picked), zone)}</p>
          </div>
          {problem && <Problem text={problem} />}
          <Field label="Name">
            <Input required autoComplete="name" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} autoFocus />
          </Field>
          <Field label="Email">
            <Input required type="email" autoComplete="email" value={form.email} onChange={(e) => setForm({ ...form, email: e.target.value })} />
          </Field>
          <Field label="Anything to share before the call?" hint="Optional">
            <Textarea maxLength={2000} rows={3} value={form.note} onChange={(e) => setForm({ ...form, note: e.target.value })} />
          </Field>
          <Button type="submit" size="lg" loading={busy} className="mt-1 w-full sm:w-auto sm:self-start">
            Book the call
          </Button>
        </form>
      )}
    </BookingShell>
  );
}

function Problem({ text }: { text: string }) {
  return (
    <p className="flex items-start gap-2 rounded-xl bg-red-50 px-4 py-3 text-sm text-red-700">
      <CircleAlert className="mt-0.5 size-4 shrink-0" />
      {text}
    </p>
  );
}
