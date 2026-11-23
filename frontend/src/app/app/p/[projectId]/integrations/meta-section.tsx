"use client";

import { useEffect } from "react";
import { RefreshCw, Share2 } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/form";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectSettings, SocialChannel } from "@/lib/types";
import { LastFailure, useConnection } from "./shared";

/** What the user's role lets Splee do on a Page or Instagram account, for its row. */
function metaNote(c: SocialChannel) {
  const where = c.platform === "facebook" ? "Facebook Page" : `Instagram, through ${c.guild_name}`;
  if (!c.can_read && !c.can_send) return `${where} · Your role here can't post or answer comments`;
  if (!c.can_send) return `${where} · Answers comments only, your role can't post`;
  if (!c.can_read) return `${where} · Posts only, your role can't answer comments`;
  return `${where} · Posts and answers comments`;
}

/** The Pages and Instagram accounts the signed-in user manages, each picked on its own. Signing in again reads the list again. */
function MetaAccounts({ projectId, signIn, signingIn }: { projectId: string; signIn: () => void; signingIn: boolean }) {
  const { data, error, mutate } = useSWR<SocialChannel[]>(`/projects/${projectId}/social-channels`, fetcher);
  const { busy, run } = useAction();
  // Each Instagram account right under its Page.
  const pages = (data ?? []).filter((c) => c.platform === "facebook");
  const accounts = pages.flatMap((p) => [p, ...(data ?? []).filter((c) => c.platform === "instagram" && c.guild_id === p.chat_id)]);
  const pick = (c: SocialChannel, picked: boolean) =>
    run(c.id, async () => {
      const fresh = await api<SocialChannel>(`/social-channels/${c.id}`, { method: "PATCH", body: { picked } });
      await mutate(data?.map((x) => (x.id === c.id ? fresh : x)), { revalidate: false });
    });
  if (error) return <LoadError error={error} what="your Pages" retry={() => mutate()} />;
  const label = (c: SocialChannel) => (c.platform === "instagram" ? `@${c.username}` : c.name);
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm font-medium">Pages and accounts the agent may use</p>
        <Button size="sm" variant="ghost" onClick={signIn} loading={signingIn} className="px-2">
          <RefreshCw className="size-3.5" /> Refresh
        </Button>
      </div>
      {!data ? null : accounts.length === 0 ? (
        <p className="text-sm text-pretty text-muted-foreground">Facebook didn&apos;t share any Pages. Refresh, and pick your Pages when Facebook asks.</p>
      ) : (
        <ul className="divide-y divide-border rounded-lg border border-border">
          {accounts.map((c) => (
            <li key={c.id} className="flex items-center gap-3 px-3 py-2.5">
              <div className="min-w-0 flex-1">
                <p className="truncate text-sm font-medium">{label(c)}</p>
                <p className="text-xs text-pretty text-muted-foreground">{metaNote(c)}</p>
              </div>
              <Switch
                checked={c.picked}
                onChange={(on) => pick(c, on)}
                loading={busy(c.id)}
                disabled={!c.picked && !c.can_read && !c.can_send}
                label={`Use ${label(c)}`}
              />
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Connects by signing in with Facebook, which only shows when the server has a Meta app. Facebook sends the browser back here
 *  with how it went, and the user picks the Pages and Instagram accounts the agent may use. */
export function MetaSection({ projectId, settings }: { projectId: string; settings: ProjectSettings }) {
  const { me } = useMe();
  const { pending, run } = useAction();
  const { busy, call } = useConnection(projectId, "meta");
  const { mutate } = useSWRConfig();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const connected = params.get("meta") === "connected";
    const failed = params.get("meta_error");
    if (!connected && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (connected) {
      toast.success("Signed in with Facebook. Pick the Pages and accounts the agent may use.");
      mutate(`/projects/${projectId}/settings`);
      mutate(`/projects/${projectId}/social-channels`);
    }
  }, [mutate, projectId]);

  if (!me?.meta_sign_in && !settings.meta_connected && !settings.meta_reconnect) return null;
  const signIn = () =>
    run("meta", async () => {
      const { url } = await api<{ url: string }>(`/projects/${projectId}/integrations/meta/oauth`, { method: "POST" });
      window.location.assign(url);
    });
  const disconnect = () => {
    if (confirm("Disconnect Facebook and Instagram? Approved posts and replies will wait until you sign in again.")) call(undefined, "Facebook and Instagram disconnected");
  };
  return (
    <Section
      title={
        <span className="flex items-center gap-2">
          <Share2 className="size-4" /> Facebook and Instagram
        </span>
      }
      description="Reads the comments on your recent posts, drafts answers and posts for the Social page, and posts the ones you approve on your Pages and Instagram accounts."
      actions={
        settings.meta_connected ? (
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy}>
            Disconnect
          </Button>
        ) : undefined
      }
    >
      {settings.meta_connected ? (
        <div className="flex flex-col gap-4 text-sm text-pretty">
          <p>Signed in as {settings.meta_name}. Each Page and account posts up to 5 times and answers up to 15 comments a day.</p>
          <MetaAccounts projectId={projectId} signIn={signIn} signingIn={pending === "meta"} />
        </div>
      ) : settings.meta_reconnect ? (
        <div className="flex flex-col items-start gap-4 text-sm text-pretty">
          <p className="rounded-lg bg-amber-50 px-3 py-2 text-amber-800">
            Facebook signed Splee out, so nothing goes out on your Pages or Instagram accounts. Approved posts went back to drafts. Sign in as{" "}
            {settings.meta_name} again to pick up where you left off.
          </p>
          <Button onClick={signIn} loading={pending === "meta"}>
            Sign in with Facebook again
          </Button>
        </div>
      ) : (
        <div className="flex flex-col items-start gap-4">
          <p className="text-sm text-pretty text-muted-foreground">
            Facebook asks which of your Pages Splee may use. An Instagram account takes part when it&apos;s a business or creator account linked to
            one of them.
          </p>
          <Button onClick={signIn} loading={pending === "meta"}>
            Sign in with Facebook
          </Button>
        </div>
      )}
      {settings.meta_connected && (
        <>
          <LastFailure error={settings.facebook_error} at={settings.facebook_failed_at} what="post" />
          <LastFailure error={settings.instagram_error} at={settings.instagram_failed_at} what="post" />
        </>
      )}
    </Section>
  );
}
