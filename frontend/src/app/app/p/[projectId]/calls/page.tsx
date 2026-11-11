"use client";

import { use, useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Check, Clock, Mic, MicOff, Phone, PhoneOff, SkipForward } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CallCard, OutcomeForm, clock, likelyOutcome } from "@/components/app/work/calls";
import { LeadModal } from "@/components/app/work/lead-modal";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Avatar, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { CallOutcome, CallRow, DialToken, TwilioStatus } from "@/lib/types";
import { loadVoiceSdk, type VoiceCall, type VoiceDevice } from "@/lib/twilio-voice";
import { cn, timeAgo } from "@/lib/utils";

type Owner = "mine_or_unassigned" | "everyone";

/** The call on screen: being placed or on, ended and waiting for its outcome, or logged by hand without dialing. */
type Active = { row: CallRow; phase: "connecting" | "ringing" | "connected" | "ended" | "log"; started?: number; ended?: number; muted?: boolean };

export default function CallsPage({ params }: PageProps<"/app/p/[projectId]/calls">) {
  const { projectId } = use(params);
  const [owner, setOwner] = useState<Owner>("mine_or_unassigned");
  const { data, error, mutate } = useSWR<CallRow[]>(`/projects/${projectId}/calls${qs({ owner })}`, fetcher, { keepPreviousData: true });
  const { data: twilio } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const [active, setActive] = useState<Active | null>(null);
  const [leadId, setLeadId] = useState<string | null>(null);
  const call = useRef<{ device: VoiceDevice; call?: VoiceCall } | null>(null);
  const calling = !!twilio?.connection?.twiml_app_sid;

  const hangUp = () => call.current?.call?.disconnect();
  const tearDown = () => {
    call.current?.device.destroy();
    call.current = null;
  };
  useEffect(() => tearDown, []);

  const dial = async (row: CallRow) => {
    setActive({ row, phase: "connecting" });
    try {
      const { token, to } = await api<DialToken>(`/calls/${row.id}/dial`, { method: "POST" });
      const { Device } = await loadVoiceSdk();
      const device = new Device(token, { codecPreferences: ["opus", "pcmu"] });
      call.current = { device };
      const placed = await device.connect({ params: { To: to, call_id: row.id } });
      call.current.call = placed;
      setActive({ row, phase: "ringing" });
      placed.on("accept", () => setActive((a) => a && { ...a, phase: "connected", started: Date.now() }));
      const ended = () => {
        setActive((a) => a && { ...a, phase: "ended", ended: Date.now() });
        tearDown();
      };
      placed.on("disconnect", ended);
      placed.on("cancel", ended);
      placed.on("reject", ended);
      placed.on("error", (e) => {
        toast.error(e.message || "The call dropped");
        ended();
      });
    } catch (e) {
      tearDown();
      setActive(null);
      toastError(e);
    }
  };

  const log = async (row: CallRow, outcome: CallOutcome, notes: string) => {
    try {
      await api(`/calls/${row.id}/log`, { body: { outcome, notes } });
      setActive(null);
      await mutate();
      toast.success(outcome === "meeting_booked" ? "Logged. They're marked as meeting booked" : "Logged. Their sequence goes on");
    } catch (e) {
      toastError(e);
    }
  };

  const skip = async (row: CallRow) => {
    try {
      await api(`/calls/${row.id}/skip`, { method: "POST" });
      await mutate();
      toast.success("Skipped. Their sequence goes on");
    } catch (e) {
      toastError(e);
    }
  };

  const rows = data ?? [];
  const groups = [
    { key: "overdue", title: "Overdue", rows: rows.filter((r) => r.bucket === "overdue") },
    { key: "today", title: "Due today", rows: rows.filter((r) => r.bucket === "today") },
    { key: "done", title: "Done in the last 14 days", rows: rows.filter((r) => r.bucket === "done") },
  ].filter((g) => g.rows.length > 0);
  const open = rows.some((r) => r.bucket !== "done");

  return (
    <>
      <PageHeader
        title="Calls"
        description="The call steps of your sequences come due here for each lead's owner. Log what happened, or skip it, and the sequence goes on."
        actions={
          <Select value={owner} onChange={(e) => setOwner(e.target.value as Owner)} className="w-auto flex-auto" aria-label="Show">
            <option value="mine_or_unassigned">Mine and unassigned</option>
            <option value="everyone">Everyone&apos;s</option>
          </Select>
        }
      />
      <PageBody className={cn(active && "pb-[28rem] sm:pb-6")}>
        {twilio && !calling && (
          <p className="mb-4 rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
            To call from Splee, turn on calling in the Twilio card on the{" "}
            <Link href={`/app/p/${projectId}/integrations`} className="font-medium underline underline-offset-2">
              Integrations page
            </Link>
            . You can still log calls you make from your phone.
          </p>
        )}
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 3 }, (_, i) => (
              <Skeleton key={i} className="h-24 rounded-2xl" />
            ))}
          </div>
        ) : groups.length === 0 ? (
          <Card>
            <EmptyState
              icon={<Phone className="size-5" />}
              title="No calls to make"
              action={
                <Link href={`/app/p/${projectId}/follow-ups`} className="text-sm font-medium underline underline-offset-2">
                  Add a call step
                </Link>
              }
            >
              Add a call to your sequence on the Follow-ups page. Leads with a phone number get one when it comes due.
            </EmptyState>
          </Card>
        ) : (
          <div className="flex flex-col gap-6">
            {!open && <p className="text-sm text-muted-foreground">Nothing to call right now.</p>}
            {groups.map((g) => (
              <section key={g.key}>
                <MicroLabel className="mb-2">
                  {g.title} · {g.rows.length}
                </MicroLabel>
                <Card className="divide-y divide-border">
                  {g.rows.map((r) =>
                    r.bucket === "done" ? (
                      <DoneRow key={r.id} row={r} onOpen={() => setLeadId(r.lead_id)} />
                    ) : (
                      <TaskRow
                        key={r.id}
                        row={r}
                        calling={calling}
                        busy={!!active}
                        onOpen={() => setLeadId(r.lead_id)}
                        onDial={() => dial(r)}
                        onLog={() => setActive({ row: r, phase: "log" })}
                        onSkip={() => skip(r)}
                      />
                    ),
                  )}
                </Card>
              </section>
            ))}
          </div>
        )}
      </PageBody>
      {active && (
        <CallPanel
          active={active}
          onHangUp={hangUp}
          onMute={() => {
            const muted = !active.muted;
            call.current?.call?.mute(muted);
            setActive({ ...active, muted });
          }}
          onLog={(outcome, notes) => log(active.row, outcome, notes)}
          onClose={() => setActive(null)}
        />
      )}
      <LeadModal leadId={leadId} onClose={() => setLeadId(null)} />
    </>
  );
}

function Who({ row: r, onOpen }: { row: CallRow; onOpen: () => void }) {
  return (
    // basis-60 sends the buttons under the name on a phone instead of squeezing it to a sliver.
    <div className="flex min-w-0 flex-1 basis-60 items-center gap-3">
      <Avatar name={r.lead_name || "?"} />
      <div className="min-w-0">
        <button type="button" onClick={onOpen} className="text-left font-medium hover:underline">
          {r.lead_name || "Unnamed lead"}
        </button>
        <p className="text-sm wrap-anywhere text-muted-foreground">{[r.lead_title, r.lead_company].filter(Boolean).join(" at ")}</p>
      </div>
    </div>
  );
}

function TaskRow({
  row: r,
  calling,
  busy,
  onOpen,
  onDial,
  onLog,
  onSkip,
}: {
  row: CallRow;
  calling: boolean;
  busy: boolean;
  onOpen: () => void;
  onDial: () => void;
  onLog: () => void;
  onSkip: () => void;
}) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3.5 sm:px-5">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-3">
        <Who row={r} onOpen={onOpen} />
        <div className="flex flex-wrap items-center gap-1.5">
          <Button size="sm" variant="ghost" onClick={onSkip} disabled={busy} className="px-2">
            <SkipForward className="size-3.5" /> Skip
          </Button>
          <Button size="sm" variant="outline" onClick={onLog} disabled={busy}>
            <Check className="size-3.5" /> Log call
          </Button>
          {calling && (
            <Button size="sm" onClick={onDial} disabled={busy || !!r.outside_hours || !r.lead_phone}>
              <Phone className="size-3.5" /> Call
            </Button>
          )}
        </div>
      </div>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-sm text-muted-foreground">
        <span className="font-mono text-foreground tabular-nums">{r.lead_phone || "No number"}</span>
        <span className="flex items-center gap-1">
          <Clock className="size-3.5" /> {r.local_time} their time
        </span>
        <span>{r.bucket === "overdue" ? `Due ${timeAgo(r.due_at)}` : "Due today"}</span>
        <span>{r.owner_name ? `For ${r.owner_name}` : "For anyone"}</span>
      </div>
      {r.instructions && <p className="rounded-lg bg-muted px-3 py-2 text-sm text-pretty">{r.instructions}</p>}
      {r.outside_hours && <p className="rounded-lg bg-amber-50 px-3 py-2 text-sm text-pretty text-amber-800">{r.outside_hours}</p>}
    </div>
  );
}

function DoneRow({ row: r, onOpen }: { row: CallRow; onOpen: () => void }) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3.5 sm:px-5">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <Who row={r} onOpen={onOpen} />
        {r.called_by_name && <span className="text-sm text-muted-foreground">By {r.called_by_name}</span>}
      </div>
      <CallCard call={r} />
    </div>
  );
}

/** Fixed to the bottom corner, or across the bottom on a phone, so the list stays usable while a call is on. */
function CallPanel({
  active: a,
  onHangUp,
  onMute,
  onLog,
  onClose,
}: {
  active: Active;
  onHangUp: () => void;
  onMute: () => void;
  onLog: (outcome: CallOutcome, notes: string) => Promise<void>;
  onClose: () => void;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (a.phase !== "connected") return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [a.phase]);
  const on = a.phase === "connecting" || a.phase === "ringing" || a.phase === "connected";
  const elapsed = a.started ? Math.max(0, Math.round(((a.ended ?? now) - a.started) / 1000)) : 0;
  const status = {
    connecting: "Connecting…",
    ringing: "Ringing…",
    connected: `On the call · ${clock(elapsed)}`,
    ended: a.started ? `Call ended · ${clock(elapsed)}` : "They didn't pick up",
    log: "What happened on the call?",
  }[a.phase];
  return (
    <div
      role="dialog"
      aria-label={`Call with ${a.row.lead_name}`}
      className="fixed inset-x-3 bottom-3 z-40 sm:inset-x-auto sm:right-6 sm:bottom-6 sm:w-96"
    >
      <Card className="flex flex-col gap-4 p-4 shadow-xl ring-1 ring-border">
        <div className="flex items-center gap-3">
          <Avatar name={a.row.lead_name || "?"} />
          <div className="min-w-0 flex-1">
            <p className="truncate font-medium">{a.row.lead_name}</p>
            <p className="font-mono text-sm text-muted-foreground tabular-nums">{a.row.lead_phone}</p>
          </div>
          {a.phase === "connected" && <span className="size-2.5 shrink-0 animate-pulse rounded-full bg-brand-500" aria-hidden />}
        </div>
        <p className={cn("text-sm", a.phase === "connected" ? "font-medium text-brand-700" : "text-muted-foreground")} aria-live="polite">
          {status}
        </p>
        {a.row.instructions && on && <p className="rounded-lg bg-muted px-3 py-2 text-sm text-pretty">{a.row.instructions}</p>}
        {on ? (
          <div className="grid grid-cols-2 gap-2">
            <Button variant="outline" onClick={onMute} disabled={a.phase !== "connected"}>
              {a.muted ? <MicOff className="size-4" /> : <Mic className="size-4" />} {a.muted ? "Unmute" : "Mute"}
            </Button>
            <Button onClick={onHangUp} className="bg-red-600 text-white hover:bg-red-700">
              <PhoneOff className="size-4" /> Hang up
            </Button>
          </div>
        ) : (
          <OutcomeForm
            initial={a.started ? null : likelyOutcome(a.row.call_status) ?? (a.phase === "ended" ? "no_answer" : null)}
            onSave={onLog}
            onCancel={onClose}
            cancelLabel={a.phase === "log" ? "Cancel" : "Later"}
          />
        )}
      </Card>
    </div>
  );
}
