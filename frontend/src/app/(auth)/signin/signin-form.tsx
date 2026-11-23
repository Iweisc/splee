"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useEffect, useEffectEvent, useRef, useState } from "react";
import { KeyRound } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { SsoButtons } from "@/components/sso";
import { useBrowserValue } from "@/hooks/use-browser-value";
import { useMe } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import { getPasskey, passkeyAutofill, passkeyCancelled, passkeysSupported, type PasskeyRequestOptions } from "@/lib/passkeys";
import type { SignIn } from "@/lib/types";

// Signing in with Google or Microsoft comes back with #two_factor_token=… when two-factor is on. A fragment never reaches
// a server or a Referer header, unlike a query.
const tokenInHash = () => new URLSearchParams(window.location.hash.slice(1)).get("two_factor_token");

export function SigninForm({ next, ssoError, passwordReset }: { next: string | null; ssoError: string | null; passwordReset: boolean }) {
  const router = useRouter();
  const { mutate } = useMe();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  // Set once the password is right for an account with two-factor on. The form then asks for the code.
  const [typedToken, setToken] = useState<string | null>(null);
  const [startedOver, setStartedOver] = useState(false);
  const hashToken = useBrowserValue(tokenInHash, null);
  const token = typedToken ?? (startedOver ? null : hashToken);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const passkeys = useBrowserValue(passkeysSupported, false);
  const [passkeyError, setPasskeyError] = useState<string | null>(null);
  const [passkeyLoading, setPasskeyLoading] = useState(false);
  // The passkey request behind the email field's autofill, which has to stop before the button can ask for one. Bumping
  // the round offers it again.
  const autofill = useRef<AbortController | null>(null);
  const [autofillRound, setAutofillRound] = useState(0);

  /** False when two-factor still wants its code, which the form then asks for. */
  const finish = async (res: SignIn) => {
    if ("two_factor_token" in res) {
      setToken(res.two_factor_token);
      return false;
    }
    await mutate(res, { revalidate: false });
    router.replace(next ?? "/app");
    return true;
  };

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);
    setPasskeyError(null);
    try {
      const res = token
        ? await api<SignIn>("/auth/login/code", { body: { two_factor_token: token, code } })
        : await api<SignIn>("/auth/login", { body: { email, password } });
      if (!(await finish(res))) setLoading(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Couldn't sign in.");
      setLoading(false);
    }
  };

  /** Without `autofillSignal`, the browser opens its own prompt to pick a passkey. True once it's signed in. */
  const signInWithPasskey = async (autofillSignal?: AbortSignal) => {
    try {
      const options = await api<PasskeyRequestOptions>("/auth/passkeys/sign-in/options", { method: "POST", signal: autofillSignal });
      const body = await getPasskey(options, autofillSignal);
      setPasskeyError(null);
      setError(null);
      setPasskeyLoading(true);
      if (await finish(await api<SignIn>("/auth/passkeys/sign-in", { body }))) return true;
    } catch (err) {
      if (!autofillSignal?.aborted && !passkeyCancelled(err)) {
        const fromBrowser = err instanceof DOMException || !(err instanceof Error);
        setPasskeyError(fromBrowser ? "Your browser couldn't use a passkey here. Sign in with your password." : err.message);
      }
    }
    setPasskeyLoading(false);
    return false;
  };

  // A failed or cancelled pick ends the browser's request, so another is offered. Only after one that waited for the person,
  // or an error on every try would loop.
  const offerInAutofill = useEffectEvent((signal: AbortSignal) => {
    const started = Date.now();
    void passkeyAutofill()
      .then((available) => (available && !signal.aborted ? signInWithPasskey(signal) : null))
      .then((signedIn) => {
        if (signedIn === false && !signal.aborted && Date.now() - started > 2000) setAutofillRound((n) => n + 1);
      });
  });
  useEffect(() => {
    if (token) return;
    const controller = new AbortController();
    autofill.current = controller;
    offerInAutofill(controller.signal);
    // The request's challenge lasts 5 minutes, so a fresh one replaces it before then.
    const refresh = setTimeout(() => setAutofillRound((n) => n + 1), 4.5 * 60_000);
    return () => {
      controller.abort();
      clearTimeout(refresh);
    };
  }, [token, autofillRound]);

  const passkeyButton = async () => {
    autofill.current?.abort();
    if (!(await signInWithPasskey())) setAutofillRound((n) => n + 1);
  };

  const startOver = () => {
    window.history.replaceState(null, "", window.location.pathname + window.location.search);
    setStartedOver(true);
    setToken(null);
    setCode("");
    setPassword("");
    setError(null);
  };

  if (token) {
    return (
      <Card className="w-full max-w-[400px] p-8">
        <h1 className="text-2xl font-medium tracking-tight">Enter your code</h1>
        <p className="mt-1 text-sm text-muted-foreground">From your authenticator app.</p>
        <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
          <Field label="Code" hint="Lost your phone? Use one of your backup codes." error={error}>
            <Input autoComplete="one-time-code" autoFocus required spellCheck={false} value={code} onChange={(e) => setCode(e.target.value)} />
          </Field>
          <Button type="submit" loading={loading} className="mt-2">
            Sign in
          </Button>
        </form>
        <p className="mt-4 text-center text-sm text-muted-foreground">
          <button type="button" onClick={startOver} className="font-medium text-foreground hover:underline">
            Start over
          </button>
        </p>
      </Card>
    );
  }

  return (
    <Card className="w-full max-w-[400px] p-8">
      <h1 className="text-2xl font-medium tracking-tight">Sign in</h1>
      <p className="mt-1 text-sm text-muted-foreground">Welcome back.</p>
      {passwordReset && (
        <p className="mt-4 rounded-lg bg-brand-50 px-3 py-2 text-sm text-pretty text-brand-800">Password changed. Sign in with your new password.</p>
      )}
      {ssoError && <p className="mt-4 rounded-lg bg-red-50 px-3 py-2 text-sm text-danger">{ssoError}</p>}
      {passkeyError && <p className="mt-4 rounded-lg bg-red-50 px-3 py-2 text-sm text-pretty text-danger">{passkeyError}</p>}
      <SsoButtons params={{ next }} />
      <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
        <Field label="Email">
          {/* "webauthn" lets browsers offer passkeys among the suggestions, see the autofill effect above. */}
          <Input type="email" autoComplete="username webauthn" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" error={error}>
          <Input type="password" autoComplete="current-password" required value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Button type="submit" loading={loading} className="mt-2">
          Sign in
        </Button>
        {passkeys && (
          <Button type="button" variant="outline" loading={passkeyLoading} onClick={() => void passkeyButton()}>
            <KeyRound className="size-4" aria-hidden />
            Sign in with a passkey
          </Button>
        )}
      </form>
      <p className="mt-4 text-center text-sm">
        <Link href="/forgot-password" className="text-muted-foreground hover:text-foreground hover:underline">
          Forgot your password?
        </Link>
      </p>
      <p className="mt-4 text-center text-sm text-muted-foreground">
        New to Splee?{" "}
        <Link href={`/signup${qs({ next })}`} className="font-medium whitespace-nowrap text-foreground hover:underline">
          Create an account
        </Link>
      </p>
    </Card>
  );
}
