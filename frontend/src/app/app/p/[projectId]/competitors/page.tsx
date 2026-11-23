"use client";

import { use, useState } from "react";
import useSWR from "swr";
import { ChevronDown, ExternalLink, Loader2, PenLine, Plus, Swords, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { BattlecardBody, useWriteBattlecard, writing } from "@/components/app/work/battlecard";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Card, EmptyState, Favicon } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { BattlecardItem, BattlecardView, Competitor } from "@/lib/types";
import { cn, plural } from "@/lib/utils";

const EMPTY = { domain: "", name: "", note: "" };

const record = (b: BattlecardItem) =>
  [b.won > 0 && `won ${plural(b.won, "deal")}`, b.lost > 0 && `lost ${plural(b.lost, "deal")}`].filter(Boolean).join(", ");

/** Under a competitor: its record against you, the card when it's open, and writing it. */
function CardRow({ b, open, toggle }: { b?: BattlecardItem; open: boolean; toggle: () => void }) {
  const { busy, write } = useWriteBattlecard();
  if (!b) return null;
  const said = record(b);
  return (
    <div className="flex flex-col gap-3 pl-9">
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 text-sm">
        {said && <span className="text-muted-foreground">You {said} against them.</span>}
        {b.card && (
          <button type="button" onClick={toggle} aria-expanded={open} className="inline-flex items-center gap-1 font-medium hover:underline">
            <ChevronDown className={cn("size-3.5 transition-transform", open && "rotate-180")} />
            {open ? "Hide the battlecard" : "Battlecard"}
          </button>
        )}
        {b.status === "writing" ? (
          <span className="inline-flex items-center gap-1.5 text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" /> The agent is reading their website and your deals against them…
          </span>
        ) : (
          <Button type="button" size="sm" variant="ghost" onClick={() => write(b.competitor_id)} loading={busy === b.competitor_id}>
            <PenLine className="size-3.5" /> {b.card ? "Write again" : "Write a battlecard"}
          </Button>
        )}
        {b.status === "failed" && <span className="text-danger">The agent couldn&apos;t finish it. Try again.</span>}
      </div>
      {open && b.card && (
        <div className="rounded-xl border border-border p-4">
          <BattlecardBody card={b.card} />
        </div>
      )}
    </div>
  );
}

export default function CompetitorsPage({ params, searchParams }: PageProps<"/app/p/[projectId]/competitors">) {
  const { projectId } = use(params);
  const { open: linked } = use(searchParams);
  const key = `/projects/${projectId}/competitors`;
  const { data, error, mutate } = useSWR<Competitor[]>(key, fetcher);
  const { pending, run } = useAction();
  const [draft, setDraft] = useState(EMPTY);
  const cards = useSWR<BattlecardView>(`/projects/${projectId}/battlecards`, fetcher, {
    refreshInterval: (v) => (writing(v?.items) ? 3000 : 0),
  });
  // A deal's "The whole battlecard" link opens its card here.
  const [open, setOpen] = useState<string | null>(typeof linked === "string" ? linked : null);
  const card = (c: Competitor) => cards.data?.items.find((b) => b.competitor_id === c.id);

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
        description="Companies that sell what you sell. The agent reads this list when it plans campaigns, and writes a battlecard for each one you win or lose a deal against."
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
                <div key={c.id} className="flex flex-col gap-2 px-5 py-4">
                  <div className="flex items-center gap-4">
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
                  <CardRow b={card(c)} open={open === c.id} toggle={() => setOpen(open === c.id ? null : c.id)} />
                </div>
              ))}
            </Card>
          )}

          {/* Waits for the list: drawn under the skeleton, it jumped down by the list's height when that loaded. */}
          {data && (
            <Section title="Add a competitor" description="Running the research again keeps the ones you add here, and any with a battlecard.">
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
