"use client";

import { useState } from "react";
import { MailCheck } from "lucide-react";
import { Section } from "@/components/app/settings/layout";
import { Button } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import type { ProjectSettings } from "@/lib/types";
import { LastFailure, useConnection, submit } from "./shared";

const VERIFIERS = {
  neverbounce: "NeverBounce",
  millionverifier: "MillionVerifier",
  hunter: "Hunter",
  kickbox: "Kickbox",
  bouncer: "Bouncer",
  emailable: "Emailable",
  clearout: "Clearout",
  reoon: "Reoon",
  debounce: "DeBounce",
  emaillistverify: "EmailListVerify",
  bouncify: "Bouncify",
  quickemailverification: "QuickEmailVerification",
  abstract: "Abstract API",
  mailboxvalidator: "MailboxValidator",
  myemailverifier: "MyEmailVerifier",
} as const;

export function VerifierSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  // Hunter connected here also finds addresses, after the email finders, as it did before Email finders existed.
  const finds = settings.verifier === "hunter" ? " Hunter also finds emails, after any email finders." : "";
  return (
    <ServiceKeySection
      projectId={projectId}
      path="verifier"
      services={VERIFIERS}
      current={settings.verifier}
      error={settings.verifier_error}
      failedAt={settings.verifier_failed_at}
      title={
        <>
          <MailCheck className="size-4" /> Email verification
        </>
      }
      description="Checks each address before its first email, so bad ones fail here instead of bouncing."
      connected={(name) => `Connected to ${name}. Addresses it can't confirm either way, like catch-all domains, still get their email.${finds}`}
      disconnectWarning="New addresses will be sent to without a check."
      creditHint="Each check uses one of the service's credits."
    />
  );
}

/** A service connected with an API key, which is write-only like a CRM token. Connecting checks it with the service first. */
function ServiceKeySection<K extends string>({
  projectId,
  path,
  services,
  current,
  error,
  failedAt,
  title,
  description,
  connected,
  disconnectWarning,
  creditHint,
}: {
  projectId: string;
  path: string;
  services: Record<K, string>;
  current: K | "";
  error: string | null;
  failedAt: string | null;
  title: React.ReactNode;
  description: string;
  connected: (name: string) => string;
  disconnectWarning: string;
  creditHint: string;
}) {
  const [provider, setProvider] = useState(Object.keys(services)[0] as K);
  const [key, setKey] = useState("");
  const { busy, call } = useConnection(projectId, path);
  const connect = async () => {
    if (await call({ provider, key }, `Connected to ${services[provider]}`)) setKey("");
  };
  const name = current ? services[current] : null;
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? ${disconnectWarning}`)) call(undefined, `${name} disconnected`);
  };
  return (
    <Section
      title={<span className="flex items-center gap-2">{title}</span>}
      description={description}
      actions={
        name ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {name ? (
        <p className="text-sm">{connected(name)}</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4 sm:flex-row">
          <Field label="Service" className="sm:w-52">
            <Select value={provider} onChange={(e) => setProvider(e.target.value as K)}>
              {(Object.entries(services) as [K, string][]).map(([value, label]) => (
                <option key={value} value={value}>
                  {label}
                </option>
              ))}
            </Select>
          </Field>
          <Field label="API key" className="flex-1" hint={creditHint}>
            <div className="flex gap-2">
              <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!key}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={error} at={failedAt} what="check" />
    </Section>
  );
}
