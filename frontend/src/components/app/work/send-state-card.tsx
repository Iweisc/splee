"use client";

import { Bot, CalendarClock, Clock, FileCheck2, Hourglass, Inbox, Mailbox, Pause, Send } from "lucide-react";
import useSWR from "swr";
import { LinkButton } from "@/components/ui/button";
import { Card } from "@/components/ui/misc";
import type { SendState, SendStateName } from "@/lib/types";
import { num, plural, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

interface View {
  icon: React.ReactNode;
  text: string;
  /** The page that changes what holds it, or none when the steps on the campaign page do. */
  fix?: { label: string; href: string };
}

/** Next send time in the viewer's own clock, like the project overview's sending hours notice. */
function when(at: string) {
  const yours = new Date(at).toLocaleString("en-US", { weekday: "short", hour: "numeric", minute: "2-digit" });
  return `${yours} your time, ${timeAgo(at)}`;
}

// Archived, not started and paused have no entry: the page header and the steps above already say so and hold the button.
const VIEWS: Partial<Record<SendStateName, (s: SendState, projectId: string) => View>> = {
  sending: (s) => ({ icon: <Send className="size-4 text-brand-600" />, text: `${s.message}.` }),
  scheduled: (s) => ({
    icon: <CalendarClock className="size-4 text-muted-foreground" />,
    text: s.today.next_send_at ? `Its next emails go out ${when(s.today.next_send_at)}.` : `${s.message}.`,
  }),
  outside_hours: (s, p) => ({
    icon: <Clock className="size-4 text-muted-foreground" />,
    text: s.today.next_send_at ? `Outside sending hours. The next emails go out ${when(s.today.next_send_at)}.` : "The project has no sending hours.",
    fix: { label: "Change hours", href: `/app/p/${p}/schedule` },
  }),
  project_paused: (_, p) => ({
    icon: <Pause className="size-4 text-amber-700" />,
    text: "Sending is paused for the whole project, so this campaign waits too.",
    fix: { label: "Project overview", href: `/app/p/${p}` },
  }),
  no_mailbox: () => ({
    icon: <Mailbox className="size-4 text-amber-700" />,
    text: "No mailbox can send. Connect one, or fix the ones that stopped.",
    fix: { label: "Mailboxes", href: "/app/domains" },
  }),
  awaiting_approval: (s, p) => ({
    icon: <FileCheck2 className="size-4 text-amber-700" />,
    text: `${plural(s.today.drafts, "email")} ${s.today.drafts === 1 ? "waits" : "wait"} for your approval.`,
    fix: { label: "Review emails", href: `/app/p/${p}/emails` },
  }),
  nothing_queued: (s) => ({ icon: <Inbox className="size-4 text-muted-foreground" />, text: `${s.message}.` }),
  monthly_cap_reached: (_, p) => ({
    icon: <Hourglass className="size-4 text-amber-700" />,
    text: "The project reached its monthly cap, so new emails wait for the 1st or a higher cap.",
    fix: { label: "Change cap", href: `/app/p/${p}/daily-limit` },
  }),
  lead_cap_reached: (_, p) => ({
    icon: <Bot className="size-4 text-amber-700" />,
    text: "Autopilot emailed this month's cap of new leads, so first emails wait for the 1st. Follow-ups still go out.",
    fix: { label: "Change cap", href: `/app/p/${p}/autopilot` },
  }),
  daily_limit_reached: (s, p) => ({
    icon: <Hourglass className="size-4 text-amber-700" />,
    text: `The project sent its ${plural(s.today.daily_email_limit, "email")} for today.${s.today.next_send_at ? ` More go out ${when(s.today.next_send_at)}.` : ""}`,
    fix: { label: "Change limit", href: `/app/p/${p}/daily-limit` },
  }),
  // Changed in this campaign's Daily limit card, further down the same page.
  campaign_limit_reached: (s) => ({
    icon: <Hourglass className="size-4 text-amber-700" />,
    text: `This campaign sent its ${plural(s.today.campaign_limit ?? 0, "email")} for today.${s.today.next_send_at ? ` More go out ${when(s.today.next_send_at)}.` : ""}`,
  }),
  mailboxes_full: (s) => ({
    icon: <Mailbox className="size-4 text-amber-700" />,
    text: `Its mailboxes sent their most for today or are still warming up.${s.today.next_send_at ? ` More go out ${when(s.today.next_send_at)}.` : ""}`,
    fix: { label: "Mailboxes", href: "/app/domains" },
  }),
};

/** What a running campaign sends today and, when nothing goes out, the one thing holding it, with the page that changes it. */
export function SendStateCard({ projectId, segmentId }: { projectId: string; segmentId: string }) {
  // Refreshed each minute, since sending hours open and limits fill while the page sits open.
  const { data } = useSWR<SendState>(`/segments/${segmentId}/send-state`, fetcher, { refreshInterval: 60_000 });
  const view = data && VIEWS[data.state]?.(data, projectId);
  if (!data || !view) return null;
  const { sent, queued, drafts, campaign_limit } = data.today;
  const today = [`${num(sent)}${campaign_limit === null ? "" : ` of ${num(campaign_limit)}`} sent today`, `${num(queued)} queued`, drafts > 0 && plural(drafts, "draft")].filter(Boolean).join(" · ");
  return (
    <Card className="flex flex-wrap items-center gap-3 px-4 py-3 text-sm">
      {view.icon}
      <div className="min-w-0 flex-1 basis-64">
        <p className="text-pretty">{view.text}</p>
        <p className="text-xs text-muted-foreground tabular-nums">{today}</p>
      </div>
      {view.fix && (
        <LinkButton href={view.fix.href} variant="outline" size="sm" className="max-sm:grow">
          {view.fix.label}
        </LinkButton>
      )}
    </Card>
  );
}
