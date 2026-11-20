"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { useMe } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { SignIn } from "@/lib/types";

export function SigninForm({ next }: { next: string | null }) {
  const router = useRouter();
  const { mutate } = useMe();
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  // Set once the password is right for an account with two-factor on. The form then asks for the code.
  const [token, setToken] = useState<string | null>(null);
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);
    try {
      const res = token
        ? await api<SignIn>("/auth/login/code", { body: { two_factor_token: token, code } })
        : await api<SignIn>("/auth/login", { body: { email, password } });
      if ("two_factor_token" in res) {
        setToken(res.two_factor_token);
        setLoading(false);
        return;
      }
      await mutate(res, { revalidate: false });
      router.replace(next ?? "/app");
    } catch (err) {
      setError(err instanceof Error ? err.message : "Couldn't sign in.");
      setLoading(false);
    }
  };

  const startOver = () => {
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
      <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
        <Field label="Email">
          <Input type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" error={error}>
          <Input type="password" autoComplete="current-password" required value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Button type="submit" loading={loading} className="mt-2">
          Sign in
        </Button>
      </form>
      <p className="mt-4 text-center text-xs text-muted-foreground">
        Forgot your password? <span className="whitespace-nowrap">Ask whoever hosts Splee to reset it.</span>
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
