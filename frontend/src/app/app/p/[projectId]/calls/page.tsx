"use client";

import { use, useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Check, Clock, Headset, Mic, MicOff, Phone, PhoneOff, PhoneOutgoing, Play, SkipForward, Voicemail } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CallCard, OutcomeForm, clock, likelyOutcome } from "@/components/app/work/calls";
import { LeadModal } from "@/components/app/work/lead-modal";
import { TalkTrack, TalkTrackRow } from "@/components/app/work/talk-track";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { VoicemailCard, useVoicemail } from "@/components/app/work/voicemail";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Avatar, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { CallOutcome, CallRow, CloudPhoneConnection, DialToken, TwilioStatus } from "@/lib/types";
import { loadVoiceSdk, type VoiceCall, type VoiceDevice } from "@/lib/twilio-voice";
import { cn, timeAgo } from "@/lib/utils";

type Owner = "mine_or_unassigned" | "everyone";

/** The cloud phones a call step can start in. The call comes back through the phone's webhook when it ends and closes the
 *  step, and the list reloads when they come back to this tab. */
const PHONES = [
  { key: "aircall", name: "Aircall", icon: Headset, started: "Ready to call in Aircall Workspace. It's logged here when it ends" },
  { key: "dialpad", name: "Dialpad", icon: PhoneOutgoing, started: "Dialpad is ringing your app. It's logged here when the call ends" },
] as const;
type CloudPhone = (typeof PHONES)[number];

/** The call on screen: being placed or on, ended and waiting for its outcome, logged by hand without dialing, or the next one
 *  in a row, which dials itself at `at`. `voicemail` is set once they asked to leave theirs in it. */
type Active = {
  row: CallRow;
  phase: "connecting" | "ringing" | "connected" | "ended" | "log" | "next";
  started?: number;
  ended?: number;
  muted?: boolean;
  at?: number;
  voicemail?: boolean;
};

/** The pause after logging a call in a row, to read the next lead's talk track before it rings. */
const NEXT_SECS = 5;

/** The calls that can be dialed now, in the order the page lists them. */
const callable = (rows: CallRow[] | undefined) =>
  ["overdue", "today"].flatMap((b) => (rows ?? []).filter((r) => r.bucket === b && !r.outside_hours && r.lead_phone));

export default function CallsPage({ params }: PageProps<"/app/p/[projectId]/calls">) {
  const { projectId } = use(params);
  const [owner, setOwner] = useState<Owner>("mine_or_unassigned");
  const { data, error, mutate } = useSWR<CallRow[]>(`/projects/${projectId}/calls${qs({ owner })}`, fetcher, {
    keepPreviousData: true,
    refreshInterval: (rows) => (rows?.some((r) => r.prep?.status === "writing") ? 4000 : 0),
  });
  const { data: twilio } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const { data: aircall } = useSWR<CloudPhoneConnection | null>(`/projects/${projectId}/aircall`, fetcher);
  const { data: dialpad } = useSWR<CloudPhoneConnection | null>(`/projects/${projectId}/dialpad`, fetcher);
  const phones = PHONES.filter((p) => ({ aircall, dialpad })[p.key]);
  const [active, setActive] = useState<Active | null>(null);
  const [pushing, setPushing] = useState<{ row: string; phone: CloudPhone["key"] } | null>(null);
  const [leadId, setLeadId] = useState<string | null>(null);
  /** Calling in a row: the calls it dialed so far, so none comes round twice. */
  const [run, setRun] = useState<string[] | null>(null);
  const call = useRef<{ device: VoiceDevice; call?: VoiceCall } | null>(null);
  const calling = !!twilio?.connection?.twiml_app_sid;
  const { data: voicemail } = useVoicemail();

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
      setRun(null);
      toastError(e);
    }
  };

  /** Twilio plays it on their side and hangs up, which ends the call here too, so they can log it and move on. */
  const leaveVoicemail = async (row: CallRow) => {
    setActive((a) => (a?.row.id === row.id ? { ...a, voicemail: true } : a));
    try {
      await api(`/calls/${row.id}/voicemail`, { method: "POST" });
    } catch (e) {
      setActive((a) => (a?.row.id === row.id ? { ...a, voicemail: false } : a));
      toastError(e);
    }
  };

  const callNext = (row: CallRow) => {
    setRun((r) => r && [...r, row.id]);
    dial(row);
  };

  const stop = () => {
    setActive(null);
    setRun(null);
  };

  const dialIn = async (row: CallRow, phone: CloudPhone) => {
    setPushing({ row: row.id, phone: phone.key });
    try {
      await api(`/calls/${row.id}/${phone.key}-dial`, { method: "POST" });
      toast.success(phone.started);
    } catch (e) {
      toastError(e);
    } finally {
      setPushing(null);
    }
  };

  const log = async (row: CallRow, outcome: CallOutcome, notes: string) => {
    try {
      await api(`/calls/${row.id}/log`, { body: { outcome, notes } });
      const fresh = await mutate();
      const next = run && callable(fresh).find((r) => !run.includes(r.id));
      // No toast here: it would sit over the panel's buttons, and the panel says it was logged.
      if (next) {
        setActive({ row: next, phase: "next", at: Date.now() + NEXT_SECS * 1000 });
        return;
      }
      stop();
      toast.success(
        run
          ? "Logged. That's everyone you can call right now"
          : outcome === "meeting_booked"
            ? "Logged. They're marked as meeting booked"
            : "Logged. Their sequence goes on",
      );
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
  const queue = callable(data);

  return (
    <>
      <PageHeader
        title="Calls"
        description="The call steps of your sequences come due here for each lead's owner. Log what happened, or skip it, and the sequence goes on."
        actions={
          <>
            {calling && !run && queue.length > 1 && (
              <Button
                onClick={() => {
                  setRun([queue[0].id]);
                  dial(queue[0]);
                }}
                disabled={!!active || !!pushing}
                className="flex-auto"
              >
                <Play className="size-4" /> Call {queue.length} in a row
              </Button>
            )}
            <Select value={owner} onChange={(e) => setOwner(e.target.value as Owner)} className="w-auto flex-auto" aria-label="Show">
              <option value="mine_or_unassigned">Mine and unassigned</option>
              <option value="everyone">Everyone&apos;s</option>
            </Select>
          </>
        }
      />
      <PageBody className={cn(active && "pb-[28rem] sm:pb-6")}>
        {twilio && aircall === null && dialpad === null && !calling && (
          <p className="mb-4 rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
            To call from Splee, turn on calling in the Twilio card or connect Aircall or Dialpad on the{" "}
            <Link href={`/app/p/${projectId}/integrations`} className="font-medium underline underline-offset-2">
              Integrations page
            </Link>
            . You can still log calls you make from your phone.
          </p>
        )}
        {calling && <VoicemailCard />}
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
                        phones={phones}
                        busy={!!active || !!pushing}
                        pushing={pushing?.row === r.id ? pushing.phone : null}
                        onOpen={() => setLeadId(r.lead_id)}
                        onDial={() => dial(r)}
                        onPhone={(phone) => dialIn(r, phone)}
                        onLog={() => setActive({ row: r, phase: "log" })}
                        onSkip={() => skip(r)}
                        onChanged={() => mutate()}
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
          prep={data?.find((r) => r.id === active.row.id)?.prep ?? active.row.prep}
          onHangUp={hangUp}
          onMute={() => {
            const muted = !active.muted;
            call.current?.call?.mute(muted);
            setActive({ ...active, muted });
          }}
          onLog={(outcome, notes) => log(active.row, outcome, notes)}
          onClose={stop}
          onCallNow={() => callNext(active.row)}
          onVoicemail={voicemail ? () => leaveVoicemail(active.row) : undefined}
          inRow={!!run}
          more={!!run && queue.some((r) => r.id !== active.row.id && !run.includes(r.id))}
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
  phones,
  busy,
  pushing,
  onOpen,
  onDial,
  onPhone,
  onLog,
  onSkip,
  onChanged,
}: {
  row: CallRow;
  calling: boolean;
  /** The connected cloud phones, each with its own button. */
  phones: CloudPhone[];
  busy: boolean;
  /** The phone its number is on its way to. */
  pushing: CloudPhone["key"] | null;
  onOpen: () => void;
  onDial: () => void;
  onPhone: (phone: CloudPhone) => void;
  onLog: () => void;
  onSkip: () => void;
  onChanged: () => Promise<unknown>;
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
          {phones.map((p, i) => (
            <Button
              key={p.key}
              size="sm"
              variant={calling || i > 0 ? "outline" : "primary"}
              onClick={() => onPhone(p)}
              loading={pushing === p.key}
              disabled={busy || !!r.outside_hours || !r.lead_phone}
            >
              <p.icon className="size-3.5" /> Call in {p.name}
            </Button>
          ))}
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
      <TalkTrackRow row={r} onChanged={onChanged} />
    </div>
  );
}

function DoneRow({ row: r, onOpen }: { row: CallRow; onOpen: () => void }) {
  return (
    <div className="flex flex-col gap-3 px-4 py-3.5 sm:px-5">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1">
        <Who row={r} onOpen={onOpen} />
        {r.called_by_name && <span className="text-sm text-muted-foreground">{r.inbound ? "Taken by" : "By"} {r.called_by_name}</span>}
      </div>
      <CallCard call={r} />
    </div>
  );
}

/** Fixed to the bottom corner, or across the bottom on a phone, so the list stays usable while a call is on. */
function CallPanel({
  active: a,
  prep,
  onHangUp,
  onMute,
  onLog,
  onClose,
  onCallNow,
  onVoicemail,
  inRow,
  more,
}: {
  active: Active;
  /** From the polled list, so a track that lands while it rings shows up. */
  prep: CallRow["prep"];
  onHangUp: () => void;
  onMute: () => void;
  onLog: (outcome: CallOutcome, notes: string) => Promise<void>;
  onClose: () => void;
  onCallNow: () => void;
  /** When they recorded a voicemail to leave. */
  onVoicemail?: () => void;
  /** Calling in a row, where a logged call moves on to the next one. */
  inRow: boolean;
  /** Whether the row has another call after this one. */
  more: boolean;
}) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (a.phase !== "connected" && a.phase !== "next") return;
    const id = setInterval(() => setNow(Date.now()), a.phase === "next" ? 250 : 1000);
    return () => clearInterval(id);
  }, [a.phase]);
  useEffect(() => {
    if (a.phase !== "next" || !a.at) return;
    const id = setTimeout(onCallNow, Math.max(0, a.at - Date.now()));
    return () => clearTimeout(id);
  }, [a.phase, a.at, onCallNow]);
  const on = a.phase === "connecting" || a.phase === "ringing" || a.phase === "connected";
  const next = a.phase === "next";
  const elapsed = a.started ? Math.max(0, Math.round(((a.ended ?? now) - a.started) / 1000)) : 0;
  const status = {
    connecting: "Connecting…",
    ringing: "Ringing…",
    connected: a.voicemail ? "Leaving your voicemail…" : `On the call · ${clock(elapsed)}`,
    ended: a.voicemail ? "Your voicemail is playing to them" : a.started ? `Call ended · ${clock(elapsed)}` : "They didn't pick up",
    log: "What happened on the call?",
    next: `Logged. Next call in ${Math.min(NEXT_SECS, Math.max(0, Math.ceil(((a.at ?? now) - now) / 1000)))}s`,
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
        {a.row.instructions && (on || next) && <p className="rounded-lg bg-muted px-3 py-2 text-sm text-pretty">{a.row.instructions}</p>}
        {prep?.prep && (on || next) && (
          <div className="max-h-[45vh] overflow-y-auto border-t border-border pt-3">
            <TalkTrack prep={prep.prep} ringing={a.phase !== "connected"} />
          </div>
        )}
        {next ? (
          <div className="grid grid-cols-2 gap-2">
            <Button variant="outline" onClick={onClose}>
              Stop
            </Button>
            <Button onClick={onCallNow}>
              <Phone className="size-4" /> Call now
            </Button>
          </div>
        ) : on ? (
          <div className="grid grid-cols-2 gap-2">
            {a.phase === "connected" && onVoicemail && (
              <Button variant="outline" onClick={onVoicemail} loading={a.voicemail} className="col-span-2">
                <Voicemail className="size-4" /> Leave your voicemail
              </Button>
            )}
            <Button variant="outline" onClick={onMute} disabled={a.phase !== "connected"}>
              {a.muted ? <MicOff className="size-4" /> : <Mic className="size-4" />} {a.muted ? "Unmute" : "Mute"}
            </Button>
            <Button onClick={onHangUp} className="bg-red-600 text-white hover:bg-red-700">
              <PhoneOff className="size-4" /> Hang up
            </Button>
          </div>
        ) : (
          <OutcomeForm
            initial={a.voicemail ? "voicemail" : a.started ? null : likelyOutcome(a.row.call_status) ?? (a.phase === "ended" ? "no_answer" : null)}
            onSave={onLog}
            onCancel={onClose}
            cancelLabel={a.phase === "log" ? "Cancel" : inRow ? "Stop" : "Later"}
            saveLabel={inRow && more ? "Save and call next" : "Save"}
          />
        )}
      </Card>
    </div>
  );
}
