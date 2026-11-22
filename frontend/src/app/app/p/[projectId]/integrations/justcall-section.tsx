"use client";

import { useState } from "react";
import { PhoneCall } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { CloudPhoneConnection } from "@/lib/types";
import { dateTime, plural, timeAgo } from "@/lib/utils";
import { LastFailure, submit } from "./shared";

/** The project's JustCall account. Connecting checks the API key and secret and adds Splee's webhook URL to JustCall's call
 *  events, so there's no URL to paste. Both are write-only. */
export function JustcallSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/justcall`;
  const { data, error, mutate } = useSWR<CloudPhoneConnection | null>(key, fetcher);
  const [apiKey, setApiKey] = useState("");
  const [apiSecret, setApiSecret] = useState("");
  const { pending, run } = useAction();
  const call = async (method: "PUT" | "DELETE", body?: { api_key: string; api_secret: string }) => {
    await mutate(await api<CloudPhoneConnection | null>(key, { method, body }), { revalidate: false });
  };
  const connect = async () => {
    if (await run("connect", () => call("PUT", { api_key: apiKey, api_secret: apiSecret }), "Connected to JustCall")) {
      setApiKey("");
      setApiSecret("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect JustCall? New calls won't come onto your leads. The ones Splee has stay."))
      run("disconnect", () => call("DELETE"), "JustCall disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <PhoneCall className="size-4" /> JustCall
        </span>
      }
      description="Brings each call your team makes or takes in JustCall onto the lead with that number, with how it went and the recording or voicemail. When JustCall AI transcribes it, the agent sums it up with a next step."
      actions={
        data ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : data ? (
        <p className="text-sm text-pretty">
          {data.calls ? `${plural(data.calls, "call")} with your leads so far.` : "No calls with your leads yet."}{" "}
          {data.last_event_at && (
            <>
              JustCall last sent an event <span title={dateTime(data.last_event_at)}>{timeAgo(data.last_event_at)}</span>.{" "}
            </>
          )}
          A call closes the lead&apos;s next call step on the Calls page, or is logged there on its own. Calls with numbers that aren&apos;t a
          lead&apos;s stay in JustCall.
        </p>
      ) : (
        data === null && (
          <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
            <Field label="API key" className="sm:w-56">
              <Input type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value.trim())} />
            </Field>
            <Field label="API secret" className="flex-1" hint="In JustCall, your profile menu → APIs and Webhooks. It takes the Team plan or above.">
              <div className="flex gap-2">
                {/* The label also wraps Connect, so name the input itself. */}
                <Input
                  aria-label="API secret"
                  type="password"
                  autoComplete="off"
                  value={apiSecret}
                  onChange={(e) => setApiSecret(e.target.value.trim())}
                />
                <Button type="submit" loading={pending === "connect"} disabled={!apiKey || !apiSecret}>
                  Connect
                </Button>
              </div>
            </Field>
          </form>
        )
      )}
      <LastFailure error={data?.error || null} at={data?.last_event_at ?? null} what="event" />
    </Section>
  );
}
