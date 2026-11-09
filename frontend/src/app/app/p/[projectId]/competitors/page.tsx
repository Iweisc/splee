"use client";

import { useParams } from "next/navigation";
import { useState } from "react";
import useSWR from "swr";
import { ExternalLink, Plus, Swords, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card, EmptyState, Favicon } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Competitor } from "@/lib/types";

const EMPTY = { domain: "", name: "", note: "" };

export default function CompetitorsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const key = `/projects/${projectId}/competitors`;
  const { data, error, mutate } = useSWR<Competitor[]>(key, fetcher);
  const { pending, run } = useAction();
  const [draft, setDraft] = useState(EMPTY);

  const remove = (c: Competitor) =>
    run(c.id, async () => {
      await api(`/competitors/${c.id}`, { method: "DELETE" });
      await mutate();
    }, `Removed ${c.name || c.domain}`);

  const add = async () => {
    const ok = await run("add", async () => {
      await api(key, { body: draft });
      await mutate();
    }, "Competitor added");
    if (ok) setDraft(EMPTY);
  };

  return (
    <>
      <PageHeader
        title="Competitors"
        description="Companies that sell what you sell. The agent reads this list when it plans campaigns. Running the research again replaces it."
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="competitors" retry={() => mutate()} />
          ) : !data ? (
            <SectionsSkeleton />
          ) : data.length === 0 ? (
            <Card>
              <EmptyState icon={<Swords className="size-6" />} title="No competitors yet">
                The research run fills this in. You can also add the ones you know below.
              </EmptyState>
            </Card>
          ) : (
            <Card className="divide-y divide-border">
              {data.map((c) => (
                <div key={c.id} className="flex items-center gap-4 px-5 py-4">
                  <Favicon domain={c.domain} size={20} />
                  <div className="min-w-0 flex-1">
                    {/* The domain drops under the name when both don't fit, so the name isn't cut off first. */}
                    <div className="flex flex-wrap items-center gap-x-2">
                      <p className="min-w-0 truncate font-medium">{c.name || c.domain}</p>
                      <a
                        href={`https://${c.domain}`}
                        target="_blank"
                        rel="noreferrer"
                        className="inline-flex shrink-0 items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
                      >
                        {c.domain} <ExternalLink className="size-3" />
                      </a>
                    </div>
                    {c.note && <p className="mt-0.5 text-sm text-muted-foreground">{c.note}</p>}
                  </div>
                  <Button variant="ghost" size="icon" onClick={() => remove(c)} loading={pending === c.id} aria-label={`Remove ${c.domain}`}>
                    <Trash2 className="size-4" />
                  </Button>
                </div>
              ))}
            </Card>
          )}

          {/* Waits for the list: drawn under the skeleton, it jumped down by the list's height when that loaded. */}
          {data && (
            <Section title="Add a competitor" description="One the research missed.">
              <form
                className="grid gap-4 sm:grid-cols-[1fr_1fr]"
                onSubmit={(e) => {
                  e.preventDefault();
                  if (draft.domain.trim()) add();
                }}
              >
                <Field label="Website">
                  <Input value={draft.domain} onChange={(e) => setDraft({ ...draft, domain: e.target.value })} placeholder="acme.com" />
                </Field>
                <Field label="Name (optional)">
                  <Input value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} placeholder="Acme" />
                </Field>
                <Field label="How it overlaps (optional)" className="sm:col-span-2">
                  <Input value={draft.note} onChange={(e) => setDraft({ ...draft, note: e.target.value })} placeholder="e.g. Same product for enterprises" />
                </Field>
                <div className="flex justify-end sm:col-span-2">
                  <Button type="submit" disabled={!draft.domain.trim()} loading={pending === "add"}>
                    <Plus className="size-4" /> Add
                  </Button>
                </div>
              </form>
            </Section>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}
