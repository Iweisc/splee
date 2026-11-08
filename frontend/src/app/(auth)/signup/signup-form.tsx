"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import useSWR from "swr";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { DemoState, Me } from "@/lib/types";

interface Claim {
  project_id: string;
  token: string;
}

export function SignupForm({
  next,
  claim,
  inviteToken,
  inviteEmail,
}: {
  next: string | null;
  claim: Claim | null;
  inviteToken: string | null;
  inviteEmail: string | null;
}) {
  const router = useRouter();
  const { mutate } = useMe();
  const [name, setName] = useState("");
  const [email, setEmail] = useState(inviteEmail ?? "");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const { data: demo } = useSWR<DemoState>(claim ? `/demo/${claim.project_id}${qs({ token: claim.token })}` : null, fetcher);
  const claimedName = demo?.project.domain || demo?.project.name;

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setLoading(true);
    setError(null);
    try {
      const me = await api<Me>("/auth/signup", {
        body: {
          name,
          email,
          password,
          claim: claim ?? undefined,
          invite_token: inviteToken ?? undefined,
        },
      });
      await mutate(me, { revalidate: false });
      router.replace(next ?? (claim ? `/app/p/${claim.project_id}` : "/app"));
    } catch (err) {
      setError(err instanceof Error ? err.message : "Couldn't create your account.");
      setLoading(false);
    }
  };

  return (
    <Card className="w-full max-w-[400px] p-8">
      <h1 className="text-2xl font-medium tracking-tight">{inviteToken ? "Join your team on Splee" : "Create your account"}</h1>
      {claim && (
        <p className="mt-4 rounded-lg bg-brand-50 px-3 py-2 text-sm text-brand-800">
          Your research for {claimedName ? <strong className="font-medium">{claimedName}</strong> : "this demo"} will be saved.
        </p>
      )}
      <form onSubmit={submit} className="mt-6 flex flex-col gap-4">
        <Field label="Name">
          <Input autoComplete="name" required value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <Field label="Work email">
          <Input type="email" autoComplete="email" required value={email} onChange={(e) => setEmail(e.target.value)} />
        </Field>
        <Field label="Password" error={error}>
          <Input type="password" autoComplete="new-password" required minLength={8} value={password} onChange={(e) => setPassword(e.target.value)} />
        </Field>
        <Button type="submit" loading={loading} className="mt-2">
          Create account
        </Button>
      </form>
      <p className="mt-6 text-center text-sm text-muted-foreground">
        Already have an account?{" "}
        <Link href={`/signin${qs({ next: next ?? (inviteToken ? `/invite/${inviteToken}` : null) })}`} className="font-medium text-foreground hover:underline">
          Sign in
        </Link>
      </p>
      <p className="mt-4 text-center text-xs text-subtle-foreground">
        By signing up you agree to the{" "}
        <Link href="/terms-of-use" className="underline underline-offset-2">
          terms of use
        </Link>
        .
      </p>
    </Card>
  );
}
