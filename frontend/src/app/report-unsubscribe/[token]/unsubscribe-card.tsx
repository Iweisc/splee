"use client";

import { useState } from "react";
import useSWR from "swr";
import { CircleAlert, CircleCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";

type Status = { email: string; title: string; unsubscribed: boolean };

// Like the lead unsubscribe page, opening this changes nothing, since mail scanners open links. Only the button does.
export function ReportUnsubscribeCard({ token }: { token: string }) {
  const path = `/report-unsubscribe/${encodeURIComponent(token)}`;
  const { data, error, mutate } = useSWR<Status>(path, fetcher);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

  const confirm = async () => {
    setBusy(true);
    setFailed(false);
    try {
      await mutate(await api<Status>(path, { method: "POST" }), { revalidate: false });
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  const who = data && <strong className="font-medium break-all text-foreground">{data.email}</strong>;
  return (
    <Card className="flex w-full max-w-[420px] flex-col items-center gap-3 p-8 text-center text-balance">
      {error instanceof ApiError && error.status === 404 ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This link doesn&apos;t work</h1>
          <p className="text-sm text-muted-foreground">Reply to the report email and ask the sender to stop it.</p>
        </>
      ) : error ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">We couldn&apos;t load this page</h1>
          <p className="text-sm text-muted-foreground">Try again in a moment.</p>
          <Button variant="outline" className="mt-2 w-full" onClick={() => mutate()}>
            Try again
          </Button>
        </>
      ) : !data ? (
        <Spinner />
      ) : data.unsubscribed ? (
        <>
          <CircleCheck className="size-6 text-brand-600" />
          <h1 className="text-xl font-medium">You&apos;re unsubscribed</h1>
          <p className="text-sm text-muted-foreground">
            {who} won&apos;t get {data.title} by email anymore. The link to the report still works.
          </p>
        </>
      ) : (
        <>
          <h1 className="text-xl font-medium">Stop this report?</h1>
          <p className="text-sm text-muted-foreground">
            {who} won&apos;t get {data.title} by email anymore. Other emails from the sender keep coming.
          </p>
          <Button className="mt-2 w-full" onClick={confirm} loading={busy}>
            Unsubscribe
          </Button>
          {failed && <p className="text-sm text-danger">That didn&apos;t go through. Try again.</p>}
        </>
      )}
    </Card>
  );
}
