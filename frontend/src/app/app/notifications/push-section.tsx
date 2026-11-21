"use client";

import { useEffect, useState, useSyncExternalStore } from "react";
import { BellRing } from "lucide-react";
import { toast } from "sonner";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, ApiError } from "@/lib/api";
import { keepPushFor, markPushOwner } from "@/lib/push";
import type { PushDevice } from "@/lib/types";
import { dateTime, timeAgo } from "@/lib/utils";

type Support = "loading" | "ok" | "insecure" | "ios-browser" | "ios-old" | "unsupported";

function detectSupport(): Support {
  if (!window.isSecureContext) return "insecure";
  if ("serviceWorker" in navigator && "PushManager" in window && "Notification" in window) return "ok";
  // iPads say they're Macs, but Macs have no touch screen.
  const ios = /iPhone|iPad|iPod/.test(navigator.userAgent) || (navigator.userAgent.includes("Macintosh") && navigator.maxTouchPoints > 1);
  if (!ios) return "unsupported";
  return window.matchMedia("(display-mode: standalone)").matches ? "ios-old" : "ios-browser";
}

const never = () => () => {};

const WHY: Record<Exclude<Support, "loading" | "ok">, string> = {
  "ios-browser":
    "On iPhone and iPad, Splee needs to be on your home screen first. In Safari, tap Share, then Add to Home Screen. Open Splee from there and turn push on.",
  "ios-old": "Push notifications need iOS 16.4 or newer. Update your iPhone or iPad, then turn push on here.",
  insecure: "Push notifications need Splee on an https:// address. Ask whoever runs your Splee to serve it over HTTPS.",
  unsupported: "This browser doesn't support push notifications. Try Chrome, Edge, Firefox or Safari.",
};

function fromBase64Url(text: string) {
  const raw = atob(text.replace(/-/g, "+").replace(/_/g, "/").padEnd(Math.ceil(text.length / 4) * 4, "="));
  const bytes = new Uint8Array(raw.length);
  for (let i = 0; i < raw.length; i++) bytes[i] = raw.charCodeAt(i);
  return bytes;
}

/** This browser's subscription to the server's current key. One made with an older key is replaced, since it no longer works. */
async function subscribeHere(reg: ServiceWorkerRegistration) {
  const { public_key } = await api<{ public_key: string }>("/push/key");
  const key = fromBase64Url(public_key);
  const current = await reg.pushManager.getSubscription();
  const had = current?.options.applicationServerKey;
  if (current && had && had.byteLength === key.length && new Uint8Array(had).every((b, i) => b === key[i])) return current;
  await current?.unsubscribe();
  return reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
}

async function save(sub: PushSubscription, userId: string) {
  const device = await api<PushDevice>("/push/subscriptions", { method: "POST", body: { ...sub.toJSON() } });
  markPushOwner(userId);
  return device;
}

/** The browser's permission, and which saved device this browser is. Saving it again picks up keys the browser changed. A
 * subscription another user made here is dropped, not taken over. */
async function thisBrowser(userId: string): Promise<{ permission: NotificationPermission; deviceId: string | null }> {
  const permission = Notification.permission;
  const reg = permission === "granted" ? await navigator.serviceWorker.getRegistration("/") : undefined;
  if (!reg || !(await reg.pushManager.getSubscription()) || !(await keepPushFor(userId))) return { permission, deviceId: null };
  return { permission, deviceId: (await save(await subscribeHere(reg), userId)).id };
}

/** Turn on push in this browser, send a test, and see or remove each browser it's on. */
export function PushSection() {
  const support = useSyncExternalStore(never, detectSupport, () => "loading" as const);
  const devices = useSWR<PushDevice[]>("/push/subscriptions", fetcher);
  const userId = useMe().me?.user.id;
  const [here, setHere] = useState<{ permission: NotificationPermission; deviceId: string | null } | null>(null);
  const { pending, run } = useAction();
  const refresh = devices.mutate;

  useEffect(() => {
    if (support !== "ok" || !userId) return;
    let live = true;
    thisBrowser(userId)
      .then((found) => {
        if (!live) return;
        setHere(found);
        refresh();
      })
      .catch(() => live && setHere({ permission: Notification.permission, deviceId: null }));
    return () => {
      live = false;
    };
  }, [support, refresh, userId]);

  const turnOn = () =>
    run("on", async () => {
      // First, while the click still counts as one: Safari only asks from a click.
      const permission = await Notification.requestPermission();
      if (permission !== "granted") {
        setHere({ permission, deviceId: null });
        return;
      }
      await navigator.serviceWorker.register("/sw.js", { scope: "/" });
      if (!userId) return;
      const device = await save(await subscribeHere(await navigator.serviceWorker.ready), userId);
      setHere({ permission, deviceId: device.id });
      await refresh();
      toast.success("Push notifications are on in this browser");
    });

  const remove = (device: Pick<PushDevice, "id" | "user_agent">) =>
    run(`remove:${device.id}`, async () => {
      if (device.id === here?.deviceId) {
        const reg = await navigator.serviceWorker.getRegistration("/");
        await (await reg?.pushManager.getSubscription())?.unsubscribe();
        setHere({ permission: Notification.permission, deviceId: null });
      }
      await api(`/push/subscriptions/${device.id}`, { method: "DELETE" }).catch((e) => {
        // Already gone: the push service told the server it expired.
        if (!(e instanceof ApiError && e.status === 404)) throw e;
      });
      await refresh();
    }, device.id === here?.deviceId ? "Push notifications are off in this browser" : `Removed ${device.user_agent || "the browser"}`);

  const sendTest = () =>
    run("test", async () => {
      const sent = await api<{ delivered: number; gone: number; failed: number }>("/push/test", { method: "POST" });
      await refresh();
      const missed = sent.gone + sent.failed;
      if (sent.delivered === 0) throw new Error(missed ? "None of your browsers took the test. Turn push off and on again here." : "Push isn't on anywhere yet.");
      const to = sent.delivered === 1 ? "1 browser" : `${sent.delivered} browsers`;
      toast.success(missed ? `Sent to ${to}. ${missed} didn't take it.` : `Sent to ${to}`);
    });

  const thisDevice = here?.deviceId ? { id: here.deviceId, user_agent: "" } : null;
  const blocked = support === "ok" && here?.permission === "denied";
  const note =
    support !== "loading" && support !== "ok"
      ? WHY[support]
      : blocked
        ? "Your browser blocks notifications from Splee. Allow them in this site's settings, next to the address bar, then reload this page."
        : null;

  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <BellRing className="size-4" /> Push notifications on this device
        </span>
      }
      description={
        <span className="block text-pretty">
          Hot replies, questions from leads, emails waiting for approval and your reminders, even when Splee isn&apos;t open. They follow your Hot
          leads and Daily digest settings above.
        </span>
      }
      actions={
        support === "ok" &&
        here &&
        !blocked &&
        (thisDevice ? (
          <>
            <Button variant="outline" size="sm" onClick={sendTest} loading={pending === "test"}>
              Send a test
            </Button>
            <Button variant="outline" size="sm" onClick={() => remove(thisDevice)} loading={pending === `remove:${thisDevice.id}`}>
              Turn off
            </Button>
          </>
        ) : (
          <Button onClick={turnOn} loading={pending === "on"}>
            Turn on
          </Button>
        ))
      }
    >
      {(note || devices.error || !!devices.data?.length) && (
        <div className="space-y-4">
          {note && <p className="rounded-lg bg-amber-50 px-3 py-2.5 text-sm text-pretty text-amber-800">{note}</p>}
          {devices.error ? (
            <LoadError error={devices.error} what="your devices" retry={() => refresh()} />
          ) : (
            !!devices.data?.length && (
              <ul className="divide-y divide-border rounded-lg border border-border">
                {devices.data.map((d) => (
                  <li key={d.id} className="flex items-center justify-between gap-3 px-3 py-2.5">
                    <div className="min-w-0 flex-1">
                      <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm font-medium">
                        <span className="break-words">{d.user_agent || "A browser"}</span>
                        {d.id === here?.deviceId && <Badge tone="green">This device</Badge>}
                      </p>
                      <p className="text-xs text-pretty text-muted-foreground">
                        {d.failures > 0 ? (
                          d.failures === 1 ? "The last notification didn't go through." : `The last ${d.failures} notifications didn't go through.`
                        ) : d.last_success_at ? (
                          <span title={dateTime(d.last_success_at)}>Last notified {timeAgo(d.last_success_at)}</span>
                        ) : (
                          <span title={dateTime(d.created_at)}>Added {timeAgo(d.created_at)}</span>
                        )}
                      </p>
                    </div>
                    <Button variant="ghost" size="sm" className="shrink-0" onClick={() => remove(d)} loading={pending === `remove:${d.id}`}>
                      Remove
                    </Button>
                  </li>
                ))}
              </ul>
            )
          )}
        </div>
      )}
    </Section>
  );
}
