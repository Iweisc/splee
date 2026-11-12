"use client";

import { useState } from "react";
import useSWR from "swr";
import { CircleAlert, CircleCheck } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import { type LanguageCode, RTL, UNSUBSCRIBE_TEXT } from "@/lib/languages";

// Opening this page doesn't unsubscribe anyone. Mail security scanners open links in a real browser, so only the button does.
// It's in the language of the last email they got. The error states stay English, since a bad link can't say which that was.
export function UnsubscribeCard({ token }: { token: string }) {
  const path = `/unsubscribe/${encodeURIComponent(token)}`;
  const { data, error, mutate } = useSWR<{ email: string | null; unsubscribed: boolean; language: LanguageCode }>(path, fetcher);
  const [busy, setBusy] = useState(false);
  const [failed, setFailed] = useState(false);

  const confirm = async () => {
    setBusy(true);
    setFailed(false);
    try {
      await api(path, { method: "POST" });
      await mutate({ email: data?.email ?? null, unsubscribed: true, language: data?.language ?? "en" }, { revalidate: false });
    } catch {
      setFailed(true);
    } finally {
      setBusy(false);
    }
  };

  const text = UNSUBSCRIBE_TEXT[data?.language ?? "en"] ?? UNSUBSCRIBE_TEXT.en;
  // A long address is wider than the card on phones. It breaks after the @ first, and anywhere only if a part is still too long.
  const [user, domain] = data?.email?.split(/@(.*)/) ?? [];
  const address = data?.email ? (
    <strong className="font-medium text-foreground">
      {user}
      {domain && (
        <>
          @<wbr />
          {domain}
        </>
      )}
    </strong>
  ) : (
    text.thisAddress
  );
  // The address goes where the language puts it in the sentence.
  const [before, after] = text.willStop.split("{address}");
  const willStop = (
    <p className="text-sm text-muted-foreground">
      {before}
      {address}
      {after}
    </p>
  );

  return (
    // Balanced lines, so a heading that just misses one line on a phone doesn't leave a word alone on the second.
    <Card
      className="flex w-full max-w-[420px] flex-col items-center gap-3 p-8 text-center text-balance"
      lang={data?.language}
      dir={data && RTL.includes(data.language) ? "rtl" : undefined}
    >
      {/* Only a 404 means the link is bad. When Splee can't be reached, the same link works once it can. */}
      {error instanceof ApiError && error.status === 404 ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">We couldn&apos;t process this link</h1>
          <p className="text-sm text-muted-foreground">It may be broken or expired. You can also reply to the email and ask the sender to stop.</p>
        </>
      ) : error ? (
        <>
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">We couldn&apos;t load this page</h1>
          <p className="text-sm text-muted-foreground">Try again in a moment, or reply to the email and ask the sender to stop.</p>
          <Button variant="outline" className="mt-2 w-full" onClick={() => mutate()}>
            Try again
          </Button>
        </>
      ) : !data ? (
        <Spinner />
      ) : data.unsubscribed ? (
        <>
          <CircleCheck className="size-6 text-brand-600" />
          <h1 className="text-xl font-medium">{text.done}</h1>
          {willStop}
        </>
      ) : (
        <>
          <h1 className="text-xl font-medium">{text.title}</h1>
          {willStop}
          <Button className="mt-2 w-full" onClick={confirm} loading={busy}>
            {text.button}
          </Button>
          {failed && <p className="text-sm text-danger">{text.failed}</p>}
        </>
      )}
    </Card>
  );
}
