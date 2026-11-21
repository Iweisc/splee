"use client";

import { useEffect, useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyButton, CopyField } from "@/components/app/settings/copy-button";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { SSO_LABELS, SsoIcon, ssoStartUrl } from "@/components/sso";
import { Button, buttonClass } from "@/components/ui/button";
import { LoadError, toastError } from "@/components/app/work/states";
import { Field, Input, Switch } from "@/components/ui/form";
import { Modal } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Me, SignInMethods, SsoProvider, TwoFactorSetup, TwoFactorStatus } from "@/lib/types";

export default function AccountPage() {
  const { me } = useMe();
  return (
    <>
      <PageHeader title="Account" description={me ? `Signed in as ${me.user.email}.` : undefined} />
      <PageBody>
        <SettingsColumn>
          <Away />
          <SignInMethodsSection />
          <ChangePassword />
          <TwoFactor />
        </SettingsColumn>
      </PageBody>
    </>
  );
}

function Away() {
  const { me, mutate } = useMe();
  if (!me) return null;
  const set = (away: boolean) =>
    void mutate(api<Me>("/auth/me", { method: "PATCH", body: { away } }), { optimisticData: { ...me, away }, revalidate: false }).catch(toastError);
  return (
    <Section
      title="Away"
      description="While you're away, round robin skips you and new leads go to the rest of the team. Leads you already own stay yours."
      actions={<Switch checked={me.away} onChange={set} label="Away" />}
    />
  );
}

function SignInMethodsSection() {
  const { data, error, mutate } = useSWR<SignInMethods>("/auth/sign-in-methods", fetcher);
  const { pending, run } = useAction();

  // Connecting comes back here with the provider it connected, or why it couldn't.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("connected");
    const failed = params.get("sso_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload or React's second dev run doesn't toast twice.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) toast.success(`${SSO_LABELS[connected as SsoProvider] ?? connected} connected`);
  }, []);

  if (error) {
    return (
      <Section title="Sign-in methods">
        <LoadError error={error} what="your sign-in methods" retry={() => mutate()} />
      </Section>
    );
  }
  if (!data || (data.providers.length === 0 && data.identities.length === 0)) return null;
  const linked = new Set(data.identities.map((i) => i.provider));
  const unlinked = data.providers.filter((p) => !linked.has(p));
  // The backend refuses it too. Disabled here so the reason shows before the click.
  const onlyWayIn = !data.password && data.identities.length === 1;
  const disconnect = (id: string, provider: SsoProvider) =>
    run(
      id,
      async () => mutate(await api<SignInMethods>(`/auth/identities/${id}`, { method: "DELETE" }), { revalidate: false }),
      `${SSO_LABELS[provider]} disconnected`,
    );

  return (
    <Section title="Sign-in methods" description="Google and Microsoft accounts that sign you in to Splee.">
      <div className="grid gap-4">
        {data.identities.map((i) => (
          <div key={i.id} className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
            <div className="flex min-w-0 flex-1 basis-48 items-center gap-3">
              <SsoIcon provider={i.provider} />
              <div className="min-w-0">
                <p className="text-sm font-medium">{SSO_LABELS[i.provider]}</p>
                {i.email && <p className="truncate text-sm text-muted-foreground">{i.email}</p>}
              </div>
            </div>
            <Button variant="outline" size="sm" disabled={onlyWayIn} loading={pending === i.id} onClick={() => void disconnect(i.id, i.provider)}>
              Disconnect
            </Button>
          </div>
        ))}
        {onlyWayIn && <p className="text-xs text-muted-foreground">To disconnect it, set a password below or connect another account first.</p>}
        {unlinked.length > 0 && (
          <div className="flex flex-wrap gap-2">
            {unlinked.map((p) => (
              <a key={p} href={ssoStartUrl(p, { from: "account" })} className={buttonClass("outline", "sm")}>
                <SsoIcon provider={p} />
                Connect {SSO_LABELS[p]}
              </a>
            ))}
          </div>
        )}
      </div>
    </Section>
  );
}

function ChangePassword() {
  // An account made with Google or Microsoft has no password until it sets one, which then needs no current password.
  const { data: methods, mutate } = useSWR<SignInMethods>("/auth/sign-in-methods", fetcher);
  const hasPassword = methods?.password ?? true;
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const { pending, run } = useAction();
  // There's no password reset by email, so a typo in the new password would lock you out. Hence the second field.
  // Held back until the second one is as long as the first, so it doesn't flash while you're still typing.
  const mismatch = repeat.length >= next.length && repeat !== next ? "The two new passwords don't match" : null;

  return (
    <Section
      title="Password"
      description={
        hasPassword
          ? "Changing it signs you out everywhere else, on other browsers and devices."
          : "You sign in with Google or Microsoft. Set a password to sign in with your email too."
      }
    >
      <form
        className="grid gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          void run(
            "password",
            async () => {
              await api("/auth/password", { body: { current_password: hasPassword ? current : undefined, new_password: next } });
              setCurrent("");
              setNext("");
              setRepeat("");
              await mutate();
            },
            hasPassword ? "Password changed" : "Password set",
          );
        }}
      >
        {hasPassword && (
          <Field label="Current password">
            <Input type="password" autoComplete="current-password" required value={current} onChange={(e) => setCurrent(e.target.value)} />
          </Field>
        )}
        <Field label="New password" hint="At least 8 characters.">
          <Input type="password" autoComplete="new-password" required minLength={8} value={next} onChange={(e) => setNext(e.target.value)} />
        </Field>
        <Field label="New password again" error={mismatch}>
          <Input type="password" autoComplete="new-password" required value={repeat} onChange={(e) => setRepeat(e.target.value)} />
        </Field>
        <div>
          <Button type="submit" disabled={(hasPassword && !current) || next.length < 8 || repeat !== next} loading={pending === "password"}>
            {hasPassword ? "Change password" : "Set password"}
          </Button>
        </div>
      </form>
    </Section>
  );
}

type Dialog = { kind: "on"; setup: TwoFactorSetup } | { kind: "off" } | { kind: "codes" };

const DIALOG_TITLES: Record<Dialog["kind"], string> = {
  on: "Turn on two-factor sign-in",
  off: "Turn off two-factor sign-in",
  codes: "New backup codes",
};

function TwoFactor() {
  const { data, error, mutate } = useSWR<TwoFactorStatus>("/auth/two-factor", fetcher);
  const [dialog, setDialog] = useState<Dialog | null>(null);
  const { pending, run } = useAction();
  const close = () => {
    setDialog(null);
    void mutate();
  };
  // Started from the click rather than when the dialog mounts, so a re-render can't make a second key behind the QR code.
  const turnOn = () =>
    run("setup", async () => setDialog({ kind: "on", setup: await api<TwoFactorSetup>("/auth/two-factor/setup", { method: "POST" }) }));
  const left = data?.backup_codes_left === 1 ? "1 backup code left" : `${data?.backup_codes_left} backup codes left`;

  return (
    <>
      <Section
        title="Two-factor sign-in"
        description={
          data?.enabled
            ? `On, with ${left}.`
            : "Signing in also asks for a code from an authenticator app, so a stolen password isn't enough."
        }
        actions={
          data &&
          (data.enabled ? (
            <>
              <Button variant="outline" onClick={() => setDialog({ kind: "codes" })}>
                New backup codes
              </Button>
              <Button variant="outline" onClick={() => setDialog({ kind: "off" })}>
                Turn off
              </Button>
            </>
          ) : (
            <Button onClick={turnOn} loading={pending === "setup"}>
              Turn on
            </Button>
          ))
        }
      >
        {error && <LoadError error={error} what="two-factor sign-in" retry={() => mutate()} />}
      </Section>
      {/* Outside the section, so its closed dialog doesn't leave a gap at the bottom of the card. */}
      <Modal open={!!dialog} onClose={close} title={dialog && DIALOG_TITLES[dialog.kind]}>
        {dialog?.kind === "on" && <TurnOn setup={dialog.setup} onDone={close} />}
        {dialog?.kind === "off" && (
          <>
            <p className="mb-4 text-sm text-muted-foreground">Signing in will only ask for your password, and your backup codes stop working.</p>
            <ProofForm
              submitLabel="Turn off"
              danger
              onProof={async (proof) => {
                await api("/auth/two-factor/disable", { body: { proof } });
                toast.success("Two-factor sign-in is off");
                close();
              }}
            />
          </>
        )}
        {dialog?.kind === "codes" && <NewBackupCodes onDone={close} />}
      </Modal>
    </>
  );
}

function TurnOn({ setup, onDone }: { setup: TwoFactorSetup; onDone: () => void }) {
  const [code, setCode] = useState("");
  const [backupCodes, setBackupCodes] = useState<string[] | null>(null);
  const { pending, run } = useAction();
  if (backupCodes) return <BackupCodeList codes={backupCodes} onDone={onDone} />;

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void run(
          "enable",
          async () => setBackupCodes((await api<{ backup_codes: string[] }>("/auth/two-factor/enable", { body: { code } })).backup_codes),
          "Two-factor sign-in is on",
        );
      }}
    >
      <p className="text-sm text-muted-foreground">Scan the QR code with your authenticator app, or type the key into it.</p>
      {/* A data URL made by the backend, so next/image has nothing to optimize. */}
      {/* eslint-disable-next-line @next/next/no-img-element */}
      <img src={setup.qr} alt="QR code for your authenticator app" className="mx-auto size-48 rounded-lg bg-white" />
      <CopyField value={setup.secret} />
      <Field label="Code from the app" hint="Turning it on needs one code, so a typo in the key can't lock you out.">
        <Input autoComplete="one-time-code" inputMode="numeric" required value={code} onChange={(e) => setCode(e.target.value)} />
      </Field>
      <div className="flex justify-end">
        <Button type="submit" disabled={!code.trim()} loading={pending === "enable"}>
          Turn on
        </Button>
      </div>
    </form>
  );
}

function NewBackupCodes({ onDone }: { onDone: () => void }) {
  const [codes, setCodes] = useState<string[] | null>(null);
  if (codes) return <BackupCodeList codes={codes} onDone={onDone} />;
  return (
    <>
      <p className="mb-4 text-sm text-muted-foreground">Your old backup codes stop working.</p>
      <ProofForm
        submitLabel="Make new codes"
        onProof={async (proof) => {
          const res = await api<{ backup_codes: string[] }>("/auth/two-factor/backup-codes", { body: { proof } });
          setCodes(res.backup_codes);
        }}
      />
    </>
  );
}

/** Turning two-factor off or replacing the backup codes takes the password or a current code. */
function ProofForm({ submitLabel, danger, onProof }: { submitLabel: string; danger?: boolean; onProof: (proof: string) => Promise<unknown> }) {
  const [proof, setProof] = useState("");
  const { pending, run } = useAction();
  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        void run("proof", () => onProof(proof));
      }}
    >
      <Field label="Your password or a code from your app">
        <Input type="password" autoComplete="current-password" autoFocus required value={proof} onChange={(e) => setProof(e.target.value)} />
      </Field>
      <div className="flex justify-end">
        <Button type="submit" variant={danger ? "danger" : "primary"} disabled={!proof} loading={pending === "proof"}>
          {submitLabel}
        </Button>
      </div>
    </form>
  );
}

function BackupCodeList({ codes, onDone }: { codes: string[]; onDone: () => void }) {
  return (
    <div className="grid gap-4">
      <p className="text-sm text-muted-foreground">
        Each code signs you in once without your phone. Keep them somewhere safe, like a password manager. They won&apos;t be shown again.
      </p>
      <ul className="grid grid-cols-2 gap-x-4 gap-y-1.5 rounded-lg bg-muted px-4 py-3 font-mono text-sm select-all">
        {codes.map((c) => (
          <li key={c}>{c}</li>
        ))}
      </ul>
      <div className="flex justify-end gap-2">
        <CopyButton value={codes.join("\n")} label="Copy all" size="md" />
        <Button onClick={onDone}>Done</Button>
      </div>
    </div>
  );
}
