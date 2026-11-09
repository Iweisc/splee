"use client";

import { use, useState } from "react";
import { Check, Copy, ExternalLink, MessageCircleReply, SkipForward, Sparkles, UserPlus } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { isActive, useProjectRuns } from "@/components/app/work/run-progress";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState, Skeleton, Spinner } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { LeadRow, LinkedinMessage, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useAgentEvents } from "@/hooks/use-agent-events";

type Sequence = { lead: LeadRow; messages: LinkedinMessage[] };

const isDue = (m: LinkedinMessage) => m.status === "todo" && !!m.due_at && new Date(m.due_at).getTime() <= Date.now();
/** When the step due soonest came due, for sorting the list with the longest-waiting first. */
const dueSince = (s: Sequence) => Math.min(...s.messages.filter(isDue).map((m) => new Date(m.due_at!).getTime()));

export default function LinkedinPage({ params }: PageProps<"/app/p/[projectId]/linkedin-sequence">) {
  const { projectId } = use(params);
  const [segment, setSegment] = useState("");
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const key = `/projects/${projectId}/linkedin${qs({ segment_id: segment })}`;
  const { data, error, mutate } = useSWR<Sequence[]>(key, fetcher, { keepPreviousData: true });
  // Sequences still being written, by lead. Without them, coming back to the page offered Generate again, and each
  // click starts another paid run.
  const { runs, mutate: refreshRuns } = useProjectRuns(projectId);
  const writing = (leadId: string) => runs?.find((r) => r.kind === "linkedin_sequence" && r.input.lead_id === leadId && isActive(r))?.id ?? null;
  const due = (data ?? []).filter((s) => s.messages.some(isDue)).sort((a, b) => dueSince(a) - dueSince(b));
  // Opens on what's due today when there is something, otherwise on every lead. It's chosen once, on the first load, so
  // ticking off the last due step shows "Nothing due right now" instead of swapping in every lead.
  const [picked, setPicked] = useState<"due" | "all" | null>(null);
  if (picked === null && data) setPicked(due.length > 0 ? "due" : "all");
  const view = picked ?? "all";

  return (
    <>
      <PageHeader
        title="LinkedIn"
        description={
          settings?.unipile_connected
            ? "Splee writes the connect note and follow-ups and sends each one from your LinkedIn through Unipile as it comes due, the messages once they accept. Any it can't send wait here for you. A reply on LinkedIn or by email stops both."
            : "Splee writes the connect note and follow-ups and brings each one due a few days after the last. You send them from your own profile and tick them off. A reply on LinkedIn or by email stops both."
        }
        actions={
          <>
            <Select value={view} onChange={(e) => setPicked(e.target.value as "due" | "all")} className="w-auto flex-auto" aria-label="Show">
              <option value="due">Due now ({due.length})</option>
              <option value="all">All leads</option>
            </Select>
            <Select value={segment} onChange={(e) => setSegment(e.target.value)} className="w-auto flex-auto" aria-label="Campaign">
              <option value="">All campaigns</option>
              {segments?.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                </option>
              ))}
            </Select>
          </>
        }
      />
      <PageBody>
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 4 }, (_, i) => (
              <Skeleton key={i} className="h-16 rounded-2xl" />
            ))}
          </div>
        ) : view === "due" && due.length === 0 ? (
          <Card>
            <EmptyState icon={<Check className="size-5" />} title="Nothing due right now">
              Each message comes due a few days after you send the one before it.
            </EmptyState>
          </Card>
        ) : data.length === 0 ? (
          <Card>
            {/* Leads without a LinkedIn profile or a sequence aren't listed, so a project full of leads can still land here. */}
            <EmptyState
              icon={<UserPlus className="size-5" />}
              title="No leads with a LinkedIn profile"
              action={
                <LinkButton href={`/app/p/${projectId}/leads`} variant="outline" size="sm">
                  Go to leads
                </LinkButton>
              }
            >
              Leads show up here once they have a LinkedIn profile. Add one in a lead&apos;s window, or find people in a campaign.
            </EmptyState>
          </Card>
        ) : (
          // One card with rows. A card per lead with gaps between made a 40-lead list several screens tall.
          <Card className="divide-y divide-border">
            {(view === "due" ? due : data).map((s) => (
              <LeadSequence
                key={s.lead.id}
                sequence={s}
                writing={writing(s.lead.id)}
                onChange={() => {
                  mutate();
                  refreshRuns();
                }}
              />
            ))}
          </Card>
        )}
      </PageBody>
    </>
  );
}

function LeadSequence({ sequence: { lead, messages }, writing, onChange }: { sequence: Sequence; writing: string | null; onChange: () => void }) {
  const [started, setStarted] = useState<string | null>(null);
  const runId = started ?? writing;
  const [starting, setStarting] = useState(false);

  useAgentEvents(runId ? `/api/runs/${runId}/events` : null, (e) => {
    if (e.kind === "done" || e.kind === "error") {
      setStarted(null);
      if (e.kind === "error") toast.error(e.message || "Couldn't write the sequence");
      onChange();
    }
  });

  const generate = async () => {
    setStarting(true);
    try {
      const { run_id } = await api<{ run_id: string }>(`/leads/${lead.id}/linkedin`, { method: "POST" });
      setStarted(run_id);
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(false);
    }
  };

  // They answered on LinkedIn: the lead moves to Replied, and the emails and steps still to send stop.
  const [marking, setMarking] = useState(false);
  // They can answer the last message too, so this stays until the lead stops being contacted, like the backend's own rule.
  const open = messages.some((m) => m.status === "todo") || (messages.length > 0 && ["new", "queued", "contacted"].includes(lead.status));
  const markReplied = async () => {
    if (!confirm(`Mark ${lead.full_name || "this lead"} as replied? Their follow-up emails and LinkedIn steps stop.`)) return;
    setMarking(true);
    try {
      await api(`/leads/${lead.id}/linkedin/replied`, { method: "POST" });
      toast.success(`Stopped emailing ${lead.full_name || "them"} and their LinkedIn steps`);
      onChange();
    } catch (e) {
      toastError(e);
    } finally {
      setMarking(false);
    }
  };

  const sorted = [...messages].sort((a, b) => a.step - b.step);
  const doneCount = messages.filter((m) => m.status !== "todo").length;

  return (
    <div className="px-5 py-3.5">
      <div className="flex flex-wrap items-center gap-3">
        <Avatar name={lead.full_name || "?"} />
        <div className="min-w-0 flex-1">
          <p className="font-medium">{lead.full_name}</p>
          <p className="text-sm wrap-anywhere text-muted-foreground">{[lead.title, lead.company_name].filter(Boolean).join(" at ")}</p>
        </div>
        {/* On phones these get their own line under the name, so a long title wraps over the full width and nothing wraps alone. */}
        <div className="flex items-center gap-3 max-sm:w-full max-sm:pl-11">
          {messages.length > 0 && (
            <span className="text-xs text-muted-foreground tabular-nums">
              {doneCount}/{messages.length} done
            </span>
          )}
          {lead.linkedin_url ? (
            <a href={lead.linkedin_url} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 text-sm hover:underline">
              Profile <ExternalLink className="size-3.5" />
            </a>
          ) : (
            <span className="text-sm text-muted-foreground">No LinkedIn URL</span>
          )}
          {open && (
            <Button size="sm" variant="ghost" onClick={markReplied} loading={marking} className="max-sm:ml-auto">
              <MessageCircleReply className="size-3.5" /> They replied
            </Button>
          )}
          {runId ? (
            <span className="inline-flex items-center gap-2 text-sm text-muted-foreground">
              <Spinner /> Writing…
            </span>
          ) : (
            messages.length === 0 && (
              <Button size="sm" variant="outline" onClick={generate} loading={starting} className="max-sm:ml-auto">
                <Sparkles className="size-3.5" /> Generate sequence
              </Button>
            )
          )}
        </div>
      </div>
      {sorted.length > 0 && (
        <ol className="mt-4 grid gap-3 lg:grid-cols-3">
          {sorted.map((m, i) => (
            <MessageCard key={m.id} message={m} before={sorted[i - 1]} onChange={onChange} />
          ))}
        </ol>
      )}
    </div>
  );
}

const stepName = (m: LinkedinMessage) => (m.kind === "connect" ? "Connect note" : `Message ${m.step}`);

function MessageCard({ message: m, before, onChange }: { message: LinkedinMessage; before?: LinkedinMessage; onChange: () => void }) {
  const sentBySplee = m.status === "done" && m.sent_via === "unipile";
  const setStatus = async (status: LinkedinMessage["status"]) => {
    try {
      await api(`/linkedin/${m.id}`, { method: "PATCH", body: { status } });
      onChange();
    } catch (e) {
      toastError(e);
    }
  };

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(m.body);
      toast.success("Copied");
    } catch (e) {
      toastError(e);
    }
  };

  return (
    <li className={cn("flex flex-col gap-2 rounded-xl border border-border p-3", m.status !== "todo" && "bg-muted/60")}>
      <div className="flex min-h-6 items-center justify-between">
        <MicroLabel>{stepName(m)}</MicroLabel>
        {m.status !== "todo" ? (
          <Badge tone={m.status === "done" ? "green" : "neutral"}>{m.status === "skipped" ? "Skipped" : sentBySplee ? "Sent by Splee" : "Done"}</Badge>
        ) : isDue(m) ? (
          <Badge tone="amber">Due now</Badge>
        ) : m.due_at ? (
          <span className="text-xs text-muted-foreground" title={dateTime(m.due_at)}>
            Due {timeAgo(m.due_at)}
          </span>
        ) : (
          before && (
            <span className="text-xs text-muted-foreground">{before.kind === "connect" ? "After the connect note" : `After message ${before.step}`}</span>
          )
        )}
      </div>
      <p className={cn("flex-1 text-sm whitespace-pre-wrap", m.status !== "todo" && "text-muted-foreground")}>{m.body}</p>
      {m.status === "todo" && m.send_error && (
        <p className="rounded-lg bg-red-50 px-2.5 py-1.5 text-xs text-red-700">Splee couldn&apos;t send this: {m.send_error}. Send it yourself.</p>
      )}
      <div className="flex gap-1">
        <Button size="sm" variant="ghost" onClick={copy}>
          <Copy className="size-3.5" /> Copy
        </Button>
        <span className="flex-1" />
        {m.status === "todo" ? (
          <>
            <Button size="sm" variant="ghost" onClick={() => setStatus("skipped")}>
              <SkipForward className="size-3.5" /> Skip
            </Button>
            <Button size="sm" variant="outline" onClick={() => setStatus("done")}>
              <Check className="size-3.5" /> Done
            </Button>
          </>
        ) : (
          // A step Splee sent is out on LinkedIn, so there's nothing to undo.
          !sentBySplee && (
            <Button size="sm" variant="ghost" onClick={() => setStatus("todo")}>
              Undo
            </Button>
          )
        )}
      </div>
    </li>
  );
}
