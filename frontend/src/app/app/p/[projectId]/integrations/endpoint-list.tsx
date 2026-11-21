"use client";

import { useState } from "react";
import { Send, Trash2 } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { isUrl } from "@/components/app/settings/is-url";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Host } from "@/components/ui/misc";
import { Field, Input } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Endpoint } from "@/lib/types";
import { LastFailure, submit } from "./shared";

/** Posts a sample alert to the URL as typed, saved or not. `compact` shows just the icon on phones, for list rows. */
function TestButton({ projectId, target, url, compact }: { projectId: string; target: "slack" | "webhook"; url: string; compact?: boolean }) {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState(false);
  const send = async () => {
    setBusy(true);
    try {
      await api(`/projects/${projectId}/integrations/test`, { body: { target, url } });
      // A test that passes on a saved URL clears its last failure.
      mutate(`/projects/${projectId}/${target === "slack" ? "chat-channels" : "webhook-endpoints"}`);
      toast.success(target === "slack" ? "Test sent. Check the channel." : "Test sent. Your endpoint accepted it.");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <Button size="sm" variant="outline" onClick={send} loading={busy} disabled={!isUrl(url)} aria-label="Send a test">
      <Send className="size-3.5" /> <span className={compact ? "max-sm:hidden" : undefined}>Send a test</span>
    </Button>
  );
}

/** URLs the project sends to, each added, tested and removed on its own, not with the page's Save. */
export function EndpointList({
  projectId,
  kind,
  name,
  added,
  confirmRemove,
  labels,
  hint,
  placeholder,
}: {
  projectId: string;
  kind: "chat-channels" | "webhook-endpoints";
  /** What to call a URL in the list and the toasts, like "Slack" or its host. */
  name: (url: string) => string;
  added: (url: string) => string;
  confirmRemove: (url: string) => string;
  labels: { first: string; next: string; input: string };
  hint?: React.ReactNode;
  placeholder: string;
}) {
  const key = `/projects/${projectId}/${kind}`;
  const { data, error, mutate } = useSWR<Endpoint[]>(key, fetcher);
  const { pending, run } = useAction();
  const [url, setUrl] = useState("");
  const add = async () => {
    const done = await run("add", async () => {
      await api(key, { body: { url } });
      await mutate();
    }, added(url));
    if (done) setUrl("");
  };
  const remove = (e: Endpoint) => {
    if (!confirm(confirmRemove(e.url))) return;
    run(e.id, async () => {
      await api(`/${kind}/${e.id}`, { method: "DELETE" });
      await mutate();
    }, `Removed ${name(e.url)}`);
  };
  // Without this a list that didn't load looked empty, with Add offered as if nothing was set up yet.
  if (error) return <LoadError error={error} what={kind === "chat-channels" ? "your chats" : "your webhooks"} retry={() => mutate()} />;
  return (
    <>
      {!!data?.length && (
        <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
          {data.map((e) => (
            <li key={e.id} className="px-3 py-2.5">
              <div className="flex items-center gap-2">
                <div className="min-w-0 flex-1">
                  <p className="text-sm font-medium">
                    <Host host={name(e.url)} />
                  </p>
                  {/* Two lines without the scheme, so two URLs on the same host still read apart on a phone. */}
                  <p className="line-clamp-2 font-mono text-xs break-all text-muted-foreground" title={e.url}>
                    {e.url.replace(/^https:\/\//, "")}
                  </p>
                </div>
                <TestButton projectId={projectId} target={kind === "chat-channels" ? "slack" : "webhook"} url={e.url} compact />
                <Button size="sm" variant="ghost" onClick={() => remove(e)} loading={pending === e.id} aria-label={`Remove ${name(e.url)}`}>
                  <Trash2 className="size-3.5" />
                </Button>
              </div>
              <LastFailure error={e.error} at={e.failed_at} />
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={submit(add)}>
        <Field label={data?.length ? labels.next : labels.first} hint={hint}>
          <div className="flex gap-2">
            {/* The label also wraps Add, so name the input itself. */}
            <Input aria-label={labels.input} type="url" value={url} onChange={(ev) => setUrl(ev.target.value.trim())} placeholder={placeholder} />
            <Button type="submit" loading={pending === "add"} disabled={!isUrl(url)}>
              Add
            </Button>
          </div>
        </Field>
      </form>
    </>
  );
}
