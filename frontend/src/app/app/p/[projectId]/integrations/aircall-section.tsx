"use client";

import { useState } from "react";
import Link from "next/link";
import { Headset } from "lucide-react";
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

const EMPTY = { api_id: "", api_token: "" };

/** The project's Aircall company. Connecting checks the API ID and token and registers Splee's webhook in Aircall, so there's
 *  no URL to paste. The token is write-only. */
export function AircallSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/aircall`;
  const { data, error, mutate } = useSWR<CloudPhoneConnection | null>(key, fetcher);
  const [form, setForm] = useState(EMPTY);
  const set = (field: keyof typeof EMPTY) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [field]: e.target.value.trim() });
  const { pending, run } = useAction();
  const call = async (method: "PUT" | "DELETE", body?: typeof EMPTY) => {
    await mutate(await api<CloudPhoneConnection | null>(key, { method, body }), { revalidate: false });
  };
  const connect = async () => {
    if (await run("connect", () => call("PUT", form), "Connected to Aircall")) setForm(EMPTY);
  };
  const disconnect = () => {
    if (confirm("Disconnect Aircall? New calls won't come onto your leads. The ones Splee has stay.")) run("disconnect", () => call("DELETE"), "Aircall disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Headset className="size-4" /> Aircall
        </span>
      }
      description="Brings each call your team makes or takes in Aircall onto the lead with that number, with how it went and the recording. When Aircall transcribes it, the agent sums it up with a next step."
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
              Aircall last sent an event <span title={dateTime(data.last_event_at)}>{timeAgo(data.last_event_at)}</span>.{" "}
            </>
          )}
          A call closes the lead&apos;s next call step on the{" "}
          <Link href={`/app/p/${projectId}/calls`} className="font-medium underline underline-offset-2">
            Calls page
          </Link>
          , or is logged there on its own. Calls with numbers that aren&apos;t a
          lead&apos;s stay in Aircall.
        </p>
      ) : (
        data === null && (
          <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
            <Field label="API ID" className="sm:w-56">
              <Input autoComplete="off" value={form.api_id} onChange={set("api_id")} />
            </Field>
            <Field label="API token" className="flex-1" hint="In Aircall, Company settings → API keys → Add a new API key. Only an admin can.">
              <div className="flex gap-2">
                {/* The label also wraps Connect, so name the input itself. */}
                <Input aria-label="API token" type="password" autoComplete="off" value={form.api_token} onChange={set("api_token")} />
                <Button type="submit" loading={pending === "connect"} disabled={!form.api_id || !form.api_token}>
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
