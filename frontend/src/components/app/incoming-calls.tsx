"use client";

import { useEffect, useRef, useState } from "react";
import Link from "next/link";
import { Mic, MicOff, Phone, PhoneOff } from "lucide-react";
import useSWR, { mutate } from "swr";
import { clock } from "@/components/app/work/calls";
import { toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Avatar, Card } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { TwilioStatus } from "@/lib/types";
import { loadVoiceSdk, type VoiceCall, type VoiceDevice } from "@/lib/twilio-voice";
import { cn } from "@/lib/utils";

interface Ringing {
  call: VoiceCall;
  name: string;
  company: string;
  /** The lead, for a number one of them has. */
  href: string | null;
  /** When it was answered. */
  started?: number;
  muted?: boolean;
}

/**
 * Rings on any page of the app when someone calls one of the organization's Twilio numbers, once calling is on. The browser
 * registers with Twilio as this member, and Splee's TwiML rings whoever should take the call, so answering here talks to the
 * lead, and the call is kept on them like one made from the Calls page.
 */
export function IncomingCalls() {
  const { data: twilio } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const on = !!twilio?.connection?.twiml_app_sid;
  const [ringing, setRinging] = useState<Ringing | null>(null);
  const current = useRef<VoiceCall | null>(null);

  useEffect(() => {
    if (!on) return;
    let device: VoiceDevice | null = null;
    let stopped = false;
    const token = () => api<{ token: string }>("/calls/ring-token", { method: "POST" }).then((t) => t.token);
    const ring = (call: VoiceCall) => {
      // One at a time. Another caller goes on ringing the rest of the team, or on to leave a message.
      if (current.current) return call.reject();
      current.current = call;
      const p = call.customParameters;
      const [lead, project] = [p.get("lead_id"), p.get("project_id")];
      const href = lead && project ? `/app/p/${project}/inbox?lead=${lead}` : null;
      setRinging({ call, name: p.get("name") || call.parameters.From || "Someone", company: p.get("company") ?? "", href });
      const end = () => {
        if (current.current !== call) return;
        current.current = null;
        setRinging(null);
        // The call is on the lead now, so the Calls page shows it.
        void mutate((key) => typeof key === "string" && key.includes("/calls"));
      };
      call.on("accept", () => setRinging((r) => (r?.call === call ? { ...r, started: Date.now() } : r)));
      call.on("cancel", end);
      call.on("reject", end);
      call.on("disconnect", end);
      call.on("error", (e) => {
        toastError(new Error(e.message || "The call dropped"));
        end();
      });
    };
    void (async () => {
      try {
        const [{ Device }, first] = await Promise.all([loadVoiceSdk(), token()]);
        if (stopped) return;
        device = new Device(first, { codecPreferences: ["opus", "pcmu"] });
        device.on("tokenWillExpire", () => void token().then((t) => device?.updateToken(t), () => {}));
        device.on("incoming", ring);
        await device.register();
      } catch {
        // Ringing here is a convenience, so it stays quiet when it can't start. Calls nobody takes still get a message.
      }
    })();
    return () => {
      stopped = true;
      device?.destroy();
    };
  }, [on]);

  return ringing && <IncomingCall ringing={ringing} onMute={(muted) => setRinging({ ...ringing, muted })} />;
}

/** Like the Calls page's panel: fixed to the bottom corner, or across the bottom on a phone. */
function IncomingCall({ ringing: r, onMute }: { ringing: Ringing; onMute: (muted: boolean) => void }) {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!r.started) return;
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, [r.started]);
  const mute = () => {
    r.call.mute(!r.muted);
    onMute(!r.muted);
  };
  return (
    <div role="dialog" aria-label={`Call from ${r.name}`} className="fixed inset-x-3 bottom-3 z-50 sm:inset-x-auto sm:right-6 sm:bottom-6 sm:w-96">
      <Card className="flex flex-col gap-4 p-4 shadow-xl ring-1 ring-border">
        <div className="flex items-center gap-3">
          <Avatar name={r.name} />
          <div className="min-w-0 flex-1">
            <p className="truncate font-medium">{r.name}</p>
            {r.company && <p className="truncate text-sm text-muted-foreground">{r.company}</p>}
          </div>
          {r.href && (
            <Link href={r.href} className="shrink-0 text-sm font-medium text-brand-700 hover:underline">
              Open lead
            </Link>
          )}
        </div>
        <p className={cn("text-sm", r.started ? "font-medium text-brand-700" : "text-muted-foreground")} aria-live="polite">
          {r.started ? `On the call · ${clock(Math.max(0, Math.round((now - r.started) / 1000)))}` : "Calling you…"}
        </p>
        <div className="grid grid-cols-2 gap-2">
          {r.started ? (
            <>
              <Button variant="outline" onClick={mute}>
                {r.muted ? <MicOff className="size-4" /> : <Mic className="size-4" />} {r.muted ? "Unmute" : "Mute"}
              </Button>
              <Button onClick={() => r.call.disconnect()} className="bg-red-600 text-white hover:bg-red-700">
                <PhoneOff className="size-4" /> Hang up
              </Button>
            </>
          ) : (
            <>
              <Button variant="outline" onClick={() => r.call.reject()}>
                <PhoneOff className="size-4" /> Decline
              </Button>
              <Button onClick={() => r.call.accept()}>
                <Phone className="size-4" /> Answer
              </Button>
            </>
          )}
        </div>
      </Card>
    </div>
  );
}
