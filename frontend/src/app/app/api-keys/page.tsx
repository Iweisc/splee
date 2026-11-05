"use client";

import { useState } from "react";
import useSWR from "swr";
import { BookOpen, KeyRound, Plus, TriangleAlert } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyField } from "@/components/app/settings/copy-button";
import { LoadError, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Badge, Card, EmptyState, Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ApiKey, ProjectSummary } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

export default function ApiKeysPage() {
  const { data, error, mutate } = useSWR<ApiKey[]>("/organization/api-keys", fetcher);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const { pending, run } = useAction();
  const [open, setOpen] = useState(false);
  const docsProject = projects?.[0]?.id;

  const revoke = (k: ApiKey) => {
    if (!confirm(`Revoke "${k.name}"? Anything using it stops working immediately.`)) return;
    run(k.id, async () => {
      await api(`/organization/api-keys/${k.id}`, { method: "DELETE" });
      await mutate();
    }, "Key revoked");
  };

  const create = (
    <Button onClick={() => setOpen(true)}>
      <Plus className="size-4" /> Create key
    </Button>
  );

  return (
    <>
      <PageHeader
        title="API keys"
        description="Keys for the Splee API. A key can read and add leads in every project of this organization."
        actions={
          <>
            {docsProject && (
              <LinkButton href={`/app/p/${docsProject}/docs`} variant="outline">
                <BookOpen className="size-4" /> API docs
              </LinkButton>
            )}
            {data && data.length > 0 && create}
          </>
        }
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="API keys" />
          ) : !data ? (
            <SectionsSkeleton count={1} />
          ) : data.length === 0 ? (
            <Card>
              <EmptyState icon={<KeyRound className="size-6" />} title="No API keys" action={create}>
                Create a key to pull hot leads into your CRM or push your own leads into Splee.
              </EmptyState>
            </Card>
          ) : (
            <Card className="divide-y divide-border">
              {data.map((k) => (
                <div key={k.id} className="flex items-center gap-4 px-5 py-4 text-sm">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <p className="truncate font-medium">{k.name}</p>
                      {k.revoked_at && <Badge tone="red">Revoked</Badge>}
                    </div>
                    <p className="mt-0.5 text-xs text-muted-foreground">
                      <code className="font-mono">{k.prefix}…</code> · created {timeAgo(k.created_at)} ·{" "}
                      {k.last_used_at ? `last used ${timeAgo(k.last_used_at)}` : "never used"}
                    </p>
                  </div>
                  {!k.revoked_at && (
                    <Button variant="outline" size="sm" onClick={() => revoke(k)} loading={pending === k.id}>
                      Revoke
                    </Button>
                  )}
                </div>
              ))}
            </Card>
          )}
        </SettingsColumn>
      </PageBody>

      <Modal open={open} onClose={() => setOpen(false)} title="Create API key">
        {open && <CreateKey onCreated={() => mutate()} onDone={() => setOpen(false)} />}
      </Modal>
    </>
  );
}

function CreateKey({ onCreated, onDone }: { onCreated: () => void; onDone: () => void }) {
  const [name, setName] = useState("");
  const [secret, setSecret] = useState<string | null>(null);
  const { pending, run } = useAction();

  if (secret) {
    return (
      <div className="grid gap-4">
        <p className="flex gap-2 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
          <TriangleAlert className="mt-0.5 size-4 shrink-0" />
          Copy this key now. It is shown only once and can&apos;t be recovered. If you lose it, revoke it and create a new one.
        </p>
        <CopyField value={secret} />
        <div className="flex justify-end">
          <Button onClick={onDone}>I&apos;ve saved it</Button>
        </div>
      </div>
    );
  }

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        run("create", async () => {
          const res = await api<{ api_key: ApiKey; secret: string }>("/organization/api-keys", { body: { name: name.trim() } });
          setSecret(res.secret);
          onCreated();
        });
      }}
    >
      <Field label="Name" hint="Where you'll use it, so you know what breaks if you revoke it.">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. HubSpot sync" autoFocus />
      </Field>
      <div className="flex justify-end">
        <Button type="submit" disabled={!name.trim()} loading={pending === "create"}>
          Create key
        </Button>
      </div>
    </form>
  );
}
