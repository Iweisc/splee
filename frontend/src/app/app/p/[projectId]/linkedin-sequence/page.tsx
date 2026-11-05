"use client";

import { use, useState } from "react";
import { Check, Copy, ExternalLink, SkipForward, Sparkles, UserPlus } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Avatar, Badge, Card, EmptyState, Skeleton, Spinner } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { LeadRow, LinkedinMessage, SegmentWithStats } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useAgentEvents } from "@/hooks/use-agent-events";

type Sequence = { lead: LeadRow; messages: LinkedinMessage[] };

export default function LinkedinPage({ params }: PageProps<"/app/p/[projectId]/linkedin-sequence">) {
  const { projectId } = use(params);
  const [segment, setSegment] = useState("");
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const key = `/projects/${projectId}/linkedin${qs({ segment_id: segment })}`;
  const { data, error, mutate } = useSWR<Sequence[]>(key, fetcher, { keepPreviousData: true });

  return (
    <>
      <PageHeader
        title="LinkedIn"
        description="LinkedIn is manual: Splee writes the connect note and follow-ups, you send them from your own profile and tick them off."
        actions={
          <Select value={segment} onChange={(e) => setSegment(e.target.value)} className="w-auto">
            <option value="">All campaigns</option>
            {segments?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        }
      />
      <PageBody>
        {error ? (
          <LoadError error={error} retry={() => mutate()} />
        ) : !data ? (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 4 }, (_, i) => (
              <Skeleton key={i} className="h-16 rounded-2xl" />
            ))}
          </div>
        ) : data.length === 0 ? (
          <Card>
            <EmptyState
              icon={<UserPlus className="size-5" />}
              title="No leads yet"
              action={
                <LinkButton href={`/app/p/${projectId}/segments`} variant="outline" size="sm">
                  Go to campaigns
                </LinkButton>
              }
            >
              Find people in a campaign first. Every lead gets a LinkedIn sequence on request.
            </EmptyState>
          </Card>
        ) : (
          // One card with rows. A card per lead with gaps between made a 40-lead list several screens tall.
          <Card className="divide-y divide-border">
            {data.map((s) => (
              <LeadSequence key={s.lead.id} sequence={s} onChange={() => mutate()} />
            ))}
          </Card>
        )}
      </PageBody>
    </>
  );
}

function LeadSequence({ sequence: { lead, messages }, onChange }: { sequence: Sequence; onChange: () => void }) {
  const [runId, setRunId] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  useAgentEvents(runId ? `/api/runs/${runId}/events` : null, (e) => {
    if (e.kind === "done" || e.kind === "error") {
      setRunId(null);
      if (e.kind === "error") toast.error(e.message || "Couldn't write the sequence");
      onChange();
    }
  });

  const generate = async () => {
    setStarting(true);
    try {
      const { run_id } = await api<{ run_id: string }>(`/leads/${lead.id}/linkedin`, { method: "POST" });
      setRunId(run_id);
    } catch (e) {
      toastError(e);
    } finally {
      setStarting(false);
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
          <p className="truncate text-sm text-muted-foreground">{[lead.title, lead.company_name].filter(Boolean).join(" at ")}</p>
        </div>
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
        {runId ? (
          <span className="inline-flex items-center gap-2 text-sm text-muted-foreground">
            <Spinner /> Writing…
          </span>
        ) : (
          messages.length === 0 && (
            <Button size="sm" variant="outline" onClick={generate} loading={starting}>
              <Sparkles className="size-3.5" /> Generate sequence
            </Button>
          )
        )}
      </div>
      {sorted.length > 0 && (
        <ol className="mt-4 grid gap-3 lg:grid-cols-3">
          {sorted.map((m) => (
            <MessageCard key={m.id} message={m} onChange={onChange} />
          ))}
        </ol>
      )}
    </div>
  );
}

function MessageCard({ message: m, onChange }: { message: LinkedinMessage; onChange: () => void }) {
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
      <div className="flex items-center justify-between">
        <MicroLabel>{m.kind === "connect" ? "Connect note" : `Message ${m.step}`}</MicroLabel>
        {m.status !== "todo" && <Badge tone={m.status === "done" ? "green" : "neutral"}>{m.status === "done" ? "Done" : "Skipped"}</Badge>}
      </div>
      <p className={cn("flex-1 text-sm whitespace-pre-wrap", m.status !== "todo" && "text-muted-foreground")}>{m.body}</p>
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
          <Button size="sm" variant="ghost" onClick={() => setStatus("todo")}>
            Undo
          </Button>
        )}
      </div>
    </li>
  );
}
