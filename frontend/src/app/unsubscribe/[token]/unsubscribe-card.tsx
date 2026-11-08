"use client";

import { useState } from "react";
import useSWR from "swr";
import { CircleAlert, CircleCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";

// Opening this page doesn't unsubscribe anyone. Mail security scanners open links in a real browser, so only the button does.
export function UnsubscribeCard({ token }: { token: string }) {
  const path = `/unsubscribe/${encodeURIComponent(token)}`;
  const { data, error, mutate } = useSWR<{ email: string | null; unsubscribed: boolean }>(path, fetcher);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

  const confirm = async () => {
    setBusy(true);
    setFailed(false);
    try {
      await api(path, { method: "POST" });
      await mutate({ email: data?.email ?? null, unsubscribed: true }, { revalidate: false });
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  const address = data?.email ? <strong className="font-medium text-foreground">{data.email}</strong> : "This address";

  return (
    <Card className="flex w-full max-w-[420px] flex-col items-center gap-3 p-8 text-center">
      {error ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">We couldn&apos;t process this link</h1>
          <p className="text-sm text-muted-foreground">It may be broken or expired. You can also reply to the email and ask the sender to stop.</p>
        </>
      ) : !data ? (
        <Spinner />
      ) : data.unsubscribed ? (
        <>
          <CircleCheck className="size-6 text-brand-600" />
          <h1 className="text-xl font-medium">You&apos;re unsubscribed</h1>
          <p className="text-sm text-muted-foreground">{address} won&apos;t get any more emails from this sender.</p>
        </>
      ) : (
        <>
          <h1 className="text-xl font-medium">Unsubscribe?</h1>
          <p className="text-sm text-muted-foreground">{address} won&apos;t get any more emails from this sender.</p>
          <Button className="mt-2 w-full" onClick={confirm} loading={busy}>
            Unsubscribe
          </Button>
          {failed && <p className="text-sm text-danger">That didn&apos;t go through. Try again, or reply to the email and ask the sender to stop.</p>}
        </>
      )}
    </Card>
  );
}
