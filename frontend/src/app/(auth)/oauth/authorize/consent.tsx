"use client";

import { useRouter } from "next/navigation";
import { useEffect, useState } from "react";
import useSWR from "swr";
import { TriangleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Field, Select } from "@/components/ui/form";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";

type Checked = { redirect: string } | { client_name: string; client_host: string | null; redirect_host: string; local: boolean };

export function Consent({ params }: { params: Record<string, string> }) {
  const router = useRouter();
  const { me } = useMe();
  const query = new URLSearchParams(params).toString();
  const { data, error } = useSWR<Checked>(me ? `/oauth/authorize?${query}` : null, fetcher, { revalidateOnFocus: false });
  const [orgId, setOrgId] = useState<string>();
  const [pending, setPending] = useState<"allow" | "deny" | null>(null);
  const [failed, setFailed] = useState<string | null>(null);
  const redirect = data && "redirect" in data ? data.redirect : null;

  useEffect(() => {
    if (me === null) router.replace(`/signin?next=${encodeURIComponent(`/oauth/authorize?${query}`)}`);
  }, [me, query, router]);
  // A request the app got wrong goes straight back to it as an OAuth error.
  useEffect(() => {
    if (redirect) window.location.assign(redirect);
  }, [redirect]);

  if (error) {
    return (
      <Card className="w-full max-w-[440px] p-8">
        <h1 className="text-2xl font-medium tracking-tight">Can&apos;t connect this app</h1>
        <p className="mt-2 text-sm text-muted-foreground">{error.message}</p>
      </Card>
    );
  }
  if (!me || !data || "redirect" in data) {
    return (
      <Card className="flex w-full max-w-[440px] justify-center p-8">
        <Spinner />
      </Card>
    );
  }

  const org = orgId ?? me.org.id;
  const orgName = me.orgs.find((o) => o.id === org)?.name ?? me.org.name;
  const decide = async (allow: boolean) => {
    setPending(allow ? "allow" : "deny");
    setFailed(null);
    try {
      const res = await api<{ redirect: string }>("/oauth/authorize", { body: { ...params, allow, org_id: org } });
      window.location.assign(res.redirect);
    } catch (e) {
      setFailed(e instanceof Error ? e.message : "Couldn't finish connecting the app.");
      setPending(null);
    }
  };

  return (
    <Card className="w-full max-w-[440px] p-8">
      <h1 className="text-2xl font-medium tracking-tight text-balance">Connect {data.client_name} to Splee</h1>
      <p className="mt-2 text-sm text-muted-foreground">
        It can do what a member can in every project of <span className="font-medium text-foreground">{orgName}</span>: search companies, start
        campaigns, find people, write emails and approve them to go out from your mailboxes.
      </p>
      {me.orgs.length > 1 && (
        <Field label="Organization" className="mt-5">
          <Select value={org} onChange={(e) => setOrgId(e.target.value)}>
            {me.orgs.map((o) => (
              <option key={o.id} value={o.id}>
                {o.name}
              </option>
            ))}
          </Select>
        </Field>
      )}
      <dl className="mt-5 grid gap-2 rounded-lg border border-border px-4 py-3 text-sm">
        <div className="flex flex-wrap justify-between gap-x-4">
          <dt className="text-muted-foreground">Sends you back to</dt>
          <dd className="min-w-0 font-mono break-all">{data.redirect_host}</dd>
        </div>
        {data.client_host && (
          <div className="flex flex-wrap justify-between gap-x-4">
            <dt className="text-muted-foreground">Published by</dt>
            <dd className="min-w-0 font-mono break-all">{data.client_host}</dd>
          </div>
        )}
      </dl>
      {data.local && (
        <p className="mt-4 flex gap-2 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
          <TriangleAlert className="mt-0.5 size-4 shrink-0" />
          This app runs on your computer. Allow it only if you started connecting it yourself just now.
        </p>
      )}
      {failed && <p className="mt-4 text-sm text-danger">{failed}</p>}
      <div className="mt-6 grid grid-cols-2 gap-3">
        <Button variant="outline" onClick={() => decide(false)} loading={pending === "deny"} disabled={pending !== null}>
          Deny
        </Button>
        <Button onClick={() => decide(true)} loading={pending === "allow"} disabled={pending !== null}>
          Allow
        </Button>
      </div>
      <p className="mt-4 text-center text-xs text-muted-foreground">You can disconnect it any time under API keys.</p>
    </Card>
  );
}
