"use client";

import { useState } from "react";
import { CreditCard } from "lucide-react";
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
import { PAYMENT_NAMES, PAYMENT_PROVIDERS, type PaymentConnection, type PaymentProvider } from "@/lib/types";
import { dateTime, num, plural, timeAgo } from "@/lib/utils";
import { submit, linkClass } from "./shared";

/** Where each provider keeps the key and the webhook. Event names are as the provider writes them. */
const PAYMENTS: Record<PaymentProvider, { key: string; keyHint: string; webhook: React.ReactNode; secretHint: string }> = {
  stripe: {
    key: "Restricted key",
    keyHint: "In Stripe, Developers → API keys → Create restricted key, with Read on Customers, Charges and Refunds.",
    webhook: (
      <>
        In Stripe, Developers → Webhooks → Add endpoint with this URL and the events <code>charge.succeeded</code> and <code>charge.refunded</code>.
      </>
    ),
    secretHint: "From the endpoint's page in Stripe. It starts with whsec_.",
  },
  paddle: {
    key: "API key",
    keyHint: "In Paddle, Developer tools → Authentication → New API key, with Read on transactions, customers and adjustments.",
    webhook: (
      <>
        In Paddle, Developer tools → Notifications → New destination with this URL and the events <code>transaction.completed</code> and{" "}
        <code>adjustment.updated</code>.
      </>
    ),
    secretHint: "The destination's secret key, from its menu in Paddle.",
  },
  lemonsqueezy: {
    key: "API key",
    keyHint: "In Lemon Squeezy, Settings → API → New API key.",
    webhook: (
      <>
        In Lemon Squeezy, Settings → Webhooks → add this URL with a signing secret you make up, and tick <code>order_created</code>,{" "}
        <code>order_refunded</code>, <code>subscription_payment_success</code> and <code>subscription_payment_refunded</code>.
      </>
    ),
    secretHint: "The signing secret you typed in Lemon Squeezy.",
  },
};

function lookBack(c: PaymentConnection) {
  if (c.backfill_status === "queued" || c.backfill_status === "running") return "Looking for payments from the last 90 days…";
  if (c.backfill_status === "failed") return "Couldn't look back over the last 90 days.";
  return `${plural(c.backfill_found, "payment")} in the last 90 days, ${num(c.backfill_matched)} from your leads.`;
}

/** Each provider connected to the project, and a form to connect another. Connecting looks back 90 days, which the list
 *  polls until it's done. Keys and signing secrets are write-only. */
export function PaymentsSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/payments`;
  const { data, error, mutate } = useSWR<PaymentConnection[]>(key, fetcher, {
    refreshInterval: (list) => (list?.some((c) => c.backfill_status === "queued" || c.backfill_status === "running") ? 3000 : 0),
  });
  const open = PAYMENT_PROVIDERS.filter((p) => !data?.some((c) => c.provider === p));
  const [picked, setPicked] = useState<PaymentProvider>("stripe");
  // Once the picked provider connects it leaves the list, so the form moves on to the next one.
  const provider = open.includes(picked) ? picked : open[0];
  const call = async (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => {
    await mutate(await api<PaymentConnection[]>(`${key}/${path}`, { method, body }), { revalidate: false });
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <CreditCard className="size-4" /> Payments
        </span>
      }
      description="When a lead, or anyone at their company, pays through Stripe, Paddle or Lemon Squeezy, Splee wins their deal with the amount and counts the revenue on Analytics."
    >
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((c) => (
                <ConnectedPayments key={c.provider} connection={c} call={call} />
              ))}
            </ul>
          )}
          {data && provider && <PaymentForm key={provider} provider={provider} open={open} pick={setPicked} call={call} label={data.length ? "Connect another" : "Provider"} />}
        </>
      )}
    </Section>
  );
}

export type PaymentCall = (path: string, method: "PUT" | "DELETE", body?: Record<string, string>) => Promise<void>;

function ConnectedPayments({ connection: c, call }: { connection: PaymentConnection; call: PaymentCall }) {
  const origin = useOrigin();
  const name = PAYMENT_NAMES[c.provider];
  const { pending, run } = useAction();
  const [secret, setSecret] = useState("");
  const [replacing, setReplacing] = useState(false);
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? New payments won't win deals or count as revenue. The ones Splee has stay.`)) run("disconnect", () => call(c.provider, "DELETE"), `${name} disconnected`);
  };
  const save = async () => {
    if (await run("secret", () => call(`${c.provider}/secret`, "PUT", { secret }), "Signing secret saved")) {
      setSecret("");
      setReplacing(false);
    }
  };
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-muted-foreground">{lookBack(c)}</p>
        </div>
        <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "disconnect"}>
          Disconnect
        </Button>
      </div>
      {c.backfill_status === "failed" && c.backfill_error && <p className="mt-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">{c.backfill_error}</p>}
      {/* Not a Field: its <label> wrapper would turn a click on the label text into a copy. */}
      <div className="mt-3 flex flex-col gap-1.5">
        <span className="text-sm font-medium">Webhook URL</span>
        <CopyField value={`${origin}/api/payments/${c.provider}/${c.webhook_token}`} />
        <p className="text-xs text-pretty text-muted-foreground">{PAYMENTS[c.provider].webhook}</p>
      </div>
      {c.has_secret && !replacing ? (
        <p className="mt-3 flex flex-wrap items-center gap-x-2 text-xs text-muted-foreground">
          <span>
            Signing secret saved.{" "}
            {c.last_event_at ? (
              <>
                Last event <span title={dateTime(c.last_event_at)}>{timeAgo(c.last_event_at)}</span>.
              </>
            ) : (
              "No events yet."
            )}
          </span>
          <button type="button" onClick={() => setReplacing(true)} className={linkClass}>
            Replace
          </button>
        </p>
      ) : (
        <form onSubmit={submit(save)} className="mt-3">
          <Field label="Signing secret" hint={PAYMENTS[c.provider].secretHint}>
            <div className="flex gap-2">
              <Input aria-label="Signing secret" type="password" autoComplete="off" value={secret} onChange={(e) => setSecret(e.target.value.trim())} />
              <Button type="submit" loading={pending === "secret"} disabled={!secret}>
                Save
              </Button>
            </div>
          </Field>
        </form>
      )}
    </li>
  );
}

/** Keyed by the provider, so picking another one starts with an empty key. */
function PaymentForm({
  provider,
  open,
  pick,
  call,
  label,
}: {
  provider: PaymentProvider;
  open: PaymentProvider[];
  pick: (p: PaymentProvider) => void;
  call: PaymentCall;
  label: string;
}) {
  const info = PAYMENTS[provider];
  const [apiKey, setApiKey] = useState("");
  const { pending, run } = useAction();
  const connect = () => run("connect", () => call(provider, "PUT", { api_key: apiKey }), `Connected to ${PAYMENT_NAMES[provider]}`);
  return (
    <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
      <Field label={label} className="sm:w-52">
        <Select value={provider} onChange={(e) => pick(e.target.value as PaymentProvider)}>
          {open.map((p) => (
            <option key={p} value={p}>
              {PAYMENT_NAMES[p]}
            </option>
          ))}
        </Select>
      </Field>
      <Field label={info.key} hint={info.keyHint} className="flex-1">
        <div className="flex gap-2">
          {/* The label also wraps Connect, so name the input itself. */}
          <Input aria-label={info.key} type="password" autoComplete="off" value={apiKey} onChange={(e) => setApiKey(e.target.value.trim())} />
          <Button type="submit" loading={pending === "connect"} disabled={!apiKey}>
            Connect
          </Button>
        </div>
      </Field>
    </form>
  );
}
