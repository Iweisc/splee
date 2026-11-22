"use client";

import { useState } from "react";
import Link from "next/link";
import { PhoneOutgoing } from "lucide-react";
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

/** The project's Dialpad company. Connecting checks the API key and registers Splee's signed webhook in Dialpad, so there's
 *  no URL to paste. The key is write-only. */
export function DialpadSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/dialpad`;
  const { data, error, mutate } = useSWR<CloudPhoneConnection | null>(key, fetcher);
  const [apiKey, setApiKey] = useState("");
  const { pending, run } = useAction();
  const call = async (method: "PUT" | "DELETE", body?: { api_key: string }) => {
    await mutate(await api<CloudPhoneConnection | null>(key, { method, body }), { revalidate: false });
  };
  const connect = async () => {
    if (await run("connect", () => call("PUT", { api_key: apiKey }), "Connected to Dialpad")) setApiKey("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Dialpad? New calls won't come onto your leads. The ones Splee has stay.")) run("disconnect", () => call("DELETE"), "Dialpad disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <PhoneOutgoing className="size-4" /> Dialpad
        </span>
      }
      description="Brings each call your team makes or takes in Dialpad onto the lead with that number, with how it went and the recording. When Dialpad transcribes it, the agent sums it up with a next step."
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
              Dialpad last sent an event <span title={dateTime(data.last_event_at)}>{timeAgo(data.last_event_at)}</span>.{" "}
            </>
          )}
          A call closes the lead&apos;s next call step on the{" "}
          <Link href={`/app/p/${projectId}/calls`} className="font-medium underline underline-offset-2">
            Calls page
          </Link>
          , or is logged there on its own. Calls with numbers that aren&apos;t a
          lead&apos;s stay in Dialpad.
        </p>
      ) : (
        data === null && (
          <form onSubmit={submit(connect)}>
            <Field
              label="API key"
              hint="In Dialpad, Admin Settings → My Company → Authentication → API keys. Only a company admin can. Give it the recordings_export scope for recordings."
            >
              <div className="flex gap-2">
                {/* The label also wraps Connect, so name the input itself. */}
                <Input aria-label="API key" type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value.trim())} />
                <Button type="submit" loading={pending === "connect"} disabled={!apiKey}>
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
