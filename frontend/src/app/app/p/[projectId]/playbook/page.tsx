"use client";

import { use, useState } from "react";
import { BookMarked, Loader2, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyButton } from "@/components/app/settings/copy-button";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Playbook, PlaybookSources, PlaybookView } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";

/** What it read, like "2 wins, 1 loss, 1 battlecard and 3 coached calls". */
function readFrom(s: PlaybookSources) {
  const parts = [
    s.profile && "the buyer profile",
    s.won > 0 && plural(s.won, "win"),
    s.lost > 0 && plural(s.lost, "loss", "losses"),
    s.battlecards > 0 && plural(s.battlecards, "battlecard"),
    s.campaigns > 0 && `insights on ${plural(s.campaigns, "campaign")}`,
    s.calls > 0 && plural(s.calls, "coached call"),
    s.emails > 0 && plural(s.emails, "email that got a hot reply", "emails that got a hot reply"),
  ].filter((p): p is string => !!p);
  return parts.length > 1 ? `${parts.slice(0, -1).join(", ")} and ${parts.at(-1)}` : (parts[0] ?? "nothing yet");
}

const total = (s: PlaybookSources) => s.won + s.lost + s.battlecards + s.campaigns + s.calls + s.emails + (s.profile ? 1 : 0);

/** The playbook as markdown, for a wiki or a doc. */
function markdown(p: Playbook) {
  const out = ["# Sales playbook", "", p.summary];
  const section = (title: string, lines: string[]) => lines.length && out.push("", `## ${title}`, "", ...lines);
  section("Who to sell to", [...p.fit.map((f) => `- Good fit: ${f}`), ...p.avoid.map((a) => `- Not a fit: ${a}`)]);
  section("The pitch", p.pitch ? [`> ${p.pitch}`] : []);
  section(
    "Discovery questions",
    p.discovery.map((q, i) => `${i + 1}. ${q.question} (${q.why})`),
  );
  section(
    "Objections",
    p.objections.flatMap((o) => [`- **"${o.they_say}"**`, `  ${o.we_say}`]),
  );
  section(
    "Competitors",
    p.competitors.map((c) => `- **${c.name}**: ${c.win_by}`),
  );
  section(
    "What gets replies",
    p.emails.map((e) => `- ${e.lesson}${e.example ? `, like "${e.example}"` : ""}`),
  );
  section(
    "How deals get won",
    p.winning.map((w) => `- ${w}`),
  );
  section(
    "What loses deals",
    p.pitfalls.map((w) => `- ${w}`),
  );
  return out.join("\n");
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="flex flex-col gap-3 border-t border-border pt-6">
      <h2 className="text-base font-semibold">{title}</h2>
      {children}
    </section>
  );
}

function Bullets({ items }: { items: string[] }) {
  return (
    <ul className="flex list-disc flex-col gap-1.5 pl-5 text-sm text-pretty">
      {items.map((i) => (
        <li key={i}>{i}</li>
      ))}
    </ul>
  );
}

function Document({ p }: { p: Playbook }) {
  return (
    <div className="flex flex-col gap-6">
      <p className="text-lg leading-relaxed text-pretty">{p.summary}</p>
      {(p.fit.length > 0 || p.avoid.length > 0) && (
        <Section title="Who to sell to">
          <div className="grid gap-4 sm:grid-cols-2">
            {p.fit.length > 0 && (
              <div>
                <MicroLabel>Good fit</MicroLabel>
                <Bullets items={p.fit} />
              </div>
            )}
            {p.avoid.length > 0 && (
              <div>
                <MicroLabel>Not a fit</MicroLabel>
                <Bullets items={p.avoid} />
              </div>
            )}
          </div>
        </Section>
      )}
      {p.pitch && (
        <Section title="The pitch">
          <blockquote className="border-l-2 border-brand-600 pl-4 text-base leading-relaxed text-pretty">{p.pitch}</blockquote>
        </Section>
      )}
      {p.discovery.length > 0 && (
        <Section title="Discovery questions">
          <ol className="flex list-decimal flex-col gap-2 pl-5 text-sm text-pretty">
            {p.discovery.map((q) => (
              <li key={q.question}>
                <span className="font-medium">{q.question}</span> <span className="text-muted-foreground">{q.why}</span>
              </li>
            ))}
          </ol>
        </Section>
      )}
      {p.objections.length > 0 && (
        <Section title="Objections">
          <ul className="flex flex-col gap-3">
            {p.objections.map((o) => (
              <li key={o.they_say} className="text-sm text-pretty">
                <p className="text-muted-foreground">&ldquo;{o.they_say}&rdquo;</p>
                <p className="mt-0.5">{o.we_say}</p>
              </li>
            ))}
          </ul>
        </Section>
      )}
      {p.competitors.length > 0 && (
        <Section title="Competitors">
          <div className="grid gap-3 sm:grid-cols-2">
            {p.competitors.map((c) => (
              <div key={c.name} className="rounded-xl border border-border p-3 text-sm text-pretty">
                <p className="font-medium">{c.name}</p>
                <p className="mt-1 text-muted-foreground">{c.win_by}</p>
              </div>
            ))}
          </div>
        </Section>
      )}
      {p.emails.length > 0 && (
        <Section title="What gets replies">
          <ul className="flex flex-col gap-3">
            {p.emails.map((e) => (
              <li key={e.lesson} className="text-sm text-pretty">
                <p className="font-medium">{e.lesson}</p>
                {e.example && <p className="mt-0.5 text-muted-foreground italic">&ldquo;{e.example}&rdquo;</p>}
              </li>
            ))}
          </ul>
        </Section>
      )}
      {(p.winning.length > 0 || p.pitfalls.length > 0) && (
        <Section title="How deals are won and lost">
          <div className="grid gap-4 sm:grid-cols-2">
            {p.winning.length > 0 && (
              <div>
                <MicroLabel>What wins</MicroLabel>
                <Bullets items={p.winning} />
              </div>
            )}
            {p.pitfalls.length > 0 && (
              <div>
                <MicroLabel>What loses</MicroLabel>
                <Bullets items={p.pitfalls} />
              </div>
            )}
          </div>
        </Section>
      )}
    </div>
  );
}

export default function PlaybookPage({ params }: PageProps<"/app/p/[projectId]/playbook">) {
  const { projectId } = use(params);
  const key = `/projects/${projectId}/playbook`;
  const { data, error, mutate } = useSWR<PlaybookView>(key, fetcher, { refreshInterval: (v) => (v?.latest?.status === "writing" ? 3000 : 0) });
  const [busy, setBusy] = useState(false);
  const latest = data?.latest;
  const writing = latest?.status === "writing";
  const p = latest?.status === "ready" ? latest.playbook : null;
  const enough = !!data && total(data.available) >= data.min_sources;

  const write = async () => {
    setBusy(true);
    try {
      await mutate(api<PlaybookView>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const writeButton = (
    <Button type="button" size="sm" variant={p ? "outline" : "primary"} onClick={write} loading={busy} disabled={writing || !enough}>
      {writing ? <Loader2 className="size-3.5 animate-spin" /> : <RefreshCw className="size-3.5" />}{" "}
      {latest ? "Write it again" : "Write the playbook"}
    </Button>
  );

  return (
    <>
      <PageHeader
        title="Playbook"
        description="What your team has learned from its wins, losses, calls and replies, written as the guide a new rep reads first."
        actions={
          p && (
            <>
              <CopyButton value={markdown(p)} label="Copy as markdown" />
              {writeButton}
            </>
          )
        }
      />
      <PageBody>
        {error ? (
          <LoadError error={error} retry={() => mutate()} what="the playbook" />
        ) : !data ? (
          <Skeleton className="h-96 rounded-2xl" />
        ) : !p ? (
          <Card>
            <EmptyState
              icon={writing ? <Loader2 className="size-5 animate-spin" /> : <BookMarked className="size-5" />}
              title={writing ? "The agent is reading what your team learned…" : latest ? "The last playbook couldn't be written" : "No playbook yet"}
              action={!writing && writeButton}
            >
              {writing
                ? null
                : (latest?.error ??
                  (enough
                    ? `The agent reads ${readFrom(data.available)} and writes who to sell to, the pitch, discovery questions, answers to objections, how to beat each competitor, what gets replies, and what wins and loses deals.`
                    : `It needs at least ${data.min_sources} things to learn from: win and loss reviews, battlecards, campaign insights, coached calls or hot replies. So far there's ${readFrom(data.available)}.`))}
            </EmptyState>
          </Card>
        ) : (
          <Card className="mx-auto w-full max-w-3xl p-6 sm:p-8">
            <Document p={p} />
            <p className="mt-8 border-t border-border pt-4 text-xs text-pretty text-muted-foreground">
              Written {timeAgo(latest!.created_at)} from {readFrom(latest!.sources)}.
              {total(data.available) > total(latest!.sources) && " There's more to learn from now, so writing it again will add it."}
            </p>
          </Card>
        )}
      </PageBody>
    </>
  );
}
