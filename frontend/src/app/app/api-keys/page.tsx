"use client";

import { useState } from "react";
import useSWR from "swr";
import { BookOpen, KeyRound, Plus, Puzzle, TriangleAlert } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CodeBlock } from "@/components/app/settings/code-block";
import { CopyField } from "@/components/app/settings/copy-button";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError as LoadErrorState, MicroLabel } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Badge, Card, EmptyState, Modal, Skeleton } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ApiKey, ConnectedApp, ProjectSummary } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

export default function ApiKeysPage() {
  const { data, error, mutate } = useSWR<ApiKey[]>("/organization/api-keys", fetcher);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const { pending, run } = useAction();
  // The name the new key starts with, or null when the dialog is closed.
  const [creating, setCreating] = useState<string | null>(null);
  const docsProject = projects?.[0]?.id;

  const revoke = (k: ApiKey) => {
    if (!confirm(`Revoke "${k.name}"? Anything using it stops working immediately.`)) return;
    run(k.id, async () => {
      await api(`/organization/api-keys/${k.id}`, { method: "DELETE" });
      await mutate();
    }, "Key revoked");
  };

  const create = (
    <Button onClick={() => setCreating("")}>
      <Plus className="size-4" /> Create key
    </Button>
  );

  return (
    <>
      <PageHeader
        title="API keys"
        description="Keys for the Splee API and MCP server. A key can do what a member can in every project of this organization."
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
            <LoadError error={error} what="API keys" retry={() => mutate()} />
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
          <ConnectedApps />
          <ConnectAssistant />
          <BrowserExtension onCreateKey={() => setCreating("Browser extension")} />
        </SettingsColumn>
      </PageBody>

      <Modal open={creating !== null} onClose={() => setCreating(null)} title="Create API key">
        {creating !== null && <CreateKey initialName={creating} onCreated={() => mutate()} onDone={() => setCreating(null)} />}
      </Modal>
    </>
  );
}

function ConnectedApps() {
  const { me } = useMe();
  const { data, error, mutate } = useSWR<ConnectedApp[]>("/organization/connected-apps", fetcher);
  const { pending, run } = useAction();
  const admin = me?.role === "owner" || me?.role === "admin";

  const disconnect = (app: ConnectedApp) => {
    if (!confirm(`Disconnect ${app.name}? It loses access right away and has to be allowed again.`)) return;
    run(app.id, async () => {
      await api(`/organization/connected-apps/${app.id}`, { method: "DELETE" });
      await mutate();
    }, "App disconnected");
  };

  return (
    <Section title="Connected apps" description="Apps someone allowed to use Splee through the MCP server, like ChatGPT or Claude.ai. Each can do what a member can.">
      {error ? (
        <LoadErrorState error={error} what="connected apps" retry={() => mutate()} />
      ) : !data ? (
        <Skeleton className="h-10 w-full" />
      ) : data.length === 0 ? (
        <p className="text-sm text-muted-foreground">None yet. ChatGPT and Claude.ai show up here once someone allows them.</p>
      ) : (
        <div className="-my-3 divide-y divide-border">
          {data.map((app) => (
            <div key={app.id} className="flex items-center gap-4 py-3 text-sm">
              <div className="min-w-0 flex-1">
                <p className="truncate font-medium">{app.name}</p>
                <p className="mt-0.5 text-xs break-words text-muted-foreground">
                  {app.mine ? "You" : app.user_name || app.user_email}{" "}
                  <span className="whitespace-nowrap">connected it {timeAgo(app.created_at)} ·</span>{" "}
                  <span className="whitespace-nowrap">{app.last_used_at ? `last used ${timeAgo(app.last_used_at)}` : "never used"}</span>
                </p>
              </div>
              {(app.mine || admin) && (
                <Button variant="outline" size="sm" onClick={() => disconnect(app)} loading={pending === app.id}>
                  Disconnect
                </Button>
              )}
            </div>
          ))}
        </div>
      )}
    </Section>
  );
}

/** ChatGPT's and Claude.ai's steps were checked against their docs in November 2026. */
function ConnectAssistant() {
  const { me } = useMe();
  if (!me) return null;
  const url = `${me.public_url.replace(/\/+$/, "")}/api/mcp`;
  const claudeCode = `claude mcp add --transport http splee ${url} \\\n  --header "Authorization: Bearer <key>"`;
  const cursor = JSON.stringify({ mcpServers: { splee: { url, headers: { Authorization: "Bearer <key>" } } } }, null, 2);
  return (
    <Section
      title="Connect ChatGPT, Claude or Cursor"
      description="Prospect from a chat with Splee's MCP server. Search companies, start campaigns, find people and approve emails."
    >
      <div className="grid gap-4 text-sm">
        <div>
          <MicroLabel>Server URL</MicroLabel>
          <div className="mt-1">
            <CopyField value={url} />
          </div>
        </div>
        <p className="text-muted-foreground">
          <span className="font-medium text-foreground">ChatGPT:</span> in Plugins, select the plus button, then Add custom MCP server. Name it Splee,
          paste the server URL under Connection, choose OAuth for authentication, select I understand and want to continue, then Create as a plugin.
          Install the plugin and select Allow on the Splee page that opens. A workspace admin may have to allow custom MCP servers first.
        </p>
        <p className="text-muted-foreground">
          <span className="font-medium text-foreground">Claude.ai:</span> under Customize, Connectors, select Add custom connector and paste the server
          URL. If it asks how to sign in, keep Sign in now and Use Claude&apos;s published identity. Select Add, then Connect, and Allow on the Splee page
          that opens. On Team and Enterprise plans an Owner adds it under Organization settings, Connectors.
        </p>
        <p className="text-muted-foreground">
          Both reach Splee over the internet, so PUBLIC_URL must be a public https address. Claude Code and Cursor take an API key instead. Put one of
          your keys where it says <code className="font-mono">&lt;key&gt;</code>.
        </p>
        <CodeBlock label="Claude Code" code={claudeCode} wrap />
        <div className="grid gap-2">
          <p className="text-muted-foreground">
            For Cursor, add this to <code className="font-mono">.cursor/mcp.json</code> in a project, or to <code className="font-mono">~/.cursor/mcp.json</code>{" "}
            for all of them.
          </p>
          <CodeBlock label="Cursor" code={cursor} wrap />
        </div>
      </div>
    </Section>
  );
}

/** The extension lives in the repository's extension/ folder. Install steps were checked against Chrome, Edge and Firefox in November 2026. */
function BrowserExtension({ onCreateKey }: { onCreateKey: () => void }) {
  const { me } = useMe();
  if (!me) return null;
  return (
    <Section
      title="Browser extension"
      description="Add the person or company you're looking at as a lead, from a LinkedIn profile, a LinkedIn company page or any company's website."
      actions={
        <Button variant="outline" onClick={onCreateKey}>
          <Puzzle className="size-4" /> Create a key for the extension
        </Button>
      }
    >
      <div className="grid gap-4 text-sm">
        <p className="text-muted-foreground">
          <span className="font-medium text-foreground">Chrome or Edge:</span> open <code className="font-mono">chrome://extensions</code> or{" "}
          <code className="font-mono">edge://extensions</code>, turn on Developer mode, select Load unpacked and pick the{" "}
          <code className="font-mono">extension</code> folder of the Splee repository.
        </p>
        <p className="text-muted-foreground">
          <span className="font-medium text-foreground">Firefox:</span> open <code className="font-mono">about:debugging</code>, select This Firefox,
          then Load Temporary Add-on, and pick <code className="font-mono">extension/manifest.json</code>. Firefox removes it when it restarts.
        </p>
        <p className="text-muted-foreground">
          To hand it to someone without the repository, run <code className="font-mono">npm run -s package</code> in{" "}
          <code className="font-mono">extension/</code>. It writes <code className="font-mono">dist/splee-extension.zip</code>, which they unzip
          and load the same way.
        </p>
        <div>
          <MicroLabel>Splee address</MicroLabel>
          <div className="mt-1">
            <CopyField value={me.public_url.replace(/\/+$/, "")} />
          </div>
        </div>
        <p className="text-muted-foreground">Open the extension on any page and sign in with this address and an API key.</p>
      </div>
    </Section>
  );
}

function CreateKey({ initialName, onCreated, onDone }: { initialName: string; onCreated: () => void; onDone: () => void }) {
  const [name, setName] = useState(initialName);
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
