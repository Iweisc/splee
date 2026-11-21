"use client";

import { useState } from "react";
import { MapPin } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, NumberInput } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { LocalUsage } from "@/lib/types";
import { num } from "@/lib/utils";
import { submit } from "./shared";

/** The organization's Google Maps Platform key for the Companies page's Local businesses tab, shared by every project.
 *  It's write-only, and saving it checks it with one free call to Google. */
export function GoogleMapsSection() {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const { data, error, mutate } = useSWR<LocalUsage>("/local-businesses/key", fetcher);
  const [key, setKey] = useState("");
  const [cap, setCap] = useState<number | null>(null);
  const { pending, run } = useAction();
  const call = (method: "PUT" | "DELETE", path: string, body?: Record<string, unknown>) => async () => {
    await mutate(await api<LocalUsage>(`/local-businesses/${path}`, { method, body }), { revalidate: false });
  };
  const connect = async () => {
    if (await run("connect", call("PUT", "key", { key }), "Connected to Google Maps")) setKey("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Google Maps? Local business search stops for every project until a key is connected again.")) {
      run("disconnect", call("DELETE", "key"), "Google Maps disconnected");
    }
  };
  const saveCap = async () => {
    if (cap !== null && (await run("cap", call("PUT", "cap", { cap }), "Monthly cap saved"))) setCap(null);
  };
  return (
    <div id="google-maps" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <MapPin className="size-4" /> Google Maps
          </span>
        }
        description="Finds local businesses by what they are and where on the Companies page. The key is shared by your organization's projects, and Google bills its calls to your account."
        actions={
          data?.connected && admin ? (
            <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
              Disconnect
            </Button>
          ) : undefined
        }
      >
        {error ? (
          <LoadError error={error} what="your Google Maps key" retry={() => mutate()} />
        ) : !data ? null : data.connected ? (
          <div className="space-y-4">
            <p className="text-sm text-pretty">
              Connected. {num(data.calls)} of {num(data.cap)} calls used this month, about ${(data.cost_cents / 100).toFixed(2)} at $
              {(data.price_per_1000_cents / 100).toFixed(2)} per 1,000. Searches stop at the cap until the 1st of next month.
            </p>
            {admin && (
              <form onSubmit={submit(saveCap)} className="flex items-end gap-2">
                <Field label="Monthly cap" hint="Each place searched costs up to 3 calls." className="w-full sm:w-72">
                  <div className="flex gap-2">
                    <NumberInput aria-label="Monthly cap" min={0} max={1000000} value={cap ?? data.cap} onValueChange={setCap} />
                    <Button type="submit" loading={pending === "cap"} disabled={cap === null || cap === data.cap}>
                      Save
                    </Button>
                  </div>
                </Field>
              </form>
            )}
          </div>
        ) : admin ? (
          <form onSubmit={submit(connect)}>
            <Field
              label="API key"
              hint="In Google Cloud, turn on Places API (New), then create a key under APIs & Services, Credentials. Splee checks it with one free call."
            >
              <div className="flex gap-2">
                <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
                <Button type="submit" loading={pending === "connect"} disabled={!key}>
                  Connect
                </Button>
              </div>
            </Field>
          </form>
        ) : (
          <p className="text-sm text-muted-foreground">Ask an admin of your organization to connect a Google Maps key.</p>
        )}
      </Section>
    </div>
  );
}
