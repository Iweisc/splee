"use client";

import { useEffect, useState, useSyncExternalStore } from "react";
import Link from "next/link";
import useSWR from "swr";
import { toast } from "sonner";
import { Pause, Pencil, Play, Plug, Plus, RefreshCw, Server, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { DnsBadge } from "@/components/app/settings/dns-badge";
import { LoadError, SectionsSkeleton } from "@/components/app/settings/layout";
import { MailboxForm } from "@/components/app/settings/mailbox-form";
import { ProviderLogo } from "@/components/app/settings/provider-logo";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Address, Badge, Card, EmptyState, Message, Modal, type BadgeTone } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { Switch } from "@/components/ui/form";
import type { Mailbox, MailboxInput, MailboxStats, SignInProvider, WarmupHealth } from "@/lib/types";
import { SIGN_IN_LABEL, bouncing, heldForWarmup, landingInSpam, todayLimit, warmupStatus, wholePercent } from "@/lib/mailbox";
import { dateTime, isPast, num, timeAgo } from "@/lib/utils";

const STATUS: Record<Mailbox["status"], { tone: BadgeTone; label: string }> = {
  active: { tone: "green", label: "Active" },
  paused: { tone: "neutral", label: "Paused" },
  error: { tone: "red", label: "Error" },
};

type TestResult = { ok: boolean; error?: string };

/** Run right after a mailbox is added, so a problem shows now and not at the first send. */
async function testAdded(id: string) {
  const res = await api<TestResult>(`/mailboxes/${id}/test`, { method: "POST" }).catch((e: Error): TestResult => ({ ok: false, error: e.message }));
  if (res.ok) toast.success("Mailbox connected");
  else toast.warning(`Mailbox added, but the connection test failed: ${res.error || "unknown error"}`);
}

const isLoopback = (url: string) => {
  try {
    return ["localhost", "127.0.0.1", "[::1]"].includes(new URL(url).hostname);
  } catch {
    return false;
  }
};

const noSubscription = () => () => {};

/** Unsubscribe and invite links in emails use the backend's PUBLIC_URL. Left on its localhost default on a server, nobody can open them. */
function PublicUrlWarning() {
  const { me } = useMe();
  // null on the server, so the warning only renders in the browser, where the address is known.
  const here = useSyncExternalStore(noSubscription, () => window.location.origin, () => null);
  if (!me || !here || !isLoopback(me.public_url) || isLoopback(here)) return null;
  return (
    <p className="mb-4 rounded-lg bg-amber-50 px-4 py-3 text-sm text-amber-800">
      Unsubscribe and invite links in your emails go to <span className="font-medium">{me.public_url}</span>, which only works on the
      server itself. Set <code className="font-mono text-xs">PUBLIC_URL={here}</code> in the backend&apos;s environment and restart it
      before you send.
    </p>
  );
}

export default function MailboxesPage() {
  // Polled every minute like the sender's recheck, so a stopped mailbox that starts sending again shows it.
  const { data, error, mutate } = useSWR<Mailbox[]>("/mailboxes", fetcher, { refreshInterval: 60_000 });
  // A month of numbers barely moves in a minute, so this one loads with the page and isn't polled.
  const { data: stats } = useSWR<MailboxStats[]>("/mailboxes/stats", fetcher);
  const { data: warmup, mutate: mutateWarmup } = useSWR<WarmupHealth[]>("/mailboxes/warmup", fetcher, { refreshInterval: 60_000 });
  const { pending, run } = useAction();
  const [editing, setEditing] = useState<Mailbox | "new" | null>(null);
  const { me } = useMe();
  const signIn = me?.mailbox_sign_in ?? [];

  // The sign-in callback comes back here with the mailbox it saved, or why it couldn't.
  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("connected");
    const failed = params.get("mailbox_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload or React's second dev run doesn't test it twice.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) testAdded(connected).then(() => Promise.all([mutate(), mutateWarmup()]));
  }, [mutate, mutateWarmup]);

  const startSignIn = (provider: SignInProvider, email?: string) =>
    run(`signin:${provider}:${email ?? ""}`, async () => {
      const { url } = await api<{ url: string }>(`/mailboxes/oauth/${provider}`, { body: { email } });
      window.location.assign(url);
    });

  const replace = (updated: Mailbox) => mutate((list) => list?.map((m) => (m.id === updated.id ? updated : m)), { revalidate: false });

  const test = (m: Mailbox) =>
    run(`test:${m.id}`, async () => {
      const res = await api<TestResult>(`/mailboxes/${m.id}/test`, { method: "POST" });
      await mutate();
      if (!res.ok) throw new Error(res.error || "Connection failed");
    }, "SMTP and IMAP connected");

  const checkDns = (m: Mailbox) =>
    run(`dns:${m.id}`, async () => replace(await api<Mailbox>(`/mailboxes/${m.id}/check-dns`, { method: "POST" })));

  const togglePause = (m: Mailbox) =>
    run(`pause:${m.id}`, async () =>
      replace(await api<Mailbox>(`/mailboxes/${m.id}`, { method: "PATCH", body: { status: m.status === "paused" ? "active" : "paused" } })),
    );

  const patch = (key: string, m: Mailbox, body: Partial<MailboxInput>) =>
    run(`${key}:${m.id}`, async () => {
      replace(await api<Mailbox>(`/mailboxes/${m.id}`, { method: "PATCH", body }));
      await mutateWarmup();
    });

  const remove = (m: Mailbox) => {
    if (!confirm(`Remove ${m.email}? Emails already sent stay in your history.`)) return;
    run(`delete:${m.id}`, async () => {
      await api(`/mailboxes/${m.id}`, { method: "DELETE" });
      await mutate();
    }, "Mailbox removed");
  };

  const submit = async (input: MailboxInput) => {
    const ok = await run(
      "save",
      async () => {
        if (editing === "new") {
          const created = await api<Mailbox>("/mailboxes", { body: { ...input } });
          await testAdded(created.id);
          await Promise.all([mutate(), mutateWarmup()]);
        } else if (editing) {
          replace(await api<Mailbox>(`/mailboxes/${editing.id}`, { method: "PATCH", body: { ...input } }));
          await mutateWarmup();
          toast.success("Mailbox saved");
        }
      },
    );
    if (ok) setEditing(null);
  };

  // On phones the buttons wrap into rows they fill, instead of leaving one short button alone at the start of a row.
  const connect = (
    <>
      {signIn.map((p) => (
        <Button key={p} variant="outline" className="max-sm:flex-1" onClick={() => startSignIn(p)} loading={pending === `signin:${p}:`}>
          <ProviderLogo provider={p} /> Connect {SIGN_IN_LABEL[p]}
        </Button>
      ))}
      <Button className="max-sm:flex-1" onClick={() => setEditing("new")}>
        <Plus className="size-4" /> {signIn.length > 0 ? "Connect with SMTP" : "Connect mailbox"}
      </Button>
    </>
  );

  return (
    <>
      <PageHeader
        title="Mailboxes"
        description={
          <>
            Splee sends from your own mailboxes. Use a secondary domain (like try-yourbrand.com) so cold outreach can&apos;t hurt your main domain&apos;s
            reputation. Splee can{" "}
            <Link href="/app/sending-domains" className="font-medium text-foreground underline-offset-4 hover:underline">
              buy and set one up
            </Link>{" "}
            for you, and{" "}
            <Link href="/app/deliverability" className="font-medium text-foreground underline-offset-4 hover:underline">
              Deliverability
            </Link>{" "}
            shows whether any of them is landing in trouble.
          </>
        }
        actions={data && data.length > 0 && connect}
      />
      <PageBody className="mx-auto max-w-5xl">
        <PublicUrlWarning />
        {error ? (
          <LoadError error={error} what="mailboxes" retry={() => mutate()} />
        ) : !data ? (
          <SectionsSkeleton />
        ) : data.length === 0 ? (
          <Card>
            <EmptyState
              icon={<Server className="size-6" />}
              title="No mailboxes yet"
              action={<div className="flex flex-wrap justify-center gap-2">{connect}</div>}
            >
              Connect at least one mailbox to start sending. Google Workspace, Microsoft 365, Zoho and any SMTP/IMAP provider work.
            </EmptyState>
          </Card>
        ) : (
          <div className="grid gap-3">
            {data.map((m) => (
              <Card key={m.id} className="min-w-0 p-5">
                <div className="flex flex-wrap items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex items-baseline gap-2">
                      <p className="min-w-0 font-medium">
                        <Address email={m.email} />
                      </p>
                      {m.needs_reconnect ? (
                        <Badge tone="red">Signed out</Badge>
                      ) : isHeld(m) ? (
                        <Badge tone="red">Held</Badge>
                      ) : (
                        <Badge tone={STATUS[m.status].tone}>{STATUS[m.status].label}</Badge>
                      )}
                    </div>
                    <p className="mt-0.5 text-sm text-muted-foreground">
                      {m.from_name && `${m.from_name} · `}
                      {m.provider === "password" ? m.smtp_host : `${SIGN_IN_LABEL[m.provider]} sign-in`} ·{" "}
                      {todayLimit(m) < m.daily_limit ? `${todayLimit(m)} today, ramping up to ${m.daily_limit}/day` : `up to ${m.daily_limit}/day`}
                    </p>
                  </div>
                  {/* On phones the five buttons don't fit one row, so each group takes a full row of its own,
                      stretched, instead of leaving edit and remove alone on a second row. The icon buttons have
                      room for their labels there. w-auto drops their fixed icon width so a label can't squeeze
                      its icon, and the first pair wraps on the narrowest phones where it can't sit side by side. */}
                  <div className="flex flex-wrap items-center gap-1.5 max-sm:w-full">
                    <div className="flex flex-wrap gap-1.5 max-sm:basis-full max-sm:[&>*]:flex-1">
                      <Button variant="outline" size="sm" onClick={() => test(m)} loading={pending === `test:${m.id}`}>
                        <Plug className="size-4" /> Test connection
                      </Button>
                      <Button variant="outline" size="sm" onClick={() => checkDns(m)} loading={pending === `dns:${m.id}`}>
                        <RefreshCw className="size-4" /> Check DNS
                      </Button>
                    </div>
                    <div className="flex gap-1.5 max-sm:basis-full max-sm:[&>*]:w-auto max-sm:[&>*]:flex-1 max-sm:[&>*]:gap-1.5">
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => togglePause(m)}
                        loading={pending === `pause:${m.id}`}
                        aria-label={m.status === "paused" ? "Resume" : "Pause"}
                        title={m.status === "paused" ? "Resume" : "Pause"}
                      >
                        {m.status === "paused" ? <Play className="size-4" /> : <Pause className="size-4" />}
                        <span className="sm:hidden">{m.status === "paused" ? "Resume" : "Pause"}</span>
                      </Button>
                      <Button variant="ghost" size="icon" onClick={() => setEditing(m)} aria-label="Edit" title="Edit">
                        <Pencil className="size-4" />
                        <span className="sm:hidden">Edit</span>
                      </Button>
                      <Button
                        variant="ghost"
                        size="icon"
                        onClick={() => remove(m)}
                        loading={pending === `delete:${m.id}`}
                        aria-label="Remove"
                        title="Remove"
                      >
                        <Trash2 className="size-4" />
                        <span className="sm:hidden">Remove</span>
                      </Button>
                    </div>
                  </div>
                </div>

                <div className="mt-4 flex flex-wrap items-center gap-2">
                  <DnsBadge name="MX" check={m.dns?.mx} />
                  <DnsBadge name="SPF" check={m.dns?.spf} />
                  <DnsBadge name="DMARC" check={m.dns?.dmarc} />
                  <DnsBadge name="DKIM" check={m.dns?.dkim} />
                  <span className="text-xs text-muted-foreground">
                    {m.dns?.checked_at ? `Checked ${timeAgo(m.dns.checked_at)}` : "DNS not checked yet"}
                  </span>
                </div>

                <Warmup
                  m={m}
                  health={warmup?.find((h) => h.mailbox_id === m.id)}
                  pending={pending}
                  toggle={() => patch("warmup", m, { warmup: !m.warmup })}
                  hold={() => patch("hold", m, { hold_until_warm: true })}
                />

                <Health stats={stats?.find((s) => s.mailbox_id === m.id)} />

                {isHeld(m) && (
                  <div className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-pretty text-red-800">
                    <p>{m.last_error}</p>
                    <p className="mt-1">
                      <Link href="/app/deliverability" className="font-medium underline underline-offset-2">
                        See what to do on the Deliverability page
                      </Link>
                    </p>
                  </div>
                )}
                {/* A signed-out mailbox's error only says what the box below it does. A paused one keeps why it was paused. */}
                {m.last_error && !isHeld(m) && !(m.needs_reconnect && m.status === "error") && (
                  <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 font-mono text-xs wrap-anywhere text-red-700">
                    <Message text={m.last_error} />
                  </p>
                )}
                {m.provider !== "password" && m.needs_reconnect ? (
                  <div className="mt-3 flex flex-wrap items-center justify-between gap-x-4 gap-y-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700">
                    <p className="min-w-0 flex-1 basis-64 text-pretty">
                      {SIGN_IN_LABEL[m.provider]} signed Splee out of this mailbox. Sign in again as {m.email} to send and read replies.
                    </p>
                    <Button
                      size="sm"
                      variant="outline"
                      className="max-sm:grow"
                      onClick={() => startSignIn(m.provider as SignInProvider, m.email)}
                      loading={pending === `signin:${m.provider}:${m.email}`}
                    >
                      <ProviderLogo provider={m.provider} /> Reconnect
                    </Button>
                  </div>
                ) : (
                  m.status === "error" && <p className="mt-2 text-xs text-muted-foreground">{nextStep(m)}</p>
                )}
              </Card>
            ))}
          </div>
        )}
      </PageBody>

      <Modal
        open={editing !== null}
        onClose={() => setEditing(null)}
        title={editing === "new" ? "Connect mailbox" : "Edit mailbox"}
        className="max-w-2xl"
      >
        {editing !== null && (
          <MailboxForm
            key={editing === "new" ? "new" : editing.id}
            mailbox={editing === "new" ? undefined : editing}
            saving={pending === "save"}
            onSubmit={submit}
          />
        )}
      </Modal>
    </>
  );
}

/** Paused by the Deliverability check, not by someone. */
const isHeld = (m: Mailbox) => m.status === "paused" && m.health_held_at !== null;

/** What happens next to a stopped mailbox. The sender tests an unreachable server or a used-up daily limit again itself. */
function nextStep(m: Mailbox): string {
  if (!m.recheck_at) return "Fix the settings, then press Test connection to start sending again.";
  if (isPast(m.recheck_at)) return "Splee is testing it again now.";
  return `Splee tests it again at ${dateTime(m.recheck_at)} and starts sending once it works.`;
}

function Health({ stats }: { stats?: MailboxStats }) {
  if (!stats?.emailed) return null;
  const pct = (n: number) => wholePercent(n, stats.emailed);
  const risky = bouncing(stats.emailed, stats.bounced);
  return (
    <>
      {/* Each part stays whole, so a narrow phone wraps between them rather than leaving a word alone. */}
      <p className="mt-3 flex flex-wrap gap-x-1.5 text-xs text-muted-foreground [&>span]:whitespace-nowrap">
        <span>30 days: {num(stats.emailed)} emailed ·</span>
        <span>
          <span className={risky ? "font-medium text-amber-700" : undefined}>{pct(stats.bounced)} bounced</span> ·
        </span>
        <span>{pct(stats.replied)} replied</span>
      </p>
      {risky && (
        <p className="mt-2 rounded-lg bg-amber-50 px-3 py-2 text-xs text-amber-800">
          More than 1 in 20 emails from this mailbox bounced. Past that, providers start sending its mail to spam. Check the addresses in your
          lead lists before you send more. Past 1 in 10, Splee pauses it.
        </p>
      )}
    </>
  );
}

function Warmup({
  m,
  health,
  pending,
  toggle,
  hold,
}: {
  m: Mailbox;
  health?: WarmupHealth;
  pending: string | null;
  toggle: () => void;
  hold: () => void;
}) {
  const status = warmupStatus(m, health);
  const seen = health ? health.inbox + health.spam : 0;
  const spam = m.warmup && health && landingInSpam(health);
  return (
    <>
      <div className="mt-3 flex flex-wrap items-center gap-x-2 gap-y-1.5">
        <Switch checked={m.warmup} onChange={toggle} disabled={pending === `warmup:${m.id}`} label="Warmup" />
        <span className="text-sm font-medium">Warmup</span>
        {m.warmup ? <Badge tone={status.tone}>{status.label}</Badge> : <span className="text-xs text-muted-foreground">Off</span>}
        {heldForWarmup(m, health) && !spam && <span className="text-xs whitespace-nowrap text-muted-foreground">Campaigns wait until it&apos;s warm</span>}
      </div>
      {m.warmup && health && health.sent + health.received > 0 && (
        <p className="mt-2 flex flex-wrap gap-x-1.5 text-xs text-muted-foreground [&>span]:whitespace-nowrap">
          <span>14 days: {num(health.sent)} sent ·</span>
          <span>{num(health.received)} received ·</span>
          {seen > 0 && (
            <span>
              <span className={spam ? "font-medium text-amber-700" : undefined}>{wholePercent(health.inbox, seen)} inbox placement</span> ·
            </span>
          )}
          <span>
            today {health.sent_today} of {health.target_today}
          </span>
        </p>
      )}
      {m.warmup && health && !health.has_peers && !health.day && (
        <p className="mt-2 text-xs text-pretty text-muted-foreground">
          Warmup trades emails with the other mailboxes warming up on this Splee. Turn it on for a second mailbox, ideally on another domain.
        </p>
      )}
      {m.warmup && !m.imap_host && (
        <p className="mt-2 text-xs text-pretty text-muted-foreground">Add IMAP settings so this mailbox can read and answer its warmup emails.</p>
      )}
      {spam && (
        <div className="mt-2 flex flex-wrap items-center justify-between gap-x-4 gap-y-2 rounded-lg bg-amber-50 px-3 py-2 text-xs text-amber-800">
          <p className="min-w-0 flex-1 basis-64 text-pretty">
            {m.hold_until_warm
              ? "Over 1 in 5 of its warmup emails landed in spam, so campaigns from this mailbox wait until most reach the inbox again."
              : "Over 1 in 5 of its warmup emails landed in spam, and its campaign emails likely do too. Hold campaigns while warmup repairs it?"}
          </p>
          {!m.hold_until_warm && (
            <Button variant="outline" size="sm" onClick={hold} loading={pending === `hold:${m.id}`}>
              Hold campaigns
            </Button>
          )}
        </div>
      )}
    </>
  );
}
