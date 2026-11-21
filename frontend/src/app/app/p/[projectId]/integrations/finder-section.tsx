"use client";

import { useState } from "react";
import { ArrowDown, ArrowUp, Trash2, UserSearch } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { FINDER_NAMES, type FinderName, type Finders } from "@/lib/types";
import { dateTime } from "@/lib/utils";
import { LastFailure, submit } from "./shared";

/** What each finder asks for when connecting, beyond an API key, and what's worth knowing before picking it. */
const FINDER_KEYS: Partial<Record<FinderName, { key?: string; secret?: string; hint: string }>> = {
  peopledatalabs: { hint: "From the API keys page of your dashboard. Each match uses a credit." },
  rocketreach: { hint: "From Account → API. Each lookup that finds the person uses a credit." },
  snov: { key: "API user ID", secret: "API secret", hint: "Both are under Account settings → API." },
  dropcontact: { hint: "From Settings → API. A search can take a minute or two." },
  contactout: { key: "API token", hint: "From your ContactOut API dashboard." },
  kaspr: { hint: "From Settings → API. Only leads with a LinkedIn profile are looked up." },
  lusha: { hint: "Every search uses a credit, even one that finds no one." },
};

/** The finders whose answers can include a mobile number, for "Find phone numbers too". Keep in sync with Finder::finds_phones. */
const PHONE_FINDERS: FinderName[] = ["peopledatalabs", "rocketreach", "lusha", "prospeo"];

/** The email finders, asked in the order of this list until one finds the address. Keys are write-only. */
export function FinderSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/finders`;
  const { data, error, mutate } = useSWR<Finders>(key, fetcher);
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>) => {
    await mutate(await api<Finders>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  const { pending, run } = useAction();
  const list = data?.finders ?? [];
  const open = (Object.keys(FINDER_NAMES) as FinderName[]).filter((p) => !list.some((f) => f.provider === p));
  const [picked, setPicked] = useState<FinderName>("hunter");
  // Once the picked finder connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const move = (i: number, by: number) => {
    const order = list.map((f) => f.provider);
    const key = `move-${by < 0 ? "up" : "down"}-${order[i]}`;
    [order[i], order[i + by]] = [order[i + by], order[i]];
    run(key, () => call("order", "PUT", { providers: order }));
  };
  const disconnect = (name: string, provider: FinderName) => {
    if (confirm(`Disconnect ${name}? The other finders and the verifier still look for emails.`)) run(`remove-${provider}`, () => call(provider, "DELETE"), `${name} disconnected`);
  };
  const phones = list.filter((f) => PHONE_FINDERS.includes(f.provider)).map((f) => FINDER_NAMES[f.provider]);
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UserSearch className="size-4" /> Email finders
        </span>
      }
      description="Find the email of a lead who came without one, and the real address behind a guessed one. Each is asked in turn, top to bottom, until one finds it."
    >
      {error ? (
        <LoadError error={error} what="your email finders" retry={() => mutate()} />
      ) : (
        data && (
          <>
            {!!list.length && (
              <ol className="mb-4 divide-y divide-border rounded-lg border border-border">
                {list.map((f, i) => {
                  const name = FINDER_NAMES[f.provider];
                  return (
                    <li key={f.provider} className="px-3 py-2">
                      <div className="flex items-center gap-2">
                        <span className="w-5 text-sm text-muted-foreground tabular-nums">{i + 1}.</span>
                        <p className="min-w-0 flex-1 text-sm font-medium">{name}</p>
                        <div className="flex">
                          <Button size="icon" variant="ghost" className="size-8" aria-label={`Move ${name} up`} disabled={i === 0} loading={pending === `move-up-${f.provider}`} onClick={() => move(i, -1)}>
                            <ArrowUp className="size-4" />
                          </Button>
                          <Button
                            size="icon"
                            variant="ghost"
                            className="size-8"
                            aria-label={`Move ${name} down`}
                            disabled={i === list.length - 1}
                            loading={pending === `move-down-${f.provider}`}
                            onClick={() => move(i, 1)}
                          >
                            <ArrowDown className="size-4" />
                          </Button>
                          <Button
                            size="icon"
                            variant="ghost"
                            className="size-8"
                            aria-label={`Disconnect ${name}`}
                            loading={pending === `remove-${f.provider}`}
                            onClick={() => disconnect(name, f.provider)}
                          >
                            <Trash2 className="size-4" />
                          </Button>
                        </div>
                      </div>
                      {/* Under the whole row, so the buttons don't squeeze it into a narrow column on phones. */}
                      {f.paused_until && (
                        <p className="pb-1 pl-7 text-xs text-pretty text-amber-700">
                          Skipped until <span title={dateTime(f.paused_until)}>{timeOf(f.paused_until)}</span>: it&apos;s out of credits or past its rate limit.
                        </p>
                      )}
                      <LastFailure error={f.error} at={f.failed_at} what="lookup" />
                    </li>
                  );
                })}
              </ol>
            )}
            {provider && <FinderForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={list.length ? "Add another" : "Service"} />}
            <div className="mt-4 flex items-start justify-between gap-4 rounded-lg border border-border px-3 py-2.5">
              <span>
                <span className="block text-sm font-medium">Find phone numbers too</span>
                <span className="block text-xs text-pretty text-muted-foreground">
                  {phones.length
                    ? `${andList(phones)} ${phones.length === 1 ? "also fills" : "also fill"} in a missing mobile number, which costs more credits.`
                    : "People Data Labs, RocketReach, Lusha and Prospeo can also fill in a missing mobile number, which costs more credits."}
                </span>
              </span>
              <Switch
                checked={data.find_phones}
                onChange={(on) => run("phones", () => call("phones", "PUT", { on }), on ? "Finding phone numbers too" : "Not finding phone numbers")}
                disabled={pending === "phones"}
                label="Find phone numbers too"
              />
            </div>
          </>
        )
      )}
    </Section>
  );
}

/** "A", "A and B", or "A, B and C". */
const andList = (names: string[]) => (names.length < 2 ? names.join("") : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`);

/** Today's time, or the date and time when it's later than today. */
const timeOf = (ts: string) => {
  const at = new Date(ts);
  const time = at.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return at.toDateString() === new Date().toDateString() ? time : `${at.toLocaleDateString([], { month: "short", day: "numeric" })}, ${time}`;
};

/** Keyed by the finder, so picking another one starts with empty fields. */
function FinderForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: FinderName;
  open: FinderName[];
  pick: (p: FinderName) => void;
  call: (path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>) => Promise<void>;
  label: string;
}) {
  const info = FINDER_KEYS[provider];
  const [key, setKey] = useState("");
  const [secret, setSecret] = useState("");
  const { pending, run } = useAction();
  const name = FINDER_NAMES[provider];
  const keyLabel = info?.key ?? "API key";
  const connect = () => run("connect", () => call(provider, "PUT", { key, secret }), `Connected to ${name}`);
  const button = (
    <Button type="submit" loading={pending === "connect"} disabled={!key || (!!info?.secret && !secret)}>
      Connect
    </Button>
  );
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
      <Field label={label} className="sm:w-52">
        <Select value={provider} onChange={(e) => pick(e.target.value as FinderName)}>
          {open.map((p) => (
            <option key={p} value={p}>
              {FINDER_NAMES[p]}
            </option>
          ))}
        </Select>
      </Field>
      {info?.secret ? (
        <>
          <Field label={keyLabel} className="flex-1" hint={info.hint}>
            <Input type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
          </Field>
          <Field label={info.secret} className="flex-1">
            <div className="flex gap-2">
              <Input aria-label={info.secret} type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
              {button}
            </div>
          </Field>
        </>
      ) : (
        <Field label={keyLabel} className="flex-1" hint={info?.hint ?? "Each address found uses one of the service's credits."}>
          <div className="flex gap-2">
            {/* The label also wraps Connect, so name the input itself. */}
            <Input aria-label={keyLabel} type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
            {button}
          </div>
        </Field>
      )}
    </form>
  );
}
