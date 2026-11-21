"use client";

import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { ApiError, api } from "@/lib/api";

export function ResetForm({ token }: { token: string | null }) {
  const router = useRouter();
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [mismatch, setMismatch] = useState(false);
  // The server's reason when the link is unknown, used or expired.
  const [dead, setDead] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  // Out of the address bar, so the token isn't left in the history or seen by anything that reads the URL later.
  useEffect(() => {
    if (token) window.history.replaceState(null, "", window.location.pathname);
  }, [token]);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setError(null);
    setMismatch(password !== again);
    if (password !== again || !token) return;
    setLoading(true);
    try {
      await api("/auth/password/reset", { body: { token, password } });
      router.replace("/signin?reset=1");
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) setDead(err.message);
      else setError(err instanceof Error ? err.message : "Couldn't change your password.");
      setLoading(false);
    }
  };

  if (!token || dead) {
    return (
      <Card className="w-full max-w-[400px] p-8 text-center">
        <h1 className="text-xl font-medium">This link doesn&apos;t work</h1>
        <p className="mt-2 text-sm text-pretty text-muted-foreground">
          {dead ?? "It's missing part of the address. Open it from the email again, or request a new one."}
        </p>
        <LinkButton href="/forgot-password" className="mt-6 w-full">
          Request a new link
        </LinkButton>
      </Card>
    );
  }

  return (
    <Card className="w-full max-w-[400px] p-8">
      <h1 className="text-2xl font-medium tracking-tight">Choose a new password</h1>
      <p className="mt-1 text-sm text-pretty text-muted-foreground">You&apos;ll sign in with it next. Every device signed in now gets signed out.</p>
      <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
        <Field label="New password" hint="At least 8 characters." error={error}>
          <Input type="password" autoComplete="new-password" autoFocus required minLength={8} value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Field label="New password again" error={mismatch ? "The passwords don't match." : null}>
          <Input type="password" autoComplete="new-password" required minLength={8} value={again} onChange={(e) => setAgain(e.target.value)} />
        </Field>
        <Button type="submit" loading={loading} className="mt-2">
          Change password
        </Button>
      </form>
    </Card>
  );
}
