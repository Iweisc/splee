"use client";

import { useState } from "react";
import { Mic } from "lucide-react";
import useSWR from "swr";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import { MEETING_NAMES, MEETING_POLLED, MEETING_PROVIDERS, type MeetingConnection, type MeetingProvider } from "@/lib/types";
import { dateTime, plural, timeAgo } from "@/lib/utils";
import type { PaymentCall } from "./payments-section";
import { submit, linkClass } from "./shared";

type MeetingField = { name: string; label: string; secret?: boolean };

/** What each provider asks for and where its webhook goes. Gong and tl;dv have no webhook: Splee reads them every 15 minutes. */
const MEETING_SETUP: Record<MeetingProvider, { fields: MeetingField[]; hint: string; webhook?: React.ReactNode; secretHint?: string }> = {
  fireflies: {
    fields: [{ name: "api_key", label: "API key", secret: true }],
    hint: "In Fireflies, Integrations → Fireflies API → copy your API key.",
    webhook: <>In Fireflies, Settings → Developer settings → Webhook, paste this URL and set a webhook secret.</>,
    secretHint: "The webhook secret you set in Fireflies.",
  },
  fathom: {
    fields: [{ name: "api_key", label: "API key", secret: true }],
    hint: "In Fathom, Settings → API Access → Generate API key.",
    webhook: <>In Fathom, Settings → API Access → Add webhook with this URL, with the transcript and summary included.</>,
    secretHint: "The webhook's secret from Fathom. It starts with whsec_.",
  },
  gong: {
    fields: [
      { name: "access_key", label: "Access key" },
      { name: "access_secret", label: "Access key secret", secret: true },
    ],
    hint: "In Gong, Company settings → Ecosystem → API → Create. A technical admin can make one.",
  },
  zoom: {
    fields: [
      { name: "account_id", label: "Account ID" },
      { name: "client_id", label: "Client ID" },
      { name: "client_secret", label: "Client secret", secret: true },
      { name: "secret_token", label: "Secret token", secret: true },
    ],
    hint: "In Zoom's App Marketplace, build a Server-to-Server OAuth app with the cloud recording and past meeting scopes. The secret token is on its Features page.",
    webhook: (
      <>
        On the Zoom app&apos;s Features page, turn on event subscriptions, add this URL and the event <code>Recording → Transcript files have completed</code>.
      </>
    ),
  },
  tldv: {
    fields: [{ name: "api_key", label: "API key", secret: true }],
    hint: "In tl;dv, Settings → Personal settings → API keys, then make a key.",
  },
};

function meetingStatus(c: MeetingConnection) {
  const kept = c.meetings ? `${plural(c.meetings, "meeting")} with your leads so far.` : "No meetings with your leads yet.";
  if (MEETING_POLLED.includes(c.provider)) return c.synced_at ? `${kept} Reads new meetings every 15 minutes.` : "Reading the last week of meetings…";
  return kept;
}

/** Each meeting recorder connected to the project, and a form to connect another. Keys and secrets are write-only. */
export function MeetingsSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/meeting-connections`;
  const { data, error, mutate } = useSWR<MeetingConnection[]>(key, fetcher, {
    refreshInterval: (list) => (list?.some((c) => MEETING_POLLED.includes(c.provider) && !c.synced_at) ? 5000 : 0),
  });
  const open = MEETING_PROVIDERS.filter((p) => !data?.some((c) => c.provider === p));
  const [picked, setPicked] = useState<MeetingProvider>("fireflies");
  // Once the picked provider connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => {
    await mutate(await api<MeetingConnection[]>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  return (
    <div id="meeting-notes" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <Mic className="size-4" /> Meeting notes
          </span>
        }
        description="After a meeting with a lead in Fireflies, Fathom, Gong, Zoom or tl;dv, the agent sums it up, drafts the follow-up for you to approve and suggests the deal's next stage."
      >
        {error ? (
          <LoadError error={error} retry={() => mutate()} />
        ) : (
          <>
            {!!data?.length && (
              <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
                {data.map((c) => (
                  <ConnectedMeetings key={c.provider} connection={c} call={call} />
                ))}
              </ul>
            )}
            {data && provider && <MeetingForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={data.length ? "Connect another" : "Recorder"} />}
          </>
        )}
      </Section>
    </div>
  );
}

function ConnectedMeetings({ connection: c, call }: { connection: MeetingConnection; call: PaymentCall }) {
  const origin = useOrigin();
  const name = MEETING_NAMES[c.provider];
  const setup = MEETING_SETUP[c.provider];
  const { pending, run } = useAction();
  const [secret, setSecret] = useState("");
  const [replacing, setReplacing] = useState(false);
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New meetings won't come into Splee. The ones already here stay.`)) run("disconnect", () => call(c.provider, "DELETE"), `${name} disconnected`);
  };
  const save = async () => {
    if (await run("secret", () => call(`${c.provider}/secret`, "PUT", { secret }), "Webhook secret saved")) {
      setSecret("");
      setReplacing(false);
    }
  };
  const asksForSecret = c.provider === "fireflies" || c.provider === "fathom";
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-muted-foreground">{meetingStatus(c)}</p>
        </div>
        <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
          Disconnect
        </Button>
      </div>
      {c.error && <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">{c.error}</p>}
      {setup.webhook && (
        // Not a Field: its <label> wrapper would turn a click on the label text into a copy.
        <div className="mt-3 flex flex-col gap-1.5">
          <span className="text-sm font-medium">Webhook URL</span>
          <CopyField value={`${origin}/api/meeting-webhooks/${c.provider}/${c.webhook_token}`} />
          <p className="text-xs text-pretty text-muted-foreground">{setup.webhook}</p>
        </div>
      )}
      {setup.webhook &&
        (!asksForSecret || (c.has_secret && !replacing) ? (
          <p className="mt-3 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
            <span>
              {asksForSecret ? "Secret saved. " : ""}
              {c.last_event_at ? (
                <>
                  Last event <span title={dateTime(c.last_event_at)}>{timeAgo(c.last_event_at)}</span>.
                </>
              ) : (
                "No events yet."
              )}
            </span>
            {asksForSecret && (
              <button type="button" onClick={() => setReplacing(true)} className={linkClass}>
                Replace
              </button>
            )}
          </p>
        ) : (
          <form onSubmit={submit(save)} className="mt-3">
            <Field label="Webhook secret" hint={setup.secretHint}>
              <div className="flex gap-2">
                <Input aria-label="Webhook secret" type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
                <Button type="submit" loading={pending === "secret"} disabled={!secret}>
                  Save
                </Button>
              </div>
            </Field>
          </form>
        ))}
    </li>
  );
}

/** Keyed by the provider, so picking another one starts with empty boxes. */
function MeetingForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: MeetingProvider;
  open: MeetingProvider[];
  pick: (p: MeetingProvider) => void;
  call: PaymentCall;
  label: string;
}) {
  const setup = MEETING_SETUP[provider];
  const [values, setValues] = useState<Record<string, string>>({});
  const { pending, run } = useAction();
  const filled = setup.fields.every((f) => values[f.name]);
  const connect = () => run("connect", () => call(provider, "PUT", values), `Connected to ${MEETING_NAMES[provider]}`);
  const input = (f: MeetingField) => (
    <Input
      aria-label={f.label}
      type={f.secret ? "password" : "text"}
      autoComplete="off"
      value={values[f.name] ?? ""}
      onChange={(e) => setValues((v) => ({ ...v, [f.name]: e.target.value.trim() }))}
    />
  );
  const button = (
    <Button type="submit" loading={pending === "connect"} disabled={!filled}>
      Connect
    </Button>
  );
  const select = (
    <Field label={label} className="sm:w-52">
      <Select value={provider} onChange={(e) => pick(e.target.value as MeetingProvider)}>
        {open.map((p) => (
          <option key={p} value={p}>
            {MEETING_NAMES[p]}
          </option>
        ))}
      </Select>
    </Field>
  );
  if (setup.fields.length === 1) {
    const [f] = setup.fields;
    return (
      <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
        {select}
        <Field label={f.label} hint={setup.hint} className="flex-1">
          <div className="flex gap-2">
            {input(f)}
            {button}
          </div>
        </Field>
      </form>
    );
  }
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4">
      {select}
      <div className="grid gap-4 sm:grid-cols-2">
        {setup.fields.map((f) => (
          <Field key={f.name} label={f.label}>
            {input(f)}
          </Field>
        ))}
      </div>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <p className="text-xs text-pretty text-muted-foreground sm:max-w-md">{setup.hint}</p>
        {button}
      </div>
    </form>
  );
}
