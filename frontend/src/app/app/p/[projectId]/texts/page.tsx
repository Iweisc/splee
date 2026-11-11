"use client";

import { use, useState } from "react";
import { Check, Pencil, Smartphone, X } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Select, Textarea } from "@/components/ui/form";
import { Avatar, Badge, type BadgeTone, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { LeadRow, SegmentWithStats, TextMessage, TwilioStatus } from "@/lib/types";
import { cn, dateTime, timeAgo } from "@/lib/utils";

type Thread = { lead: LeadRow; texts: TextMessage[] };

const STATUS: Record<TextMessage["status"], { label: string; tone: BadgeTone }> = {
  draft: { label: "Needs approval", tone: "amber" },
  approved: { label: "Scheduled", tone: "blue" },
  sending: { label: "Sending", tone: "blue" },
  sent: { label: "Sent", tone: "green" },
  delivered: { label: "Delivered", tone: "green" },
  failed: { label: "Failed", tone: "red" },
  cancelled: { label: "Cancelled", tone: "neutral" },
};

export default function TextsPage({ params }: PageProps<"/app/p/[projectId]/texts">) {
  const { projectId } = use(params);
  const [segment, setSegment] = useState("");
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const { data: twilio } = useSWR<TwilioStatus>("/organization/twilio", fetcher);
  const { data, error, mutate } = useSWR<Thread[]>(`/projects/${projectId}/texts${qs({ segment_id: segment })}`, fetcher, { keepPreviousData: true });
  const waiting = (data ?? []).filter((t) => t.texts.some((m) => m.status === "draft"));
  // Opens on the drafts when there are some, chosen once so approving the last one shows "Nothing to approve".
  const [picked, setPicked] = useState<"drafts" | "all" | null>(null);
  if (picked === null && data) setPicked(waiting.length > 0 ? "drafts" : "all");
  const view = picked ?? "all";
  const shown = view === "drafts" ? waiting : (data ?? []);

  return (
    <>
      <PageHeader
        title="Texts"
        description="The SMS and WhatsApp steps of your sequences. Drafts wait here for you unless their campaign is approved, and a reply on any channel stops the rest."
        actions={
          <>
            <Select value={view} onChange={(e) => setPicked(e.target.value as "drafts" | "all")} className="w-auto flex-auto" aria-label="Show">
              <option value="drafts">Needs approval ({waiting.length})</option>
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
        {twilio && !twilio.connection && (
          <p className="mb-4 rounded-xl bg-amber-50 px-4 py-3 text-sm text-pretty text-amber-800">
            Twilio isn&apos;t connected, so approved texts wait. Connect it on the Integrations page.
          </p>
        )}
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <div className="flex flex-col gap-3">
            {Array.from({ length: 3 }, (_, i) => (
              <Skeleton key={i} className="h-24 rounded-2xl" />
            ))}
          </div>
        ) : shown.length === 0 ? (
          <Card>
            {view === "drafts" ? (
              <EmptyState icon={<Check className="size-5" />} title="Nothing to approve">
                New drafts show up here as each text step comes due.
              </EmptyState>
            ) : (
              <EmptyState
                icon={<Smartphone className="size-5" />}
                title="No texts yet"
                action={
                  <LinkButton href={`/app/p/${projectId}/follow-ups`} variant="outline" size="sm">
                    Add text steps
                  </LinkButton>
                }
              >
                Add SMS or WhatsApp steps on the Follow-ups page. Leads with a phone number get them after their first email.
              </EmptyState>
            )}
          </Card>
        ) : (
          <Card className="divide-y divide-border">
            {shown.map((t) => (
              <LeadTexts key={t.lead.id} thread={t} onChange={() => mutate()} />
            ))}
          </Card>
        )}
      </PageBody>
    </>
  );
}

function LeadTexts({ thread: { lead, texts }, onChange }: { thread: Thread; onChange: () => Promise<unknown> }) {
  return (
    <div className="px-4 py-3.5 sm:px-5">
      <div className="flex items-center gap-3">
        <Avatar name={lead.full_name || "?"} />
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-baseline justify-between gap-x-3">
            <p className="font-medium">{lead.full_name}</p>
            <span className="font-mono text-sm text-muted-foreground tabular-nums">{lead.phone}</span>
          </div>
          <p className="text-sm wrap-anywhere text-muted-foreground">{[lead.title, lead.company_name].filter(Boolean).join(" at ")}</p>
        </div>
      </div>
      <ol className="mt-4 grid gap-3 md:grid-cols-2">
        {texts.map((m) => (
          <TextCard key={m.id} text={m} onChange={onChange} />
        ))}
      </ol>
    </div>
  );
}

function TextCard({ text: m, onChange }: { text: TextMessage; onChange: () => Promise<unknown> }) {
  const [editing, setEditing] = useState(false);
  const [body, setBody] = useState(m.body);
  const [busy, setBusy] = useState<"approved" | "cancelled" | "save" | null>(null);
  const open = m.status === "draft" || m.status === "approved";
  const change = async (what: "approved" | "cancelled" | "save") => {
    setBusy(what);
    try {
      await api(`/texts/${m.id}`, { method: "PATCH", body: { ...(what === "save" ? {} : { status: what }), ...(editing ? { body } : {}) } });
      await onChange();
      setEditing(false);
      if (what === "approved") toast.success(m.due_at && new Date(m.due_at) > new Date() ? `Approved. It goes out ${timeAgo(m.due_at)}` : "Approved. It goes out in your sending hours");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const status = STATUS[m.status];
  const when = m.sent_at ?? m.due_at;
  const name = m.kind === "reply" ? "Your reply" : `Text ${m.step}`;
  return (
    <li className={cn("flex flex-col gap-2 rounded-xl border border-border p-3 wrap-anywhere", !open && "bg-muted/60")}>
      <div className="flex min-h-6 flex-wrap items-center justify-between gap-2">
        <MicroLabel>
          {name} · {m.channel === "whatsapp" ? (m.template_sid ? "WhatsApp template" : "WhatsApp") : "SMS"}
        </MicroLabel>
        <Badge tone={status.tone}>{status.label}</Badge>
      </div>
      {editing ? (
        <Textarea value={body} onChange={(e) => setBody(e.target.value)} className="min-h-28 field-sizing-content" autoFocus />
      ) : (
        <p className={cn("flex-1 text-sm whitespace-pre-wrap", !open && "text-muted-foreground")}>{m.body}</p>
      )}
      {editing && <p className={cn("text-right text-xs tabular-nums", body.length > 320 ? "text-amber-700" : "text-muted-foreground")}>{body.length} characters</p>}
      {m.error && (m.status === "failed" || m.status === "cancelled") && (
        <p className={cn("rounded-lg px-2.5 py-1.5 text-xs", m.status === "failed" ? "bg-red-50 text-red-700" : "bg-muted text-muted-foreground")}>{m.error}</p>
      )}
      {when && (
        <p className="text-xs text-muted-foreground" title={dateTime(when)}>
          {m.sent_at ? `Sent ${dateTime(m.sent_at)}` : `Due ${timeAgo(when)}`}
        </p>
      )}
      {open && (
        <div className="flex flex-wrap justify-end gap-1">
          <Button size="sm" variant="ghost" onClick={() => change("cancelled")} loading={busy === "cancelled"} disabled={!!busy} className="px-2">
            <X className="size-3.5" /> Cancel
          </Button>
          {!m.template_sid && !editing && (
            <Button size="sm" variant="ghost" onClick={() => setEditing(true)} className="px-2">
              <Pencil className="size-3.5" /> Edit
            </Button>
          )}
          {m.status === "draft" ? (
            <Button size="sm" onClick={() => change("approved")} loading={busy === "approved"} disabled={!!busy || !body.trim()}>
              <Check className="size-3.5" /> Approve
            </Button>
          ) : (
            editing && (
              <Button size="sm" onClick={() => change("save")} loading={busy === "save"} disabled={!!busy || !body.trim()}>
                Save
              </Button>
            )
          )}
        </div>
      )}
    </li>
  );
}
