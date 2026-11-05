"use client";

import { useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { Pause, Pencil, Play, Plug, Plus, RefreshCw, Server, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, SectionsSkeleton } from "@/components/app/settings/layout";
import { MailboxForm } from "@/components/app/settings/mailbox-form";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Badge, Card, EmptyState, Modal, type BadgeTone } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { DnsCheck, Mailbox, MailboxInput } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

const STATUS: Record<Mailbox["status"], { tone: BadgeTone; label: string }> = {
  active: { tone: "green", label: "Active" },
  paused: { tone: "neutral", label: "Paused" },
  error: { tone: "red", label: "Error" },
};

type TestResult = { ok: boolean; error?: string };

export default function MailboxesPage() {
  const { data, error, mutate } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const { pending, run } = useAction();
  const [editing, setEditing] = useState<Mailbox | "new" | null>(null);

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
          // Test right away so a wrong password shows up now, not at the first send.
          const res = await api<TestResult>(`/mailboxes/${created.id}/test`, { method: "POST" }).catch(
            (e: Error): TestResult => ({ ok: false, error: e.message }),
          );
          await mutate();
          if (res.ok) toast.success("Mailbox connected");
          else toast.warning(`Mailbox added, but the connection test failed: ${res.error || "unknown error"}`);
        } else if (editing) {
          replace(await api<Mailbox>(`/mailboxes/${editing.id}`, { method: "PATCH", body: { ...input } }));
          toast.success("Mailbox saved");
        }
      },
    );
    if (ok) setEditing(null);
  };

  const connect = (
    <Button onClick={() => setEditing("new")}>
      <Plus className="size-4" /> Connect mailbox
    </Button>
  );

  return (
    <>
      <PageHeader
        title="Mailboxes"
        description="Splee sends from your own mailboxes. Use a secondary domain (like try-yourbrand.com) so cold outreach can't hurt your main domain's reputation."
        actions={data && data.length > 0 && connect}
      />
      <PageBody className="mx-auto max-w-5xl">
        {error ? (
          <LoadError error={error} what="mailboxes" />
        ) : !data ? (
          <SectionsSkeleton />
        ) : data.length === 0 ? (
          <Card>
            <EmptyState icon={<Server className="size-6" />} title="No mailboxes yet" action={connect}>
              Connect at least one mailbox to start sending. Google Workspace, Microsoft 365, Zoho and any SMTP/IMAP provider work.
            </EmptyState>
          </Card>
        ) : (
          <div className="grid gap-3">
            {data.map((m) => (
              <Card key={m.id} className="p-5">
                <div className="flex flex-wrap items-start justify-between gap-4">
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <p className="truncate font-medium">{m.email}</p>
                      <Badge tone={STATUS[m.status].tone}>{STATUS[m.status].label}</Badge>
                    </div>
                    <p className="mt-0.5 text-sm text-muted-foreground">
                      {m.from_name && `${m.from_name} · `}
                      {m.smtp_host} · up to {m.daily_limit}/day
                    </p>
                  </div>
                  <div className="flex flex-wrap items-center gap-1.5">
                    <Button variant="outline" size="sm" onClick={() => test(m)} loading={pending === `test:${m.id}`}>
                      {pending !== `test:${m.id}` && <Plug className="size-4" />} Test connection
                    </Button>
                    <Button variant="outline" size="sm" onClick={() => checkDns(m)} loading={pending === `dns:${m.id}`}>
                      {pending !== `dns:${m.id}` && <RefreshCw className="size-4" />} Check DNS
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => togglePause(m)}
                      loading={pending === `pause:${m.id}`}
                      aria-label={m.status === "paused" ? "Resume" : "Pause"}
                      title={m.status === "paused" ? "Resume" : "Pause"}
                    >
                      {pending !== `pause:${m.id}` && (m.status === "paused" ? <Play className="size-4" /> : <Pause className="size-4" />)}
                    </Button>
                    <Button variant="ghost" size="icon" onClick={() => setEditing(m)} aria-label="Edit" title="Edit">
                      <Pencil className="size-4" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      onClick={() => remove(m)}
                      loading={pending === `delete:${m.id}`}
                      aria-label="Remove"
                      title="Remove"
                    >
                      {pending !== `delete:${m.id}` && <Trash2 className="size-4" />}
                    </Button>
                  </div>
                </div>

                <div className="mt-4 flex flex-wrap items-center gap-2">
                  <DnsBadge name="MX" check={m.dns?.mx} />
                  <DnsBadge name="SPF" check={m.dns?.spf} />
                  <DnsBadge name="DMARC" check={m.dns?.dmarc} />
                  <span className="text-xs text-muted-foreground">
                    {m.dns?.checked_at ? `Checked ${timeAgo(m.dns.checked_at)}` : "DNS not checked yet"}
                  </span>
                </div>

                {m.last_error && (
                  <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 font-mono text-xs break-words text-red-700">{m.last_error}</p>
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

function DnsBadge({ name, check }: { name: string; check?: DnsCheck }) {
  const tone: BadgeTone = !check ? "neutral" : check.ok ? "green" : "amber";
  return (
    <Badge tone={tone} title={check?.detail || check?.record || undefined} className="font-mono">
      {name} {!check ? "·" : check.ok ? "✓" : "✗"}
    </Badge>
  );
}
