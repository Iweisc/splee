"use client";

import { useState } from "react";
import { MessageCircleQuestionMark, MessageSquareText, Trash2 } from "lucide-react";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Host } from "@/components/ui/misc";
import { Field, Input } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { DiscourseForum, ProjectSettings } from "@/lib/types";
import { LastFailure, useConnection, submit, hostOf } from "./shared";

/** Stack Exchange's OAuth dialog for an app with client side OAuth. It ends on a page with the token in its address. */
const stackExchangeDialog = (clientId: string) =>
  `https://stackoverflow.com/oauth/dialog?${new URLSearchParams({
    client_id: clientId,
    scope: "write_access,no_expiry",
    redirect_uri: "https://stackexchange.com/oauth/login_success",
  })}`;

/** The access token is write-only. Connecting checks that it can write and that its user has an account on the site. The client
 *  id only opens the dialog that makes the token, so it isn't sent. */
export function StackExchangeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ key: "", token: "", site: "" });
  const [clientId, setClientId] = useState("");
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const { busy, call } = useConnection(projectId, "stackexchange");
  const connect = async () => {
    if (await call(form, "Connected to Stack Exchange")) setForm({ key: "", token: "", site: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Stack Exchange? Approved answers will wait until you connect it again.")) call(undefined, "Stack Exchange disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessageCircleQuestionMark className="size-4" /> Stack Exchange
        </span>
      }
      description="Searches one Stack Exchange site, like Stack Overflow, for open questions and posts the answers you approve on the Social page."
      actions={
        settings.stackexchange_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.stackexchange_connected ? (
        <p className="text-sm text-pretty">
          Answering as {settings.stackexchange_user} on {settings.stackexchange_site}, up to 5 answers a day. Approved answers go out one at a time every ten
          minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            On{" "}
            <a href="https://stackapps.com/apps/oauth/register" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Stack Apps
            </a>
            , register an app with client side OAuth turned on and give it a Stack Apps post, which apps need before they can write. Copy its key, then
            get an access token with its client id.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Key">
              <Input autoComplete="off" value={form.key} onChange={set("key")} />
            </Field>
            <Field label="Site" hint="One site per project.">
              <Input autoComplete="off" placeholder="stackoverflow.com" value={form.site} onChange={set("site")} />
            </Field>
            <Field label="Client id" hint="Copy access_token from the address it ends on.">
              <div className="flex gap-2">
                <Input aria-label="Client id" autoComplete="off" inputMode="numeric" value={clientId} onChange={(e) => setClientId(e.target.value.trim())} />
                <Button
                  type="button"
                  variant="outline"
                  disabled={!clientId}
                  onClick={() => window.open(stackExchangeDialog(clientId), "_blank", "noreferrer")}
                >
                  Get token
                </Button>
              </div>
            </Field>
            <Field label="Access token">
              <Input type="password" autoComplete="off" value={form.token} onChange={set("token")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.key || !form.token || !form.site} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.stackexchange_error} at={settings.stackexchange_failed_at} what="post" />
    </Section>
  );
}

/** Each forum is added and removed on its own, not with the page's Save. Keys are write-only, and adding checks the key with
 *  the forum and reads back the user it posts as. Adding the same user again replaces the key and the category. */
export function DiscourseSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/discourse-forums`;
  const { data, error, mutate } = useSWR<DiscourseForum[]>(key, fetcher);
  const { pending, run } = useAction();
  const empty = { url: "", key: "", username: "", category: "" };
  const [form, setForm] = useState(empty);
  const set = (field: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [field]: e.target.value });
  const add = async () => {
    const done = await run("add", async () => {
      await api(key, { body: { ...form, url: form.url.trim(), key: form.key.trim() } });
      await mutate();
    }, `Joined ${hostOf(`https://${form.url.trim().replace(/^https?:\/\//, "")}`)}`);
    if (done) setForm(empty);
  };
  const remove = (f: DiscourseForum) => {
    if (!confirm(`Leave ${hostOf(f.url)}? Approved posts for it go back to drafts.`)) return;
    run(f.id, async () => {
      await api(`/discourse-forums/${f.id}`, { method: "DELETE" });
      await mutate();
    }, `Left ${hostOf(f.url)}`);
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessageSquareText className="size-4" /> Discourse forums
        </span>
      }
      description="Searches the forums you join for recent topics where your buyers ask for help, and posts the replies you approve on the Social page."
    >
      {error ? (
        <LoadError error={error} what="your forums" retry={() => mutate()} />
      ) : (
        <>
          {!!data?.length && (
            <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
              {data.map((f) => (
                <li key={f.id} className="px-3 py-2.5">
                  <div className="flex items-center gap-2">
                    <div className="min-w-0 flex-1">
                      <p className="text-sm font-medium">
                        <Host host={f.url.replace(/^https:\/\//, "")} />
                      </p>
                      <p className="text-xs text-pretty text-muted-foreground">
                        Posting as {f.username}, up to 5 a day. {f.category_id ? `New topics go in ${f.category_name}.` : "Replies only."}
                      </p>
                    </div>
                    <Button size="sm" variant="ghost" onClick={() => remove(f)} loading={pending === f.id} aria-label={`Leave ${hostOf(f.url)}`}>
                      <Trash2 className="size-3.5" />
                    </Button>
                  </div>
                  <LastFailure error={f.error} at={f.failed_at} what="post" />
                </li>
              ))}
            </ul>
          )}
          <form onSubmit={submit(add)} className="flex flex-col gap-4">
            <p className="text-sm text-pretty text-muted-foreground">
              On a forum you run, open Admin, then API, and make a key for your own user. On other forums, paste a User API Key and leave the
              username empty.
            </p>
            <div className="grid gap-4 sm:grid-cols-2">
              <Field label={data?.length ? "Another forum" : "Forum"}>
                <Input autoComplete="off" placeholder="forum.example.com" value={form.url} onChange={set("url")} />
              </Field>
              <Field label="API key">
                <Input type="password" autoComplete="off" value={form.key} onChange={set("key")} />
              </Field>
              <Field label="Username" hint="Only for a key an admin made.">
                <Input autoComplete="off" value={form.username} onChange={set("username")} />
              </Field>
              <Field label="Category for new topics" hint="Leave it empty for replies only.">
                <Input autoComplete="off" placeholder="Name or link" value={form.category} onChange={set("category")} />
              </Field>
            </div>
            <Button type="submit" loading={pending === "add"} disabled={!form.url.trim() || !form.key.trim()} className="sm:self-start">
              Join forum
            </Button>
          </form>
        </>
      )}
    </Section>
  );
}
