"use client";

import { useState } from "react";
import { Newspaper, NotebookPen, PenLine, Trash2 } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Badge, Host, Tabs } from "@/components/ui/misc";
import { Field, Input, Select } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Blog, BlogTest, ProjectSettings } from "@/lib/types";
import { LastFailure, useConnection, submit } from "./shared";

/** The API key is write-only. Connecting checks it with DEV and reads the username back. */
export function DevSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [key, setKey] = useState("");
  const { busy, call } = useConnection(projectId, "dev");
  const connect = async () => {
    if (await call({ key }, "Connected to DEV")) setKey("");
  };
  const disconnect = () => {
    if (confirm("Disconnect DEV? Approved articles will wait until you connect it again.")) call(undefined, "DEV disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Newspaper className="size-4" /> DEV
        </span>
      }
      description="Publishes the articles you approve on the Social page from your dev.to account. DEV can't take comments from apps, so it gets articles only."
      actions={
        settings.dev_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.dev_connected ? (
        <p className="text-sm text-pretty">Publishing as @{settings.dev_username}, up to 2 articles a day. Approved articles go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://dev.to/settings/extensions" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              DEV&apos;s extension settings
            </a>
            , generate an API key.
          </p>
          <Field label="API key">
            <div className="flex gap-2">
              <Input aria-label="API key" type="password" autoComplete="off" value={key} onChange={(e) => setKey(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!key}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.dev_error} at={settings.dev_failed_at} what="post" />
    </Section>
  );
}

const BLOG_KINDS = { wordpress: "WordPress", ghost: "Ghost", webflow: "Webflow" } as const;

type BlogKind = keyof typeof BLOG_KINDS;

const WEEKLY_CAPS = [1, 2, 3, 4, 5, 6, 7];

/** What a blog does with an approved article. */
function PublishSelect({ kind, publish, onChange, disabled }: { kind: BlogKind; publish: boolean; onChange: (publish: boolean) => void; disabled?: boolean }) {
  return (
    <Select value={publish ? "publish" : "draft"} disabled={disabled} onChange={(e) => onChange(e.target.value === "publish")} aria-label="When approved">
      <option value="draft">Save as a draft in {BLOG_KINDS[kind]}</option>
      <option value="publish">Publish it</option>
    </Select>
  );
}

function CapSelect({ cap, onChange, disabled }: { cap: number; onChange: (cap: number) => void; disabled?: boolean }) {
  return (
    <Select value={cap} disabled={disabled} onChange={(e) => onChange(Number(e.target.value))} aria-label="Articles a week">
      {WEEKLY_CAPS.map((n) => (
        <option key={n} value={n}>
          {n === 1 ? "1 article a week" : `${n} articles a week`}
        </option>
      ))}
    </Select>
  );
}

/** A connected blog's settings, saved as they change. A paused blog says why and offers to connect it again. */
function BlogRow({ blog: b, onChange, onReconnect }: { blog: Blog; onChange: () => Promise<unknown>; onReconnect: (b: Blog) => void }) {
  const { pending, run } = useAction();
  const save = (body: { publish?: boolean; weekly_cap?: number }) =>
    run("save", async () => {
      await api(`/blogs/${b.id}`, { method: "PATCH", body });
      await onChange();
    }, "Saved");
  const remove = () => {
    if (!confirm(`Remove ${b.name}? Approved articles for it go back to drafts.`)) return;
    run("remove", async () => {
      await api(`/blogs/${b.id}`, { method: "DELETE" });
      await onChange();
    }, `Removed ${b.name}`);
  };
  const where =
    b.kind === "wordpress" ? `Writing as ${b.username}.` : b.kind === "webflow" ? `Articles go in the ${b.collection_name} collection.` : "Writing with the Admin API key.";
  return (
    <li className="flex flex-col gap-3 px-3 py-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm font-medium">
            {b.name} <Badge>{BLOG_KINDS[b.kind]}</Badge>
          </p>
          <p className="text-xs text-pretty text-muted-foreground">
            <Host host={b.url.replace(/^https?:\/\//, "")} /> · {where}
          </p>
        </div>
        <Button size="sm" variant="ghost" onClick={remove} loading={pending === "remove"} aria-label={`Remove ${b.name}`}>
          <Trash2 className="size-3.5" />
        </Button>
      </div>
      {b.paused ? (
        <div className="flex flex-col gap-2 rounded-lg bg-red-50 px-3 py-2 text-xs text-red-700 sm:flex-row sm:items-center">
          <p className="flex-1 text-pretty">
            <span className="font-medium">Paused. </span>
            {b.error ?? `${BLOG_KINDS[b.kind]} turned Splee down.`}
          </p>
          <Button size="sm" variant="outline" onClick={() => onReconnect(b)} className="self-start">
            Connect again
          </Button>
        </div>
      ) : (
        <div className="grid gap-2 sm:grid-cols-2">
          <PublishSelect kind={b.kind} publish={b.publish} onChange={(publish) => save({ publish })} disabled={pending === "save"} />
          <CapSelect cap={b.weekly_cap} onChange={(weekly_cap) => save({ weekly_cap })} disabled={pending === "save"} />
        </div>
      )}
      {!b.paused && <LastFailure error={b.error} at={b.failed_at} what="post" />}
    </li>
  );
}

const BLOG_STEPS: Record<BlogKind, string> = {
  wordpress: "In WordPress, open Users, then Profile, and add an application password near the bottom. Self-hosted sites and WordPress.com Business plans have it.",
  ghost: "In Ghost's admin, open Settings, then Integrations, add a custom integration and copy its Admin API key.",
  webflow: "In your Webflow site's settings, open Apps & integrations and generate an API token that can read sites and read and write the CMS.",
};

type Fields = { title_field: string; body_field: string; tags_field: string; excerpt_field: string };

type Collection = BlogTest["sites"][number]["collections"][number];

/** The fields a collection's articles most likely go in: its name field for the title, its first rich text field for the
 *  article, and plain text fields named like tags and a summary. */
function guessFields(c: Collection | undefined): Fields {
  const plain = (c?.fields ?? []).filter((f) => f.type === "PlainText");
  const named = (...words: string[]) => plain.find((f) => words.some((w) => f.slug.includes(w)))?.slug ?? "";
  return {
    title_field: plain.some((f) => f.slug === "name") ? "name" : (plain[0]?.slug ?? ""),
    body_field: c?.fields.find((f) => f.type === "RichText")?.slug ?? "",
    tags_field: named("tag"),
    excerpt_field: named("summary", "excerpt", "description"),
  };
}

/** Picks a field of the collection of one type. Optional ones can be left out. */
function FieldSelect({
  label,
  collection,
  type,
  value,
  optional,
  onChange,
}: {
  label: string;
  collection: Collection;
  type: "PlainText" | "RichText";
  value: string;
  optional?: boolean;
  onChange: (slug: string) => void;
}) {
  const fields = collection.fields.filter((f) => f.type === type && f.slug !== "slug");
  return (
    <Field label={label}>
      <Select value={value} onChange={(e) => onChange(e.target.value)}>
        {(optional || !value) && <option value="">{optional ? "Leave it out" : "Pick a field"}</option>}
        {fields.map((f) => (
          <option key={f.slug} value={f.slug}>
            {f.name}
          </option>
        ))}
      </Select>
    </Field>
  );
}

/** Each blog is connected and removed on its own, not with the page's Save. Secrets are write-only. Testing reads the blog
 *  with what was typed, and connecting reads it again before saving. Connecting the same blog again replaces its secret and
 *  unpauses it. */
export function BlogSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/blogs`;
  const { data, error, mutate } = useSWR<Blog[]>(key, fetcher);
  const { pending, run } = useAction();
  const [kind, setKind] = useState<BlogKind>("wordpress");
  const empty = { url: "", username: "", secret: "" };
  const [form, setForm] = useState(empty);
  const [tested, setTested] = useState<BlogTest | null>(null);
  const [siteId, setSiteId] = useState("");
  const [collectionId, setCollectionId] = useState("");
  const [fields, setFields] = useState<Fields>(guessFields(undefined));
  const [publish, setPublish] = useState(false);
  const [cap, setCap] = useState(2);
  const site = tested?.sites.find((s) => s.id === siteId);
  const collection = site?.collections.find((c) => c.id === collectionId);
  // Typing anything new means the blog has to be read again.
  const set = (field: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => {
    setForm({ ...form, [field]: e.target.value });
    setTested(null);
  };
  const pickKind = (k: BlogKind) => {
    setKind(k);
    setTested(null);
  };
  const pickCollection = (s: BlogTest["sites"][number] | undefined, id: string) => {
    setCollectionId(id);
    setFields(guessFields(s?.collections.find((c) => c.id === id)));
  };
  const pickSite = (found: BlogTest, id: string) => {
    const s = found.sites.find((x) => x.id === id);
    setSiteId(id);
    pickCollection(s, s?.collections[0]?.id ?? "");
  };
  const body = () => ({ kind, url: form.url.trim(), username: form.username.trim(), secret: form.secret.trim() });
  const test = () =>
    run("test", async () => {
      const found = await api<BlogTest>(`${key}/test`, { body: body() });
      setTested(found);
      if (kind === "webflow") pickSite(found, found.sites[0]?.id ?? "");
    });
  const connect = async () => {
    const done = await run("connect", async () => {
      const where = kind === "webflow" ? { site_id: siteId, collection_id: collectionId, ...fields } : {};
      await api(key, { body: { ...body(), ...where, publish, weekly_cap: cap } });
      await mutate();
    }, `Connected ${kind === "webflow" ? (site?.name ?? "Webflow") : tested?.name}`);
    if (done) {
      setForm(empty);
      setTested(null);
    }
  };
  const reconnect = (b: Blog) => {
    setKind(b.kind);
    setForm({ url: b.kind === "webflow" ? "" : b.url, username: b.username, secret: "" });
    setPublish(b.publish);
    setCap(b.weekly_cap);
    setTested(null);
  };
  const typed = !!form.secret.trim() && (kind === "webflow" || !!form.url.trim()) && (kind !== "wordpress" || !!form.username.trim());
  const ready = !!tested && (kind !== "webflow" || (!!collection && !!fields.body_field && !!fields.title_field));
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <NotebookPen className="size-4" /> Your blogs
        </span>
      }
      description="Drafts articles for your own WordPress, Ghost or Webflow blog, and publishes the ones you approve on the Social page."
    >
      {error ? (
        <LoadError error={error} what="your blogs" retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((b) => (
                <BlogRow key={b.id} blog={b} onChange={() => mutate()} onReconnect={reconnect} />
              ))}
            </ul>
          )}
          <form onSubmit={submit(tested ? connect : test)} className="flex flex-col gap-4">
            <div className="flex flex-col gap-3 sm:flex-row sm:items-center sm:justify-between">
              <p className="text-sm font-medium">{data?.length ? "Connect another blog" : "Connect a blog"}</p>
              <Tabs
                value={kind}
                onChange={pickKind}
                options={(Object.keys(BLOG_KINDS) as BlogKind[]).map((k) => ({ value: k, label: BLOG_KINDS[k] }))}
                className="self-start"
              />
            </div>
            <p className="text-sm text-pretty text-muted-foreground">{BLOG_STEPS[kind]}</p>
            <div className="grid gap-4 sm:grid-cols-2">
              {kind !== "webflow" && (
                // WordPress's three fields fill the grid with the site on a row of its own.
                <Field label={kind === "ghost" ? "Admin address" : "Site"} className={kind === "wordpress" ? "sm:col-span-2" : undefined}>
                  <Input autoComplete="off" placeholder={kind === "ghost" ? "example.ghost.io" : "example.com/blog"} value={form.url} onChange={set("url")} />
                </Field>
              )}
              {kind === "wordpress" && (
                <Field label="Username">
                  <Input autoComplete="off" value={form.username} onChange={set("username")} />
                </Field>
              )}
              <Field label={kind === "wordpress" ? "Application password" : kind === "ghost" ? "Admin API key" : "API token"} className={kind === "webflow" ? "sm:col-span-2" : undefined}>
                <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
              </Field>
            </div>
            {tested && kind !== "webflow" && (
              <p className="rounded-lg bg-muted/60 px-3 py-2 text-sm text-pretty">
                Found <span className="font-medium">{tested.name}</span>
                {tested.user ? `, signed in as ${tested.user}.` : "."}
              </p>
            )}
            {tested && kind === "webflow" && (
              <div className="grid gap-4 rounded-lg border border-border p-3 sm:grid-cols-2">
                <Field label="Site">
                  <Select value={siteId} onChange={(e) => pickSite(tested, e.target.value)}>
                    {tested.sites.map((s) => (
                      <option key={s.id} value={s.id}>
                        {s.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                <Field label="Collection">
                  <Select value={collectionId} onChange={(e) => pickCollection(site, e.target.value)}>
                    {site?.collections.map((c) => (
                      <option key={c.id} value={c.id}>
                        {c.name}
                      </option>
                    ))}
                  </Select>
                </Field>
                {collection && (
                  <>
                    <FieldSelect label="Title goes in" collection={collection} type="PlainText" value={fields.title_field} onChange={(title_field) => setFields({ ...fields, title_field })} />
                    <FieldSelect label="Article goes in" collection={collection} type="RichText" value={fields.body_field} onChange={(body_field) => setFields({ ...fields, body_field })} />
                    <FieldSelect label="Tags go in" collection={collection} type="PlainText" value={fields.tags_field} optional onChange={(tags_field) => setFields({ ...fields, tags_field })} />
                    <FieldSelect label="Excerpt goes in" collection={collection} type="PlainText" value={fields.excerpt_field} optional onChange={(excerpt_field) => setFields({ ...fields, excerpt_field })} />
                  </>
                )}
              </div>
            )}
            {tested && (
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="When you approve an article">
                  <PublishSelect kind={kind} publish={publish} onChange={setPublish} />
                </Field>
                <Field label="At most">
                  <CapSelect cap={cap} onChange={setCap} />
                </Field>
              </div>
            )}
            <div className="flex flex-wrap gap-2">
              {tested ? (
                <Button type="submit" loading={pending === "connect"} disabled={!ready}>
                  Connect blog
                </Button>
              ) : (
                <Button type="submit" loading={pending === "test"} disabled={!typed}>
                  Test connection
                </Button>
              )}
            </div>
          </form>
        </>
      )}
    </Section>
  );
}

/** The token is write-only. Connecting checks it with Hashnode and reads back the username and publications. */
export function HashnodeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "hashnode");
  const publication = useConnection(projectId, "hashnode/publication");
  const connect = async () => {
    if (await call({ token }, "Connected to Hashnode")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Hashnode? Approved articles and comments will wait until you connect it again.")) call(undefined, "Hashnode disconnected");
  };
  const publications = settings.hashnode_publications;
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <PenLine className="size-4" /> Hashnode
        </span>
      }
      description="Searches recent Hashnode articles by tag, comments on the ones you approve on the Social page, and publishes your articles."
      actions={
        settings.hashnode_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.hashnode_connected ? (
        <div className="flex flex-col gap-3 text-sm text-pretty">
          <p>Writing as @{settings.hashnode_username}, up to 2 articles and 10 comments a day, one every ten minutes.</p>
          {publications.length > 0 ? (
            <Field label="Publish articles in">
              <Select
                value={settings.hashnode_publication_id}
                disabled={publication.busy}
                onChange={(e) => publication.call({ publication_id: e.target.value }, "Publication saved")}
                className="sm:w-72"
              >
                {publications.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.title || p.url}
                  </option>
                ))}
              </Select>
            </Field>
          ) : (
            <p className="text-muted-foreground">The account has no blog yet, so only comments go out. Start one on Hashnode, then connect again.</p>
          )}
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://hashnode.com/settings/developer" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Hashnode&apos;s developer settings
            </a>
            , generate a personal access token.
          </p>
          <Field label="Access token">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.hashnode_error} at={settings.hashnode_failed_at} what="post" />
    </Section>
  );
}
