"use client";

import { useDeferredValue, useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { Download, Search, ShieldBan, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { LoadError } from "@/components/app/work/states";
import { useAction } from "@/components/app/settings/use-action";
import { Button, buttonClass } from "@/components/ui/button";
import { Input, Textarea } from "@/components/ui/form";
import { Badge, Card, EmptyState, Skeleton, type BadgeTone } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { Paginated, Suppression } from "@/lib/types";
import { num, timeAgo } from "@/lib/utils";

const PER_PAGE = 50;

const REASONS: Record<Suppression["reason"], { label: string; tone: BadgeTone }> = {
  manual: { label: "Added by you", tone: "neutral" },
  customer: { label: "Customer", tone: "blue" },
  unsubscribe: { label: "Unsubscribed", tone: "amber" },
  bounce: { label: "Bounced", tone: "red" },
};

export default function DoNotContactPage() {
  const [values, setValues] = useState("");
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(1);
  const q = useDeferredValue(query.trim());
  const key = `/suppressions${qs({ q, page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<Suppression>>(key, fetcher, { keepPreviousData: true });
  const { pending, run } = useAction();

  const pasted = values.split(/[\s,;]+/).filter(Boolean).length;
  const pages = data ? Math.max(1, Math.ceil(data.total / PER_PAGE)) : 1;

  const add = () =>
    run("add", async () => {
      const { added } = await api<{ added: number }>("/suppressions", { body: { values } });
      await mutate();
      setValues("");
      toast.success(`${num(added)} added${added < pasted ? `, ${num(pasted - added)} skipped as duplicates or invalid` : ""}`);
    });

  const remove = (s: Suppression) =>
    run(s.id, async () => {
      await api(`/suppressions/${s.id}`, { method: "DELETE" });
      await mutate();
    });

  return (
    <>
      <PageHeader
        title="Do-not-contact"
        description="The agent never emails anyone on this list. Unsubscribes and bounces land here automatically."
      />
      <PageBody>
        <SettingsColumn>
          <Section
            title="Add emails or domains"
            description="One per line or comma separated. A domain like acme.com blocks everyone at that company, useful for existing customers and partners."
          >
            <Textarea
              value={values}
              onChange={(e) => setValues(e.target.value)}
              placeholder={"jane@acme.com\nglobex.com"}
              rows={4}
              className="font-mono text-[13px]"
            />
            <div className="mt-3 flex items-center justify-between">
              <span className="text-xs text-muted-foreground">{pasted > 0 && `${num(pasted)} entries`}</span>
              <Button onClick={add} disabled={!pasted} loading={pending === "add"}>
                Add to list
              </Button>
            </div>
          </Section>

          <Card>
            <div className="flex items-center justify-between gap-4 border-b border-border p-4">
              <div className="relative max-w-xs flex-1">
                <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
                <Input
                  value={query}
                  onChange={(e) => {
                    setQuery(e.target.value);
                    setPage(1);
                  }}
                  placeholder="Search"
                  className="h-9 pl-9"
                  aria-label="Search the list"
                />
              </div>
              {data && (
                <div className="flex shrink-0 items-center gap-3">
                  <span className="text-sm text-muted-foreground tabular-nums">{num(data.total)} total</span>
                  {data.total > 0 && (
                    // A plain link so the browser streams the file with the session cookie. It exports every match, not just this page.
                    <a href={`/api/suppressions/export${qs({ q })}`} download className={buttonClass("outline", "sm", "h-9")}>
                      <Download className="size-3.5" /> Export
                    </a>
                  )}
                </div>
              )}
            </div>

            {error ? (
              <LoadError error={error} what="the list" retry={() => mutate()} />
            ) : !data ? (
              <div className="space-y-3 p-4">
                {Array.from({ length: 4 }, (_, i) => (
                  <Skeleton key={i} className="h-6 w-full" />
                ))}
              </div>
            ) : data.items.length === 0 ? (
              <EmptyState icon={<ShieldBan className="size-6" />} title={q ? "No matches" : "The list is empty"}>
                {q ? `Nothing matches "${q}".` : "Add customers and partners above so the agent never reaches out to them."}
              </EmptyState>
            ) : (
              <ul className="divide-y divide-border">
                {data.items.map((s) => (
                  <li key={s.id} className="flex items-center gap-3 px-4 py-2.5 text-sm">
                    <span className="min-w-0 flex-1 truncate font-mono text-[13px]">{s.value}</span>
                    <span className="hidden text-xs text-muted-foreground sm:inline">{s.kind === "domain" ? "Domain" : "Email"}</span>
                    <Badge tone={REASONS[s.reason].tone}>{REASONS[s.reason].label}</Badge>
                    <span className="hidden w-24 text-right text-xs text-muted-foreground sm:inline">{timeAgo(s.created_at)}</span>
                    <Button variant="ghost" size="icon" className="size-8" onClick={() => remove(s)} loading={pending === s.id} aria-label={`Remove ${s.value}`}>
                      {pending !== s.id && <Trash2 className="size-4" />}
                    </Button>
                  </li>
                ))}
              </ul>
            )}

            {data && pages > 1 && (
              <div className="flex items-center justify-between border-t border-border px-4 py-3 text-sm">
                <span className="text-muted-foreground tabular-nums">
                  Page {page} of {pages}
                </span>
                <div className="flex gap-2">
                  <Button variant="outline" size="sm" onClick={() => setPage(page - 1)} disabled={page <= 1}>
                    Previous
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => setPage(page + 1)} disabled={page >= pages}>
                    Next
                  </Button>
                </div>
              </div>
            )}
          </Card>
        </SettingsColumn>
      </PageBody>
    </>
  );
}
