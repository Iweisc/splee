"use client";

import useSWR from "swr";
import { CircleAlert, CircleCheck } from "lucide-react";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";

export function UnsubscribeCard({ token }: { token: string }) {
  // Unsubscribing twice is harmless, so SWR's retries and refocus refetches are fine here.
  const { data, error } = useSWR<{ ok: boolean; email: string }>(`/unsubscribe/${encodeURIComponent(token)}`, fetcher);

  return (
    <Card className="flex w-full max-w-[420px] flex-col items-center gap-3 p-8 text-center">
      {error ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">We couldn&apos;t process this link</h1>
          <p className="text-sm text-muted-foreground">It may be broken or expired. You can also reply to the email and ask the sender to stop.</p>
        </>
      ) : data ? (
        <>
          <CircleCheck className="size-6 text-brand-600" />
          <h1 className="text-xl font-medium">You&apos;re unsubscribed</h1>
          <p className="text-sm text-muted-foreground">
            {data.email ? <strong className="font-medium text-foreground">{data.email}</strong> : "This address"} won&apos;t get any more emails
            from this sender.
          </p>
        </>
      ) : (
        <>
          <Spinner />
          <p className="text-sm text-muted-foreground">Unsubscribing…</p>
        </>
      )}
    </Card>
  );
}
