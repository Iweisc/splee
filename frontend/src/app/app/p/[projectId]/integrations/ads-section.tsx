"use client";

import { useEffect } from "react";
import { Megaphone } from "lucide-react";
import { toast } from "sonner";
import useSWR from "swr";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { AudienceNote } from "@/components/app/work/ad-audiences";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Select, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { AD_PLATFORM_NAMES, type AdConnection, type AdPlatform, type AdsStatus } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

/** What each platform asks for when it signs in, under its Connect button. */
const AD_HINTS: Record<AdPlatform, string> = {
  linkedin: "Fills a Matched Audience in your Campaign Manager account.",
  google: "Fills a Customer Match list in your Google Ads account.",
  meta: "Fills a Custom Audience in your Meta ad account. Accept Meta's Custom Audience terms there first.",
};

/** Sends the browser to the platform's sign-in page. It comes back to this page with how it went, which AdAudiencesSection shows. */
async function signInToAds(projectId: string, platform: AdPlatform) {
  const { url } = await api<{ url: string }>(`/projects/${projectId}/ads/${platform}/oauth`, { method: "POST" });
  window.location.assign(url);
}

/** The ad platforms the project signed in to, each with its ad account and the hot leads' audience, and a button for each one
 *  this Splee has an app for. Each campaign's own audiences are turned on from the campaign's page. */
export function AdAudiencesSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/ads`;
  const { data, error, mutate } = useSWR<AdsStatus>(key, fetcher);
  const { pending, busy, run } = useAction();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("ads");
    const failed = params.get("ads_error");
    if (!done && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done && done in AD_PLATFORM_NAMES) {
      toast.success(`Connected to ${AD_PLATFORM_NAMES[done as AdPlatform]}`);
      mutate();
    }
  }, [mutate]);

  if (error) return <LoadError error={error} what="your ad platforms" retry={() => mutate()} />;
  if (!data || (!data.available.length && !data.connections.length)) return null;
  const open = data.available.filter((p) => !data.connections.some((c) => c.platform === p));
  const call = (what: string, path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>, done?: string) =>
    run(what, () => mutate(api<AdsStatus>(`${key}/${path}`, { method, body }), { revalidate: false }), done);
  return (
    <div id="ad-audiences" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <Megaphone className="size-4" /> Ad audiences
          </span>
        }
        description="Show ads to the people you're emailing. Turn on an audience per campaign from its page, and Splee keeps it in sync every hour. Only hashed addresses and numbers leave Splee."
      >
        {!!data.connections.length && (
          <ul className="mb-4 divide-y divide-border rounded-lg border border-border">
            {data.connections.map((c) => (
              <ConnectedAds key={c.platform} projectId={projectId} connection={c} hot={data.audiences.find((a) => a.platform === c.platform)} busy={busy} call={call} />
            ))}
          </ul>
        )}
        {!!open.length && (
          <div className="flex flex-col gap-3 sm:flex-row sm:flex-wrap">
            {open.map((p) => (
              <div key={p} className="flex flex-col items-start gap-1.5 sm:flex-1 sm:basis-56">
                <Button variant="outline" onClick={() => run(p, () => signInToAds(projectId, p))} loading={pending === p}>
                  Connect {AD_PLATFORM_NAMES[p]}
                </Button>
                <p className="text-xs text-pretty text-muted-foreground">{AD_HINTS[p]}</p>
              </div>
            ))}
          </div>
        )}
      </Section>
    </div>
  );
}

type AdsCall = (what: string, path: string, method: "PUT" | "DELETE", body?: Record<string, unknown>, done?: string) => Promise<boolean>;

function ConnectedAds({
  projectId,
  connection: c,
  hot,
  busy,
  call,
}: {
  projectId: string;
  connection: AdConnection;
  hot: AdsStatus["audiences"][number] | undefined;
  busy: (key: string) => boolean;
  call: AdsCall;
}) {
  const name = AD_PLATFORM_NAMES[c.platform];
  const { pending: signingIn, run } = useAction();
  const disconnect = () => {
    if (confirm(`Disconnect ${name}? Splee deletes its audiences there and stops syncing.`)) call(`${c.platform}:off`, c.platform, "DELETE", undefined, `${name} disconnected`);
  };
  const pick = (account_id: string) => call(`${c.platform}:account`, `${c.platform}/account`, "PUT", { account_id }, "Ad account picked");
  const toggleHot = (on: boolean) => call(`${c.platform}:hot`, `${c.platform}/hot`, "PUT", { on }, on ? `Hot leads audience made in ${name}` : `Hot leads audience deleted in ${name}`);
  return (
    <li className="px-3 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <div className="min-w-40 flex-1">
          <p className="text-sm font-medium">{name}</p>
          <p className="text-xs text-pretty text-muted-foreground">
            {c.needs_reconnect ? "Signed out. Reconnect to keep its audiences in sync." : c.ad_account_id ? `Ad account: ${c.ad_account_name}` : "Pick the ad account to fill."}
          </p>
          {c.held_until && <p className="text-xs text-pretty text-amber-700">{name} asked Splee to slow down. Syncing again {timeAgo(c.held_until)}.</p>}
        </div>
        <div className="flex gap-2">
          {c.needs_reconnect && (
            <Button size="sm" onClick={() => run("reconnect", () => signInToAds(projectId, c.platform))} loading={signingIn === "reconnect"}>
              Reconnect
            </Button>
          )}
          <Button size="sm" variant="outline" onClick={disconnect} loading={busy(`${c.platform}:off`)}>
            Disconnect
          </Button>
        </div>
      </div>
      {c.ad_accounts.length > 1 && (
        <Field label="Ad account" hint="Audiences move to the account you pick." className="mt-3 sm:w-80">
          <Select value={c.ad_account_id} disabled={c.needs_reconnect || busy(`${c.platform}:account`)} onChange={(e) => pick(e.target.value)}>
            {!c.ad_account_id && <option value="">Pick one</option>}
            {c.ad_accounts.map((a) => (
              <option key={a.id} value={a.id}>
                {a.name}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <div className="mt-3 flex items-center gap-3">
        <Switch
          checked={!!hot}
          onChange={toggleHot}
          loading={busy(`${c.platform}:hot`)}
          disabled={!hot && (c.needs_reconnect || !c.ad_account_id)}
          label={`Hot leads audience in ${name}`}
        />
        <span className="min-w-0">
          <span className="block text-sm">Hot leads from every campaign</span>
          {hot ? <AudienceNote audience={hot} /> : <span className="block text-xs text-muted-foreground">An audience called Splee: Hot leads</span>}
        </span>
      </div>
    </li>
  );
}
