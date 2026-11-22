"use client";

import { use, useState } from "react";
import Link from "next/link";
import { ChevronDown, GraduationCap, Loader2, Phone, Video } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LeadModal } from "@/components/app/work/lead-modal";
import { QUALIFY_LABELS } from "@/components/app/work/qualification";
import { LoadError, MicroLabel } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Avatar, Badge, type BadgeTone, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { CoachedCall, CoachingView } from "@/lib/types";
import { cn, dayTime, plural } from "@/lib/utils";

type Coached = CoachedCall & { coaching: NonNullable<CoachedCall["coaching"]> };

const ANYONE = "";
const NOBODY = "none";

const tone = (score: number): BadgeTone => (score >= 8 ? "green" : score >= 5 ? "amber" : "red");
const average = (ns: number[]) => (ns.length ? ns.reduce((a, b) => a + b, 0) / ns.length : null);

/** Averages over the coached calls. Talk share counts only calls whose speakers could be told apart. */
function stats(calls: Coached[]) {
  const shares = calls.flatMap((c) => (c.talk_share == null ? [] : [c.talk_share]));
  return {
    score: average(calls.map((c) => c.coaching.score)),
    share: average(shares),
    questions: average(calls.map((c) => c.questions)),
    nextStep: calls.length ? calls.filter((c) => c.coaching.next_step_agreed).length / calls.length : null,
  };
}

const fixed = (n: number | null, digits = 1) => (n == null ? "–" : n.toFixed(digits));
const pct = (n: number | null) => (n == null ? "–" : `${Math.round(n)}%`);

export default function CoachingPage({ params }: PageProps<"/app/p/[projectId]/coaching">) {
  const { projectId } = use(params);
  const { data, error, mutate } = useSWR<CoachingView>(`/projects/${projectId}/coaching`, fetcher, {
    refreshInterval: (v) => (v?.items.some((i) => i.status === "writing") ? 5000 : 0),
  });
  const [rep, setRep] = useState(ANYONE);
  const [leadId, setLeadId] = useState<string | null>(null);

  const coached = (data?.items ?? []).filter((i): i is Coached => i.status === "ready" && !!i.coaching);
  const writing = (data?.items ?? []).filter((i) => i.status === "writing").length;
  const failed = (data?.items ?? []).filter((i) => i.status === "failed").length;
  const failedNote = failed > 0 && `The agent couldn't coach ${plural(failed, "call")}.`;
  const reps = new Map<string, { name: string; calls: Coached[] }>();
  for (const c of coached) {
    const key = c.owner_id ?? NOBODY;
    const r = reps.get(key) ?? { name: c.owner_name ?? (c.owner_id ? "Someone" : "No owner"), calls: [] };
    r.calls.push(c);
    reps.set(key, r);
  }
  const shown = rep === ANYONE ? coached : coached.filter((c) => (c.owner_id ?? NOBODY) === rep);
  const all = stats(shown);

  return (
    <>
      <PageHeader
        title="Coaching"
        description="Every sales meeting and call, scored once it's summarized, with what went well and what to say next time."
        actions={
          reps.size > 1 && (
            <Select value={rep} onChange={(e) => setRep(e.target.value)} className="w-auto flex-auto" aria-label="Rep">
              <option value={ANYONE}>Everyone</option>
              {[...reps].map(([id, r]) => (
                <option key={id} value={id}>
                  {r.name}
                </option>
              ))}
            </Select>
          )
        }
      />
      <PageBody>
        {data && !data.enabled && (
          <p className="mb-4 rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
            Coaching is off, so new calls aren&apos;t scored. Turn on Coach calls on the{" "}
            <Link href={`/app/p/${projectId}/autopilot`} className="font-medium underline underline-offset-2">
              Autopilot page
            </Link>
            .
          </p>
        )}
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <div className="flex flex-col gap-3">
            <Skeleton className="h-24 rounded-2xl" />
            <Skeleton className="h-64 rounded-2xl" />
          </div>
        ) : coached.length === 0 ? (
          <Card>
            <EmptyState
              icon={writing ? <Loader2 className="size-5 animate-spin" /> : <GraduationCap className="size-5" />}
              title={writing ? "Coaching your calls" : failed ? "Coaching didn't finish" : "No calls coached yet"}
              action={
                !writing &&
                !failedNote && (
                  <LinkButton href={`/app/p/${projectId}/integrations#meeting-notes`} variant="outline" size="sm">
                    Open Integrations
                  </LinkButton>
                )
              }
            >
              {writing
                ? `The agent is listening to ${plural(writing, "call")}. Scores show up here in a minute.`
                : failedNote
                  ? `${failedNote} New calls are coached once they're summarized.`
                  : "Connect a call recorder like Fireflies, Fathom, Gong, Zoom or tl;dv on the Integrations page, or turn on transcription for calls made from Splee. Each one is coached after it's summarized."}
            </EmptyState>
          </Card>
        ) : (
          <div className="flex flex-col gap-6">
            <div className="grid grid-cols-2 gap-3 lg:grid-cols-4">
              <Kpi label="Average score" value={fixed(all.score)} hint={`out of 10, over ${plural(shown.length, "call")}`} />
              <Kpi label="Our talk share" value={pct(all.share)} hint="of the words, by your side" />
              <Kpi label="Questions asked" value={fixed(all.questions)} hint="per call, by your side" />
              <Kpi label="Next step agreed" value={pct(all.nextStep == null ? null : all.nextStep * 100)} hint="of calls ended with one" />
            </div>

            {rep === ANYONE && reps.size > 1 && (
              <Card className="overflow-x-auto">
                <table className="w-full text-sm">
                  <thead>
                    <tr className="text-left text-xs text-muted-foreground">
                      <th className="px-4 py-3 font-medium">Rep</th>
                      <th className="px-4 py-3 text-right font-medium">Calls</th>
                      <th className="px-4 py-3 text-right font-medium">Score</th>
                      <th className="px-4 py-3 text-right font-medium">Talk share</th>
                      <th className="px-4 py-3 text-right font-medium">Questions</th>
                      <th className="px-4 py-3 text-right font-medium">Next step</th>
                    </tr>
                  </thead>
                  <tbody className="divide-y divide-border">
                    {[...reps].map(([id, r]) => {
                      const s = stats(r.calls);
                      return (
                        <tr key={id} className="cursor-pointer hover:bg-muted/50" onClick={() => setRep(id)}>
                          <td className="px-4 py-2.5">
                            <span className="flex items-center gap-2 font-medium">
                              <Avatar name={r.name} className="size-6 text-[10px]" /> {r.name}
                            </span>
                          </td>
                          <td className="px-4 py-2.5 text-right tabular-nums">{r.calls.length}</td>
                          <td className="px-4 py-2.5 text-right">{s.score != null && <Badge tone={tone(s.score)}>{fixed(s.score)}</Badge>}</td>
                          <td className="px-4 py-2.5 text-right tabular-nums">{pct(s.share)}</td>
                          <td className="px-4 py-2.5 text-right tabular-nums">{fixed(s.questions)}</td>
                          <td className="px-4 py-2.5 text-right tabular-nums">{pct(s.nextStep == null ? null : s.nextStep * 100)}</td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </Card>
            )}

            <div className="flex flex-col gap-3">
              {writing > 0 && (
                <p className="flex items-center gap-2 text-sm text-muted-foreground">
                  <Loader2 className="size-3.5 animate-spin" /> Coaching {plural(writing, "more call")}…
                </p>
              )}
              {failedNote && <p className="text-sm text-muted-foreground">{failedNote}</p>}
              {shown.map((c) => (
                <CallRow key={c.id} call={c} openLead={() => setLeadId(c.lead_id)} />
              ))}
            </div>
          </div>
        )}
      </PageBody>
      <LeadModal leadId={leadId} onClose={() => setLeadId(null)} />
    </>
  );
}

function Kpi({ label, value, hint }: { label: string; value: string; hint: string }) {
  return (
    <Card className="p-4">
      <MicroLabel>{label}</MicroLabel>
      <p className="mt-1 text-2xl font-semibold tabular-nums">{value}</p>
      <p className="text-xs text-muted-foreground">{hint}</p>
    </Card>
  );
}

function CallRow({ call: c, openLead }: { call: Coached; openLead: () => void }) {
  const [open, setOpen] = useState(false);
  const k = c.coaching;
  const Icon = c.kind === "meeting" ? Video : Phone;
  const minutes = c.duration_secs ? `${Math.max(1, Math.round(c.duration_secs / 60))} min` : null;
  return (
    <Card>
      <button type="button" onClick={() => setOpen(!open)} aria-expanded={open} className="flex w-full items-center gap-3 p-4 text-left">
        <Badge tone={tone(k.score)} className="w-9 justify-center tabular-nums">
          {k.score}
        </Badge>
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-medium">
            {c.lead_name}
            {c.company_name && <span className="font-normal text-muted-foreground"> · {c.company_name}</span>}
          </p>
          <p className="flex items-center gap-1.5 truncate text-xs text-muted-foreground">
            <Icon className="size-3 shrink-0" />
            {[c.title || (c.kind === "meeting" ? "Meeting" : "Call"), dayTime(c.happened_at), minutes, c.owner_name].filter(Boolean).join(" · ")}
          </p>
        </div>
        <TalkBar share={c.talk_share} />
        <span className="hidden w-24 shrink-0 text-right text-xs text-muted-foreground tabular-nums sm:block">{plural(c.questions, "question")}</span>
        <ChevronDown className={cn("size-4 shrink-0 text-muted-foreground transition-transform", open && "rotate-180")} />
      </button>
      {open && (
        <div className="grid gap-5 border-t border-border p-4 lg:grid-cols-[minmax(0,1fr)_16rem]">
          <div className="flex flex-col gap-4">
            {k.improve.length > 0 && (
              <div>
                <MicroLabel className="mb-2">Try next time</MicroLabel>
                <ul className="flex flex-col gap-3">
                  {k.improve.map((i) => (
                    <li key={i.moment} className="text-sm">
                      <p className="font-medium">{i.moment}</p>
                      {i.quote && (
                        <blockquote className="mt-1 border-l-2 border-border pl-3 text-muted-foreground italic">&ldquo;{i.quote}&rdquo;</blockquote>
                      )}
                      <p className="mt-1 text-pretty">
                        <span className="text-brand-700">Instead: </span>
                        {i.instead}
                      </p>
                    </li>
                  ))}
                </ul>
              </div>
            )}
            {k.strengths.length > 0 && (
              <div>
                <MicroLabel className="mb-2">Keep doing</MicroLabel>
                <ul className="flex list-disc flex-col gap-1 pl-5 text-sm text-pretty">
                  {k.strengths.map((s) => (
                    <li key={s}>{s}</li>
                  ))}
                </ul>
              </div>
            )}
          </div>
          <div className="flex flex-col gap-4">
            <div>
              <MicroLabel className="mb-1">Next step</MicroLabel>
              <p className={cn("text-sm text-pretty", !k.next_step_agreed && "text-red-700")}>{k.next_step_agreed ? k.next_step : "None agreed"}</p>
            </div>
            <div>
              <MicroLabel className="mb-1.5">Got into</MicroLabel>
              <div className="flex flex-wrap gap-1">
                {Object.entries(QUALIFY_LABELS).map(([key, label]) => (
                  <Badge key={key} tone={k.covered.includes(key) ? "green" : "neutral"} className={cn(!k.covered.includes(key) && "opacity-50")}>
                    {label}
                  </Badge>
                ))}
              </div>
            </div>
            <Button type="button" size="sm" variant="outline" onClick={openLead} className="self-start">
              Open {c.lead_name.split(" ")[0] || "lead"}
            </Button>
          </div>
        </div>
      )}
    </Card>
  );
}

/** Our share of the words beside theirs. Past 60% the rep did most of the talking. */
function TalkBar({ share }: { share: number | null }) {
  if (share == null) return <span className="hidden w-28 shrink-0 sm:block" />;
  return (
    <span className="hidden w-28 shrink-0 items-center gap-2 sm:flex" title={`We spoke ${share}% of the words`}>
      <span className="h-1.5 flex-1 overflow-hidden rounded-full bg-sky-200" aria-hidden>
        <span className={cn("block h-full rounded-full", share > 60 ? "bg-amber-500" : "bg-brand-500")} style={{ width: `${share}%` }} />
      </span>
      <span className="w-8 text-right text-xs text-muted-foreground tabular-nums">{share}%</span>
    </span>
  );
}
