"use client";

import { useEffect, useState } from "react";
import { AtSign, Cloud, Globe, MessagesSquare, Repeat2, Spool, TvMinimalPlay, UsersRound, Zap } from "lucide-react";
import { toast } from "sonner";
import { useSWRConfig } from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings } from "@/lib/types";
import { LastFailure, useConnection, submit } from "./shared";

/** The cookies are write-only. Connecting has bird sign in with them and reports the handle. */
export function XSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [authToken, setAuthToken] = useState("");
  const [ct0, setCt0] = useState("");
  const { busy, call } = useConnection(projectId, "x");
  const connect = async () => {
    if (await call({ auth_token: authToken, ct0 }, "Connected to X")) {
      setAuthToken("");
      setCt0("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect X? Approved posts for X will wait until you connect it again.")) call(undefined, "X disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <AtSign className="size-4" /> X
        </span>
      }
      description="Searches X as your account and posts what you approve on the Social page."
      actions={
        settings.x_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.x_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.x_handle}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In a browser that&apos;s signed in to x.com, open the dev tools and copy the auth_token and ct0 cookies.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="auth_token" className="flex-1">
              <Input type="password" autoComplete="off" value={authToken} onChange={(e) => setAuthToken(e.target.value.trim())} />
            </Field>
            <Field label="ct0" className="flex-1">
              <Input type="password" autoComplete="off" value={ct0} onChange={(e) => setCt0(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!authToken || !ct0}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.x_error} at={settings.x_failed_at} what="post" />
    </Section>
  );
}

/** The secret and password are write-only. Connecting signs in to Reddit with them and reads the username back. */
export function RedditSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ client_id: "", secret: "", username: "", password: "" });
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value });
  const { busy, call } = useConnection(projectId, "reddit");
  const connect = async () => {
    if (await call(form, "Connected to Reddit")) setForm({ client_id: "", secret: "", username: "", password: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Reddit? Approved posts for Reddit will wait until you connect it again.")) call(undefined, "Reddit disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <MessagesSquare className="size-4" /> Reddit
        </span>
      }
      description="Searches Reddit as your account and posts what you approve on the Social page."
      actions={
        settings.reddit_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.reddit_connected ? (
        <p className="text-sm text-pretty">Posting as u/{settings.reddit_username}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Signed in as the account, open{" "}
            <a href="https://www.reddit.com/prefs/apps" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              reddit.com/prefs/apps
            </a>{" "}
            and create a script app. Any redirect URI works. The client id is the code under the app&apos;s name.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Client id">
              <Input autoComplete="off" value={form.client_id} onChange={set("client_id")} />
            </Field>
            <Field label="Secret">
              <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
            </Field>
            <Field label="Username">
              <Input autoComplete="off" placeholder="u/yourname" value={form.username} onChange={set("username")} />
            </Field>
            <Field label="Password" hint="Accounts with two-factor sign-in can't connect this way.">
              <Input type="password" autoComplete="off" value={form.password} onChange={set("password")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.client_id.trim() || !form.secret.trim() || !form.username.trim() || !form.password} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.reddit_error} at={settings.reddit_failed_at} what="post" />
    </Section>
  );
}

/** The app password is write-only. Connecting signs in to Bluesky with it and reads the handle back. */
export function BlueskySection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [handle, setHandle] = useState("");
  const [password, setPassword] = useState("");
  const { busy, call } = useConnection(projectId, "bluesky");
  const connect = async () => {
    if (await call({ handle, password }, "Connected to Bluesky")) {
      setHandle("");
      setPassword("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Bluesky? Approved posts for Bluesky will wait until you connect it again.")) call(undefined, "Bluesky disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Cloud className="size-4" /> Bluesky
        </span>
      }
      description="Searches Bluesky as your account and posts what you approve on the Social page."
      actions={
        settings.bluesky_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.bluesky_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.bluesky_handle}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Signed in as the account, open{" "}
            <a href="https://bsky.app/settings/app-passwords" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              bsky.app/settings/app-passwords
            </a>{" "}
            and add an app password. Splee never needs the account&apos;s own password.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="Handle" className="flex-1">
              <Input autoComplete="off" placeholder="yourname.bsky.social" value={handle} onChange={(e) => setHandle(e.target.value.trim())} />
            </Field>
            <Field label="App password" className="flex-1">
              <Input type="password" autoComplete="off" placeholder="xxxx-xxxx-xxxx-xxxx" value={password} onChange={(e) => setPassword(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!handle || !password}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.bluesky_error} at={settings.bluesky_failed_at} what="post" />
    </Section>
  );
}

/** The token is write-only. Connecting checks it with the instance and reads the account and the instance's post limit back. */
export function MastodonSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [url, setUrl] = useState("");
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "mastodon");
  const connect = async () => {
    if (await call({ url, token }, "Connected to Mastodon")) {
      setUrl("");
      setToken("");
    }
  };
  const disconnect = () => {
    if (confirm("Disconnect Mastodon? Approved posts for Mastodon will wait until you connect it again.")) call(undefined, "Mastodon disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Globe className="size-4" /> Mastodon
        </span>
      }
      description="Searches Mastodon as your account and posts what you approve on the Social page."
      actions={
        settings.mastodon_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.mastodon_connected ? (
        <p className="text-sm text-pretty">
          Posting as @{settings.mastodon_handle}, up to 15 posts a day of up to {settings.mastodon_max_chars} characters. Approved posts go out one at a time
          every ten minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            On your instance, open Preferences, then Development, and create an application with the read and write:statuses scopes. Copy its access
            token.
          </p>
          <div className="flex flex-col gap-4 sm:flex-row sm:items-end">
            <Field label="Instance" className="flex-1">
              <Input autoComplete="off" placeholder="mastodon.social" value={url} onChange={(e) => setUrl(e.target.value.trim())} />
            </Field>
            <Field label="Access token" className="flex-1">
              <Input type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
            </Field>
            <Button type="submit" loading={busy} disabled={!url || !token}>
              Connect
            </Button>
          </div>
        </form>
      )}
      <LastFailure error={settings.mastodon_error} at={settings.mastodon_failed_at} what="post" />
    </Section>
  );
}

/** The token is write-only. Connecting checks it with Threads and reads the account back. */
export function ThreadsSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [token, setToken] = useState("");
  const { busy, call } = useConnection(projectId, "threads");
  const connect = async () => {
    if (await call({ token }, "Connected to Threads")) setToken("");
  };
  const disconnect = () => {
    if (confirm("Disconnect Threads? Approved posts for Threads will wait until you connect it again.")) call(undefined, "Threads disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Spool className="size-4" /> Threads
        </span>
      }
      description="Searches Threads as your account and posts what you approve on the Social page."
      actions={
        settings.threads_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.threads_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.threads_username}, up to 15 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In{" "}
            <a href="https://developers.facebook.com/apps" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              Meta for Developers
            </a>
            , create an app with the Threads API use case and the threads_basic, threads_content_publish, threads_read_replies and
            threads_keyword_search permissions. Add your account as a Threads tester, then generate a long-lived access token.
          </p>
          <Field label="Access token" hint="It lasts 60 days. Until Meta approves keyword search for the app, searches only find your own posts.">
            <div className="flex gap-2">
              <Input aria-label="Access token" type="password" autoComplete="off" value={token} onChange={(e) => setToken(e.target.value.trim())} />
              <Button type="submit" loading={busy} disabled={!token}>
                Connect
              </Button>
            </div>
          </Field>
        </form>
      )}
      <LastFailure error={settings.threads_error} at={settings.threads_failed_at} what="post" />
    </Section>
  );
}

/** The client secret and refresh token are write-only. Connecting trades the token with Google and reads the channel back. */
export function YouTubeSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ client_id: "", secret: "", refresh_token: "" });
  const set = (key: keyof typeof form) => (e: React.ChangeEvent<HTMLInputElement>) => setForm({ ...form, [key]: e.target.value.trim() });
  const { busy, call } = useConnection(projectId, "youtube");
  const connect = async () => {
    if (await call(form, "Connected to YouTube")) setForm({ client_id: "", secret: "", refresh_token: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect YouTube? Approved comments for YouTube will wait until you connect it again.")) call(undefined, "YouTube disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <TvMinimalPlay className="size-4" /> YouTube
        </span>
      }
      description="Searches YouTube for videos and posts the comments you approve on the Social page as your channel."
      actions={
        settings.youtube_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.youtube_connected ? (
        <p className="text-sm text-pretty">
          Commenting as {settings.youtube_channel}, up to 15 comments a day. Approved comments go out one at a time every ten minutes.
        </p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            In Google Cloud, turn on the YouTube Data API v3, create an OAuth client and publish the app, since tokens from an app in testing
            expire in 7 days. Then in the{" "}
            <a href="https://developers.google.com/oauthplayground" target="_blank" rel="noreferrer" className="font-medium text-foreground underline underline-offset-4">
              OAuth Playground
            </a>
            , use your own client, sign in as the channel with the youtube.force-ssl scope and copy the refresh token.
          </p>
          <div className="grid gap-4 sm:grid-cols-2">
            <Field label="Client id">
              <Input autoComplete="off" placeholder="1234.apps.googleusercontent.com" value={form.client_id} onChange={set("client_id")} />
            </Field>
            <Field label="Client secret">
              <Input type="password" autoComplete="off" value={form.secret} onChange={set("secret")} />
            </Field>
            <Field label="Refresh token" className="sm:col-span-2">
              <Input type="password" autoComplete="off" value={form.refresh_token} onChange={set("refresh_token")} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.client_id || !form.secret || !form.refresh_token} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.youtube_error} at={settings.youtube_failed_at} what="post" />
    </Section>
  );
}

/** The password is write-only. Connecting signs in to the instance and reads the account back. */
export function LemmySection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ url: "", username: "", password: "" });
  const { busy, call } = useConnection(projectId, "lemmy");
  const connect = async () => {
    if (await call(form, "Connected to Lemmy")) setForm({ url: "", username: "", password: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Lemmy? Approved posts for Lemmy will wait until you connect it again.")) call(undefined, "Lemmy disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <UsersRound className="size-4" /> Lemmy
        </span>
      }
      description="Searches Lemmy communities as your account and posts what you approve on the Social page."
      actions={
        settings.lemmy_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.lemmy_connected ? (
        <p className="text-sm text-pretty">Posting as @{settings.lemmy_handle}, up to 10 posts a day. Approved posts go out one at a time every ten minutes.</p>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Splee signs in as your account on its instance. Accounts that sign in with two-factor codes can&apos;t be used.
          </p>
          <div className="grid gap-4 sm:grid-cols-3">
            <Field label="Instance">
              <Input autoComplete="off" placeholder="lemmy.world" value={form.url} onChange={(e) => setForm({ ...form, url: e.target.value.trim() })} />
            </Field>
            <Field label="Username">
              <Input autoComplete="off" value={form.username} onChange={(e) => setForm({ ...form, username: e.target.value.trim() })} />
            </Field>
            <Field label="Password">
              <Input type="password" autoComplete="off" value={form.password} onChange={(e) => setForm({ ...form, password: e.target.value })} />
            </Field>
          </div>
          <Button type="submit" loading={busy} disabled={!form.url || !form.username || !form.password} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.lemmy_error} at={settings.lemmy_failed_at} what="post" />
    </Section>
  );
}

const hostOfRelay = (r: string) => r.replace(/^wss:\/\//, "");

/** "a", "a and b", "a, b and c". */
const listOf = (items: string[]) => (items.length < 2 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`);

/** The secret key is write-only. Connecting reads the npub from it and checks that each relay is wss:// at a public address. */
export function NostrSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const [form, setForm] = useState({ key: "", relays: "" });
  const { busy, call } = useConnection(projectId, "nostr");
  const connect = async () => {
    if (await call({ key: form.key, relays: [form.relays] }, "Connected to Nostr")) setForm({ key: "", relays: "" });
  };
  const disconnect = () => {
    if (confirm("Disconnect Nostr? Approved notes will wait until you connect it again.")) call(undefined, "Nostr disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Zap className="size-4" /> Nostr
        </span>
      }
      description="Searches Nostr for notes where your buyers ask for help, and posts the notes and replies you approve on the Social page with your key."
      actions={
        settings.nostr_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.nostr_connected ? (
        <div className="flex flex-col gap-1.5 text-sm text-pretty">
          <p>Posting as</p>
          <p className="rounded-lg bg-muted/60 px-3 py-2 font-mono text-xs break-all">{settings.nostr_npub}</p>
          <p>Notes go to {listOf(settings.nostr_relays.map(hostOfRelay))}, up to 15 a day, one every ten minutes.</p>
        </div>
      ) : (
        <form onSubmit={submit(connect)} className="flex flex-col gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Splee signs each note with your secret key and sends it to your relays. Use a key you made for the company, not your own.
          </p>
          <Field label="Secret key">
            <Input type="password" autoComplete="off" placeholder="nsec1…" value={form.key} onChange={(e) => setForm({ ...form, key: e.target.value.trim() })} />
          </Field>
          <Field label="Relays" hint="One per line. Leave it empty for relay.damus.io, nos.lol and relay.nostr.band.">
            <Textarea
              rows={3}
              autoComplete="off"
              placeholder="wss://relay.damus.io"
              value={form.relays}
              onChange={(e) => setForm({ ...form, relays: e.target.value })}
              className="min-h-0"
            />
          </Field>
          <Button type="submit" loading={busy} disabled={!form.key} className="sm:self-start">
            Connect
          </Button>
        </form>
      )}
      <LastFailure error={settings.nostr_error} at={settings.nostr_failed_at} what="post" />
    </Section>
  );
}

/** Connects by signing in to Tumblr, which only shows when the server has a Tumblr app. Tumblr sends the browser back here
 *  with how it went. */
export function TumblrSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { pending, run } = useAction();
  const { busy, call } = useConnection(projectId, "tumblr");
  const { mutate } = useSWRConfig();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("tumblr") === "connected";
    const failed = params.get("tumblr_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) {
      toast.success("Connected to Tumblr");
      mutate(`/projects/${projectId}/settings`);
    }
  }, [mutate, projectId]);

  if (!me?.tumblr_sign_in && !settings.tumblr_connected) return null;
  const signIn = () =>
    run("tumblr", async () => {
      const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/tumblr/oauth`, { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (confirm("Disconnect Tumblr? Approved posts and reblogs will wait until you connect it again.")) call(undefined, "Tumblr disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Repeat2 className="size-4" /> Tumblr
        </span>
      }
      description="Searches Tumblr tags for posts where your buyers ask for help, and posts what you approve on the Social page from your blog."
      actions={
        settings.tumblr_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      <p className="text-sm text-pretty text-muted-foreground">
        Tumblr has no replies for apps, so Splee answers a post by reblogging it to your blog with your comment on top.
      </p>
      {settings.tumblr_connected ? (
        <div className="mt-4 flex flex-col gap-3">
          <Field label="Post as" hint={`Signed in as ${settings.tumblr_username}. Up to 10 posts and reblogs a day, one every ten minutes.`}>
            <Select value={settings.tumblr_blog} disabled={busy} onChange={(e) => call({ blog: e.target.value }, `Posting as ${e.target.value}`)} className="sm:w-72">
              {settings.tumblr_blogs.map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
            </Select>
          </Field>
        </div>
      ) : (
        <Button onClick={signIn} loading={pending === "tumblr"} className="mt-4">
          Connect Tumblr
        </Button>
      )}
      <LastFailure error={settings.tumblr_error} at={settings.tumblr_failed_at} what="post" />
    </Section>
  );
}
