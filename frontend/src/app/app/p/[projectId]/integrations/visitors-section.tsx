"use client";

import { useState } from "react";
import { Eye } from "lucide-react";
import useSWR from "swr";
import { CodeBlock } from "@/components/app/settings/code-block";
import { ChipInput } from "@/components/app/settings/chip-input";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import { useOrigin } from "@/hooks/use-origin";
import { api } from "@/lib/api";
import type { VisitorSite } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

/** The visitors snippet, where page views may come from, and the pages that alert. Saved on its own, not with the page. */
export function VisitorsSection({ projectId }: { projectId: string }) {
  const origin = useOrigin();
  const key = `/projects/${projectId}/visitors/site`;
  const { data, error, mutate } = useSWR<VisitorSite>(key, fetcher);
  const { pending, run } = useAction();
  const [draft, setDraft] = useState<{ domains: string[]; intent_paths: string[] } | null>(null);
  const [checked, setChecked] = useState(false);
  const form = draft ?? { domains: data?.domains ?? [], intent_paths: data?.intent_paths ?? [] };
  const save = () =>
    run("save", async () => {
      await mutate(await api<VisitorSite>(key, { method: "PUT", body: form }), { revalidate: false });
      setDraft(null);
    }, "Saved");
  const check = () =>
    run("check", async () => {
      await mutate();
      setChecked(true);
    });
  const last = data?.last_hit_at
    ? `The last page view came ${timeAgo(data.last_hit_at)}${data.last_hit_path ? `, on ${data.last_hit_path}` : ""}.`
    : checked
      ? "No page views yet. Open your site in a new tab, then check again."
      : "No page views yet.";
  return (
    <div id="visitors" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <Eye className="size-4" /> Website visitors
          </span>
        }
        description="Shows which companies visit your site and the pages they read, on the Companies page."
      >
        {error ? (
          <LoadError error={error} what="the snippet" retry={() => mutate()} />
        ) : !data ? null : (
          <div className="flex flex-col gap-4">
            <div className="flex flex-col gap-1.5">
              <CodeBlock code={`<script async src="${origin}/api/v/${data.site_key}.js"></script>`} label="Add to every page" wrap />
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
                <Button size="sm" variant="outline" onClick={check} loading={pending === "check"}>
                  Check install
                </Button>
                <p className="text-sm text-pretty text-muted-foreground" role="status">
                  {last}
                </p>
              </div>
            </div>
            <Field label="Your site's domains" hint={<span className="text-pretty">Page views from anywhere else are ignored. Subdomains count too.</span>} group>
              <ChipInput value={form.domains} onChange={(domains) => setDraft({ ...form, domains })} placeholder="example.com" />
            </Field>
            <Field
              label="High-intent pages"
              hint={
                <span className="text-pretty">
                  Alerts your chats when a company in a campaign, or one that read 3 pages today, opens one, once a day per company.
                </span>
              }
              group
            >
              <ChipInput value={form.intent_paths} onChange={(intent_paths) => setDraft({ ...form, intent_paths })} placeholder="/pricing, /demo" />
            </Field>
            <p className="text-xs text-pretty text-muted-foreground">
              The snippet sets no cookies and sends nothing when the browser asks not to be tracked. Splee never stores a visitor&apos;s IP
              address, only a keyed hash of it, and deletes page views after 180 days.
            </p>
            <Button onClick={save} loading={pending === "save"} disabled={!draft} className="sm:self-start">
              Save
            </Button>
          </div>
        )}
      </Section>
    </div>
  );
}
