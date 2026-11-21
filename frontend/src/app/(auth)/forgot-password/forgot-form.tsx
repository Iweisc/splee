"use client";

import Link from "next/link";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Address, Card } from "@/components/ui/misc";
import { api } from "@/lib/api";

export function ForgotForm() {
  const [email, setEmail] = useState("");
  const [sent, setSent] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);
    try {
      await api("/auth/password/forgot", { body: { email } });
      setSent(true);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Couldn't send the link.");
    }
    setLoading(false);
  };

  const backToSignIn = (
    <p className="mt-6 text-center text-sm text-muted-foreground">
      <Link href="/signin" className="font-medium text-foreground hover:underline">
        Back to sign in
      </Link>
    </p>
  );

  // The same words whether or not the account exists, since the server answers the same either way.
  if (sent) {
    return (
      <Card className="w-full max-w-[400px] p-8">
        <h1 className="text-2xl font-medium tracking-tight">Check your email</h1>
        <p className="mt-2 text-sm text-pretty text-muted-foreground">
          If an account uses <strong className="font-medium text-foreground">
            <Address email={email.trim()} />
          </strong>{" "}
          and its team has a connected mailbox, a link to reset the password is on its way.
        </p>
        <p className="mt-3 text-sm text-pretty text-muted-foreground">If nothing arrives in a few minutes, ask whoever hosts Splee to reset it.</p>
        {backToSignIn}
      </Card>
    );
  }

  return (
    <Card className="w-full max-w-[400px] p-8">
      <h1 className="text-2xl font-medium tracking-tight">Reset your password</h1>
      <p className="mt-1 text-sm text-pretty text-muted-foreground">We&apos;ll email you a link to choose a new one.</p>
      <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
        <Field label="Email" error={error}>
          <Input type="email" autoComplete="email" autoFocus required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Button type="submit" loading={loading} className="mt-2">
          Send link
        </Button>
      </form>
      {backToSignIn}
    </Card>
  );
}
