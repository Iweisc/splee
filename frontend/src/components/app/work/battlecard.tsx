"use client";

import { useState } from "react";
import Link from "next/link";
import { ChevronDown, Loader2, PenLine, Swords } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Battlecard, BattlecardItem, Deal } from "@/lib/types";
import { cn } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

export const competitorName = (i: BattlecardItem) => i.name || i.domain;
export const writing = (items?: BattlecardItem[]) => !!items?.some((i) => i.status === "writing");

/** Asks for a competitor's card now and refreshes every list that shows it. */
export function useWriteBattlecard() {
  const { mutate } = useSWRConfig();
  const [busy, setBusy] = useState<string | null>(null);
  const write = async (competitorId: string) => {
    setBusy(competitorId);
    try {
      await api(`/competitors/${competitorId}/battlecard`, { method: "POST" });
      await mutate((k) => typeof k === "string" && k.includes("/battlecards"));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  return { busy, write };
}

function List({ title, items, className }: { title: string; items: React.ReactNode[]; className?: string }) {
  if (items.length === 0) return null;
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <MicroLabel>{title}</MicroLabel>
      <ul className="flex flex-col gap-1.5 text-sm">
        {items.map((item, i) => (
          <li key={i} className="text-pretty">
            {item}
          </li>
        ))}
      </ul>
    </div>
  );
}

const point = (p: { point: string; evidence: string }) => (
  <>
    {p.point} <span className="text-muted-foreground">{p.evidence}</span>
  </>
);

function Objections({ card }: { card: Battlecard }) {
  return (
    <List
      title="When they say"
      items={card.objections.map((o) => (
        <>
          <span className="font-medium">&ldquo;{o.they_say}&rdquo;</span>
          <span className="mt-0.5 block text-muted-foreground">{o.we_say}</span>
        </>
      ))}
    />
  );
}

/** The whole card, for the Competitors page. */
export function BattlecardBody({ card }: { card: Battlecard }) {
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-pretty">{card.summary}</p>
      {card.pricing && (
        <p className="text-sm text-pretty">
          <span className="text-muted-foreground">Pricing: </span>
          {card.pricing}
        </p>
      )}
      <div className="grid gap-4 sm:grid-cols-2">
        <List title="Where you win" items={card.wins.map(point)} />
        <List title="Where you lose" items={card.losses.map(point)} />
      </div>
      <List title="What they do well" items={card.strengths} />
      <List title="Ask the buyer" items={card.landmines} />
      <Objections card={card} />
      <List
        title="What buyers said"
        items={card.quotes.map((q) => (
          <>
            &ldquo;{q.quote}&rdquo; <span className="text-muted-foreground">{q.who}</span>
          </>
        ))}
      />
    </div>
  );
}

/** One competitor the deal is up against: what to ask and how to answer, with the rest a click away. */
function UpAgainst({ item, projectId }: { item: BattlecardItem; projectId: string }) {
  const { busy, write } = useWriteBattlecard();
  const [open, setOpen] = useState(false);
  const card = item.card;
  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <Swords className="size-3.5" /> Up against {competitorName(item)}
          </p>
          {card ? (
            <p className="mt-1 text-sm text-pretty text-muted-foreground">{card.summary}</p>
          ) : item.status === "writing" ? (
            <p className="mt-1 text-sm text-muted-foreground">
              The agent is reading {competitorName(item)}&apos;s website and your deals against them…
            </p>
          ) : item.status === "failed" ? (
            <p className="mt-1 text-sm text-danger">The agent couldn&apos;t finish the battlecard. Try again.</p>
          ) : (
            <p className="mt-1 text-sm text-muted-foreground">This deal mentions them. The agent can write how to sell against them.</p>
          )}
        </div>
        {item.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          !card && (
            <Button type="button" size="sm" variant="outline" onClick={() => write(item.competitor_id)} loading={busy === item.competitor_id}>
              <PenLine className="size-3.5" /> Write a battlecard
            </Button>
          )
        )}
      </div>
      {card && (
        <>
          <List title="Ask the buyer" items={card.landmines} />
          {open && <Objections card={card} />}
          <div className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs">
            {card.objections.length > 0 && (
              <button
                type="button"
                onClick={() => setOpen(!open)}
                aria-expanded={open}
                className="inline-flex items-center gap-1 text-muted-foreground hover:text-foreground"
              >
                <ChevronDown className={cn("size-3.5 transition-transform", open && "rotate-180")} />
                {open ? "Hide answers" : `Answers to what they say for ${competitorName(item)}`}
              </button>
            )}
            <Link href={`/app/p/${projectId}/competitors?open=${item.competitor_id}`} className="text-muted-foreground hover:text-foreground">
              The whole battlecard
            </Link>
          </div>
        </>
      )}
    </section>
  );
}

/** The competitors a deal is up against, for its window. Nothing when it mentions none. */
export function DealBattlecards({ deal }: { deal: Deal }) {
  const { data } = useSWR<BattlecardItem[]>(`/deals/${deal.id}/battlecards`, fetcher, {
    refreshInterval: (items) => (writing(items) ? 3000 : 0),
  });
  if (!data?.length) return null;
  return (
    <>
      {data.map((item) => (
        <UpAgainst key={item.competitor_id} item={item} projectId={deal.project_id} />
      ))}
    </>
  );
}
