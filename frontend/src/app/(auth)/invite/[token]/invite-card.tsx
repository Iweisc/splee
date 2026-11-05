"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";
import useSWR from "swr";
import { Button, LinkButton } from "@/components/ui/button";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { Me } from "@/lib/types";

interface InvitePreview {
  org_name: string;
  email: string;
  role: string;
}

export function InviteCard({ token }: { token: string }) {
  const router = useRouter();
  const { me, mutate, isLoading: meLoading } = useMe();
  const { data: invite, error } = useSWR<InvitePreview>(`/invites/${encodeURIComponent(token)}`, fetcher);
  const [accepting, setAccepting] = useState(false);
  const [acceptError, setAcceptError] = useState<string | null>(null);

  const accept = async () => {
    setAccepting(true);
    setAcceptError(null);
    try {
      const next = await api<Me>(`/invites/${encodeURIComponent(token)}/accept`, { method: "POST" });
      await mutate(next, { revalidate: false });
      router.replace("/app");
    } catch (e) {
      setAcceptError(e instanceof Error ? e.message : "Couldn't accept the invite.");
      setAccepting(false);
    }
  };

  if (error) {
    return (
      <Card className="w-full max-w-[400px] p-8 text-center">
        <h1 className="text-xl font-medium">This invite isn&apos;t valid</h1>
        <p className="mt-2 text-sm text-muted-foreground">It may have been used already or revoked. Ask your teammate for a new link.</p>
      </Card>
    );
  }

  if (!invite || meLoading) {
    return (
      <Card className="flex w-full max-w-[400px] justify-center p-12">
        <Spinner />
      </Card>
    );
  }

  return (
    <Card className="w-full max-w-[400px] p-8">
      <h1 className="text-2xl font-medium tracking-tight">Join {invite.org_name}</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        {invite.email} was invited as {invite.role === "admin" ? "an admin" : "a member"}.
      </p>
      {me ? (
        <>
          <p className="mt-6 text-sm">
            You&apos;re signed in as <strong className="font-medium">{me.user.email}</strong>.
          </p>
          {acceptError && <p className="mt-2 text-sm text-danger">{acceptError}</p>}
          <Button onClick={accept} loading={accepting} className="mt-4 w-full">
            Accept invite
          </Button>
        </>
      ) : (
        <div className="mt-6 flex flex-col gap-2">
          <LinkButton href={`/signup${qs({ invite: token, email: invite.email })}`}>Create an account</LinkButton>
          <LinkButton href={`/signin${qs({ next: `/invite/${token}` })}`} variant="outline">
            I already have an account
          </LinkButton>
        </div>
      )}
    </Card>
  );
}
