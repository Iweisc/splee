"use client";

import { useState } from "react";
import { Check } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { api } from "@/lib/api";
import type { LeadReminder } from "@/lib/types";
import { cn, dayTime, isPast } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";
import { LoadError, toastError } from "./states";

/** "2026-11-24" for the day `days` from today, where you are. */
function localDay(days: number): string {
  const d = new Date();
  d.setDate(d.getDate() + days);
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
}

/** The lead's open reminders, the team's and yours, and a form to set one for yourself. A reminder comes at 9:00 your time on its
 *  day, on Today from the evening before, until you mark it done. */
export function LeadReminders({ leadId }: { leadId: string }) {
  const key = `/leads/${leadId}/reminders`;
  const { me } = useMe();
  const { data: reminders, error, mutate } = useSWR<LeadReminder[]>(key, fetcher);
  const { mutate: mutateAll } = useSWRConfig();
  const [day, setDay] = useState(() => localDay(1));
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const refresh = async () => {
    await mutate();
    // Today lists your due reminders, so it changes with them.
    mutateAll((k) => typeof k === "string" && k.startsWith("/today"));
  };

  const add = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy("add");
    try {
      await api(key, { body: { due_at: new Date(`${day}T09:00`).toISOString(), note: note.trim() } });
      setNote("");
      toast.success("Reminder set");
      await refresh();
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(null);
    }
  };

  const done = async (id: string) => {
    setBusy(id);
    try {
      await api(`/reminders/${id}/done`, { body: {} });
      await refresh();
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-2">
      {error && !reminders && <LoadError error={error} retry={() => void mutate()} what="reminders" />}
      {reminders?.map((r) => (
        <div key={r.id} className="flex items-start gap-3 rounded-lg border border-border px-3 py-2 text-sm">
          <div className="min-w-0 flex-1">
            <p>
              <span className={cn("font-medium", isPast(r.due_at) && "text-red-700")}>{dayTime(r.due_at)}</span>
              <span className="text-muted-foreground"> · {r.user_id === me?.user.id ? "You" : r.user_name}</span>
            </p>
            {r.note && <p className="text-pretty text-muted-foreground">{r.note}</p>}
          </div>
          {r.user_id === me?.user.id && (
            <Button size="sm" variant="ghost" className="-my-1 shrink-0" onClick={() => done(r.id)} loading={busy === r.id}>
              <Check className="size-3.5" /> Done
            </Button>
          )}
        </div>
      ))}
      <form onSubmit={add} className="flex flex-wrap gap-2">
        <Input type="date" value={day} min={localDay(0)} onChange={(e) => setDay(e.target.value)} aria-label="Remind me on" className="w-40" required />
        <Input
          value={note}
          onChange={(e) => setNote(e.target.value)}
          maxLength={500}
          placeholder="What to do, like call about the pilot"
          aria-label="What to do"
          className="min-w-48 flex-1"
        />
        <Button type="submit" variant="outline" loading={busy === "add"} disabled={!day}>
          Remind me
        </Button>
      </form>
    </div>
  );
}
