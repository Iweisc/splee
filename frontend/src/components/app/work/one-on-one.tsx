"use client";

import { useState } from "react";
import { Headset, Loader2, MessagesSquare, RefreshCw } from "lucide-react";
import useSWR from "swr";
import { CopyButton } from "@/components/app/settings/copy-button";
import { Button } from "@/components/ui/button";
import { Badge, EmptyState, Modal } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { OneOnOnePrep, OneOnOneView, PracticeAssignment } from "@/lib/types";
import { dealMoney, timeAgo } from "@/lib/utils";
import { scenarioLabel, tone } from "./practice";
import { MicroLabel, toastError } from "./states";

/** The month the prep was written in, like "$7,200 won of $12,000 on day 14 of 30, $4,800 in commit, $2,000 best case". */
function monthLine(p: OneOnOnePrep) {
  const m = p.month;
  const money = (cents: number) => dealMoney(cents / 100, m.currency.toUpperCase());
  return [
    `${money(m.won_cents)} won${m.target_cents ? ` of ${money(m.target_cents)}` : ""} on day ${m.day} of ${m.days}`,
    `${money(m.commit_cents)} in commit`,
    `${money(m.best_case_cents)} best case`,
  ].join(", ");
}

/** The prep as plain text, for pasting into the one-on-one's notes. */
function asText(name: string, p: OneOnOnePrep) {
  const lines = [`1:1 with ${name}`, "", p.headline, monthLine(p)];
  if (p.recognize.length) lines.push("", "Recognize", ...p.recognize.map((r) => `- ${r}`));
  if (p.deals.length) lines.push("", "Deals to dig into", ...p.deals.flatMap((d) => [`- ${d.company}: ${d.why}`, `  Ask: ${d.ask}`]));
  lines.push("", `Skill to work on: ${p.skill.name}`, p.skill.evidence, `Drill: ${p.skill.drill}`, "", `Commitment: ${p.commitment}`);
  return lines.join("\n");
}

/** How the drill they were assigned is going: waiting on their Today, started, scored, or the deal closed first. */
function Assignment({ a, first }: { a: PracticeAssignment; first: string }) {
  return (
    <div className="mt-3 flex items-start gap-2 border-t border-border pt-3">
      {a.score ? <Badge tone={tone(a.score.score)}>{a.score.score}</Badge> : <Badge>{a.session_id ? "Started" : "Assigned"}</Badge>}
      <p className="text-muted-foreground">
        {a.score
          ? `${first} practiced ${scenarioLabel(a.scenario).toLowerCase()} on ${a.company}. ${a.score.skill ?? a.score.summary}`
          : !a.open
            ? `The ${a.company} deal closed before ${first} practiced it.`
            : a.session_id
              ? `${first} started the practice call on ${a.company} but hasn't been scored yet.`
              : `Assigned ${timeAgo(a.created_at)}. It's on ${first}'s Today until they practice it.`}
      </p>
    </div>
  );
}

/** A teammate's 1:1 prep in its own window: the latest, writing a new one, and the deals it names opening their deal. */
export function OneOnOneDialog({
  userId,
  name,
  onClose,
  onOpen,
}: {
  userId: string;
  name: string;
  onClose: () => void;
  onOpen: (dealId: string) => void;
}) {
  const key = `/one-on-ones/${userId}`;
  const { data, mutate } = useSWR<OneOnOneView>(key, fetcher, { refreshInterval: (v) => (v?.latest?.status === "writing" ? 2000 : 0) });
  const { me } = useMe();
  const [busy, setBusy] = useState(false);
  const [assigning, setAssigning] = useState(false);
  const first = name.split(" ")[0];
  const latest = data?.latest;
  const writing = latest?.status === "writing";
  const p = latest?.status === "ready" ? latest.prep : null;

  const write = async () => {
    setBusy(true);
    try {
      await mutate(api<OneOnOneView>(key, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };
  const assign = async () => {
    setAssigning(true);
    try {
      await mutate(api<OneOnOneView>(`${key}/practice`, { method: "POST" }), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setAssigning(false);
    }
  };
  const writeButton = (
    <Button type="button" size="sm" variant={p ? "outline" : "primary"} onClick={write} loading={busy} disabled={writing}>
      <RefreshCw className="size-3.5" /> {latest ? "Write a new one" : "Prep it"}
    </Button>
  );

  return (
    <Modal open onClose={onClose} className="max-w-2xl" title={`1:1 with ${name}`}>
      {!data ? (
        <Loader2 className="mx-auto size-5 animate-spin text-muted-foreground" />
      ) : !p ? (
        <EmptyState
          icon={writing ? <Loader2 className="size-5 animate-spin" /> : <MessagesSquare className="size-5" />}
          title={writing ? `Reading ${first}'s month…` : latest ? "The last prep couldn't be written" : "No prep yet"}
          action={!writing && writeButton}
        >
          {writing
            ? null
            : latest?.error ||
              `The agent reads ${first}'s month against their target, their open deals, what they won and lost, their coached calls and practice calls, and writes what to bring to your one-on-one.`}
        </EmptyState>
      ) : (
        <div className="flex flex-col gap-5">
          <div>
            <p className="text-sm text-pretty">{p.headline}</p>
            <p className="mt-1 text-xs text-muted-foreground tabular-nums">{monthLine(p)}</p>
          </div>
          {p.recognize.length > 0 && (
            <div>
              <MicroLabel>Recognize</MicroLabel>
              <ul className="mt-1 flex list-disc flex-col gap-1 pl-5 text-sm text-pretty">
                {p.recognize.map((r) => (
                  <li key={r}>{r}</li>
                ))}
              </ul>
            </div>
          )}
          {p.deals.length > 0 && (
            <div>
              <MicroLabel>Deals to dig into</MicroLabel>
              <ul className="mt-1 flex flex-col gap-2">
                {p.deals.map((d) => (
                  <li key={d.deal_id} className="rounded-xl border border-border p-3 text-sm text-pretty">
                    <button
                      type="button"
                      className="font-medium hover:underline"
                      onClick={() => {
                        onClose();
                        onOpen(d.deal_id);
                      }}
                    >
                      {d.company}
                    </button>
                    <p className="text-muted-foreground">{d.why}</p>
                    <p className="mt-1">
                      <span className="font-medium">Ask:</span> {d.ask}
                    </p>
                  </li>
                ))}
              </ul>
            </div>
          )}
          <div className="rounded-xl bg-muted/60 p-3 text-sm text-pretty">
            <MicroLabel>Skill to work on</MicroLabel>
            <p className="mt-1 font-medium">{p.skill.name}</p>
            <p className="text-muted-foreground">{p.skill.evidence}</p>
            <p className="mt-1">
              <span className="font-medium">Drill:</span> {p.skill.drill}
            </p>
            {p.skill.practice &&
              (data.assignment ? (
                <Assignment a={data.assignment} first={first} />
              ) : (
                <Button type="button" size="sm" variant="outline" className="mt-3" onClick={assign} loading={assigning}>
                  <Headset className="size-3.5" /> {me?.user.id === userId ? "Put it on my Today" : `Assign it to ${first}`}
                </Button>
              ))}
          </div>
          <div>
            <MicroLabel>Commitment for the week</MicroLabel>
            <p className="mt-1 text-sm text-pretty">{p.commitment}</p>
          </div>
          <div className="flex flex-wrap items-center justify-end gap-2 border-t border-border pt-4">
            <p className="mr-auto text-xs text-muted-foreground">
              {[latest?.requested_by && `Asked for by ${latest.requested_by}`, latest && timeAgo(latest.created_at)].filter(Boolean).join(" · ")}
            </p>
            <CopyButton value={asText(name, p)} label="Copy" />
            {writeButton}
          </div>
        </div>
      )}
    </Modal>
  );
}
