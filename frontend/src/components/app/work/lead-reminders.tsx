"use client";

import { useEffect, useRef, useState } from "react";
import { Check } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { api } from "@/lib/api";
import type { LeadReminder } from "@/lib/types";
import { cn, dayTime, isPast, timeAgo } from "@/lib/utils";
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
  const root = useRef<HTMLDivElement>(null);
  // A done reminder whose Done had focus, and where that Done was. Once its row leaves the list and takes focus with it, focus
  // goes to the Done now in that place, or the last one, or the note, so someone on the keyboard isn't sent back to the top of
  // the page. Someone who moved focus elsewhere meanwhile keeps it there.
  const refocus = useRef<{ id: string; at: number } | null>(null);
  useEffect(() => {
    const pending = refocus.current;
    if (!pending || reminders?.some((r) => r.id === pending.id)) return;
    refocus.current = null;
    if (!root.current || document.activeElement !== document.body) return;
    const dones = root.current.querySelectorAll<HTMLElement>("[data-done]");
    (dones[pending.at] ?? dones[dones.length - 1] ?? root.current.querySelector("input"))?.focus();
  }, [reminders]);

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

  const done = async (id: string, button: HTMLElement) => {
    const at = button === document.activeElement ? [...(root.current?.querySelectorAll("[data-done]") ?? [])].indexOf(button) : -1;
    setBusy(id);
    try {
      await api(`/reminders/${id}/done`, { body: {} });
      if (at >= 0) refocus.current = { id, at };
      toast.success("Reminder done");
      await refresh();
    } catch (err) {
      toastError(err);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div ref={root} className="flex flex-col gap-2">
      {error && !reminders && <LoadError error={error} retry={() => void mutate()} what="reminders" />}
      {reminders?.map((r) => (
        <div key={r.id} className="flex items-start gap-3 rounded-lg border border-border px-3 py-2 text-sm">
          <div className="min-w-0 flex-1">
            <p>
              {/* Says it's late in words, not only in red, like Today's reminders card. */}
              <span className={cn("font-medium", isPast(r.due_at) && "text-red-700")}>{isPast(r.due_at) ? `Due ${timeAgo(r.due_at)}` : dayTime(r.due_at)}</span>
              <span className="text-muted-foreground"> · {r.user_id === me?.user.id ? "You" : r.user_name}</span>
            </p>
            {r.note && <p className="text-pretty text-muted-foreground">{r.note}</p>}
          </div>
          {r.user_id === me?.user.id && (
            <Button
              size="sm"
              variant="ghost"
              className="-my-1 shrink-0"
              onClick={(e) => done(r.id, e.currentTarget)}
              loading={busy === r.id}
              aria-label={`Done, the reminder for ${dayTime(r.due_at)}`}
              data-done
            >
              <Check className="size-3.5" /> Done
            </Button>
          )}
        </div>
      ))}
      <form onSubmit={add} className="flex flex-wrap gap-2">
        <Input
          value={note}
          onChange={(e) => setNote(e.target.value)}
          maxLength={500}
          placeholder="What to do, like call about the pilot"
          aria-label="What to do"
          // On phones the note takes a row of its own, so its example isn't cut off.
          className="min-w-48 flex-1 max-sm:basis-full"
        />
        <Input type="date" value={day} min={localDay(0)} onChange={(e) => setDay(e.target.value)} aria-label="Remind me on" className="w-40" required />
        <Button type="submit" variant="outline" loading={busy === "add"} disabled={!day}>
          Remind me
        </Button>
      </form>
    </div>
  );
}
