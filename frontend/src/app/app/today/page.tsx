"use client";

import { useState } from "react";
import Link from "next/link";
import { CalendarClock, FileCheck2, Hourglass, MessageSquareReply, PhoneCall, Sun } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { EMAIL_KINDS } from "@/components/app/work/email-preview";
import { LeadModal } from "@/components/app/work/lead-modal";
import { LoadError } from "@/components/app/work/states";
import { ClassificationBadge } from "@/components/app/work/status";
import { Select } from "@/components/ui/form";
import { Badge, Card, EmptyState, Skeleton } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { Today } from "@/lib/types";
import { cn, dayTime, dealMoney, isPast, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

type Owner = "mine_or_unassigned" | "mine" | "everyone";

const daysSince = (ts: string) => Math.floor((Date.now() - new Date(ts).getTime()) / 86_400_000);

export default function TodayPage() {
  const [owner, setOwner] = useState<Owner>("mine_or_unassigned");
  const [lead, setLead] = useState<string | null>(null);
  const { data, error, mutate } = useSWR<Today>(`/today${qs({ owner })}`, fetcher, { refreshInterval: 60_000, keepPreviousData: true });

  const empty = data && !data.replies.total && !data.approvals.length && !data.calls.total && !data.meetings.total && !data.quiet.total;
  return (
    <>
      <PageHeader
        title="Today"
        description="What's waiting on you across your projects: replies to answer, calls and meetings coming up, drafts to approve and deals gone quiet."
        actions={
          <Select value={owner} onChange={(e) => setOwner(e.target.value as Owner)} className="h-9 w-auto" aria-label="Whose leads">
            <option value="mine_or_unassigned">Mine and unassigned</option>
            <option value="mine">Mine</option>
            <option value="everyone">Everyone</option>
          </Select>
        }
      />
      <PageBody>
        {error && !data ? (
          <LoadError error={error} retry={() => void mutate()} what="today's work" />
        ) : !data ? (
          <div className="grid gap-4 lg:grid-cols-2">
            {[0, 1, 2, 3].map((i) => (
              <Skeleton key={i} className="h-48 rounded-2xl" />
            ))}
          </div>
        ) : empty ? (
          <Card>
            <EmptyState icon={<Sun className="size-5" />} title="Nothing is waiting on you">
              Replies, calls, meetings, drafts and quiet deals show up here as they come in.
            </EmptyState>
          </Card>
        ) : (
          <div className="gap-4 lg:columns-2 [&>*]:mb-4 [&>*]:break-inside-avoid">
            {data.replies.total > 0 && (
              <Part
                icon={MessageSquareReply}
                title="Replies to answer"
                total={data.replies.total}
                shown={data.replies.items.length}
                more={{ href: "/app/inbox", label: "Open the inbox" }}
              >
                {data.replies.items.map((r) => (
                  <Row key={r.lead_id} href={`/app/p/${r.project_id}/inbox${qs({ lead: r.lead_id })}`}>
                    <Who name={r.name} company={r.company} aside={timeAgo(r.at)} />
                    <p className="mt-0.5 line-clamp-2 text-sm text-muted-foreground">{r.snippet}</p>
                    <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                      {r.classification && <ClassificationBadge value={r.classification} />}
                      <span className="text-xs text-muted-foreground">{r.project_name}</span>
                    </div>
                  </Row>
                ))}
              </Part>
            )}
            {data.meetings.total > 0 && (
              <Part icon={CalendarClock} title="Meetings in the next day" total={data.meetings.total} shown={data.meetings.items.length}>
                {data.meetings.items.map((m) => (
                  <Row key={m.lead_id} onClick={() => setLead(m.lead_id)}>
                    <Who name={m.name} company={m.company} aside={dayTime(m.meeting_at)} />
                    <div className="mt-1.5">
                      {m.brief ? (
                        <Badge tone="green">Brief ready</Badge>
                      ) : (
                        <span className="text-xs text-muted-foreground">Brief comes a day before</span>
                      )}
                    </div>
                  </Row>
                ))}
              </Part>
            )}
            {data.calls.total > 0 && (
              <Part icon={PhoneCall} title="Calls due" total={data.calls.total} shown={data.calls.items.length}>
                {data.calls.items.map((c) => (
                  <Row key={c.id} href={`/app/p/${c.project_id}/calls`}>
                    <Who
                      name={c.name}
                      company={c.company}
                      aside={<span className={cn(isPast(c.due_at) && "text-red-700")}>Due {timeAgo(c.due_at)}</span>}
                    />
                  </Row>
                ))}
              </Part>
            )}
            {data.approvals.length > 0 && (
              <Part icon={FileCheck2} title="Emails to approve" total={data.approvals.reduce((n, w) => n + w.count, 0)}>
                {data.approvals.map((w) => (
                  <Row key={`${w.project_id}-${w.kind}`} href={`/app/p/${w.project_id}/emails`}>
                    <Who
                      name={w.kind === "follow_up" ? "Follow-ups" : EMAIL_KINDS[w.kind]}
                      company={w.project_name}
                      aside={plural(w.count, "draft")}
                    />
                  </Row>
                ))}
              </Part>
            )}
            {data.quiet.total > 0 && (
              <Part
                icon={Hourglass}
                title="Deals gone quiet"
                total={data.quiet.total}
                shown={data.quiet.items.length}
                more={{ href: "/app/pipeline", label: "Open the pipeline" }}
              >
                {data.quiet.items.map((q) => (
                  <Row key={q.deal_id} href={`/app/pipeline${qs({ deal: q.deal_id })}`}>
                    <Who name={q.name} company={q.company} aside={dealMoney(q.value_cents / 100, q.currency)} />
                    <div className="mt-1.5 flex flex-wrap items-center gap-1.5">
                      <Badge>{q.stage}</Badge>
                      <span className="text-xs text-muted-foreground">
                        Quiet {plural(daysSince(q.last_touch_at), "day")}
                      </span>
                      {q.nudge_ready && <Badge tone="green">Nudge ready</Badge>}
                    </div>
                  </Row>
                ))}
              </Part>
            )}
          </div>
        )}
      </PageBody>
      <LeadModal leadId={lead} onClose={() => setLead(null)} />
    </>
  );
}

function Part({
  icon: Icon,
  title,
  total,
  shown,
  more,
  children,
}: {
  icon: React.ComponentType<{ className?: string }>;
  title: string;
  total: number;
  /** Leave it out when every item is listed. */
  shown?: number;
  more?: { href: string; label: string };
  children: React.ReactNode;
}) {
  return (
    <Card className="overflow-hidden">
      <div className="flex items-center gap-2 px-5 pt-4 pb-2">
        <Icon className="size-4 text-muted-foreground" />
        <h2 className="text-sm font-semibold">{title}</h2>
        <span className="ml-auto rounded-full bg-foreground px-1.5 text-[11px] leading-[18px] font-medium text-background tabular-nums">{total}</span>
      </div>
      <ul className="divide-y divide-border">{children}</ul>
      {shown !== undefined && total > shown && (
        <div className="border-t border-border px-5 py-2.5 text-xs text-muted-foreground">
          {total - shown} more
          {more && (
            <>
              {" · "}
              <Link href={more.href} className="font-medium text-foreground hover:underline">
                {more.label}
              </Link>
            </>
          )}
        </div>
      )}
    </Card>
  );
}

function Row({ href, onClick, children }: { href?: string; onClick?: () => void; children: React.ReactNode }) {
  const className = "block w-full px-5 py-3 text-left transition-colors hover:bg-black/[0.03]";
  return (
    <li>
      {href ? (
        <Link href={href} className={className}>
          {children}
        </Link>
      ) : (
        <button type="button" onClick={onClick} className={className}>
          {children}
        </button>
      )}
    </li>
  );
}

function Who({ name, company, aside }: { name: string; company: string; aside: React.ReactNode }) {
  return (
    <div className="flex items-baseline gap-2">
      <p className="min-w-0 truncate text-sm">
        <span className="font-medium">{name}</span>
        {company && <span className="text-muted-foreground"> · {company}</span>}
      </p>
      <span className="ml-auto shrink-0 text-xs text-muted-foreground tabular-nums">{aside}</span>
    </div>
  );
}
