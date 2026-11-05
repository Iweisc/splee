"use client";

import Link from "next/link";
import { ArrowRight, Check } from "lucide-react";
import useSWR from "swr";
import { Card } from "@/components/ui/misc";
import type { Mailbox, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { cn } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";
import { MicroLabel } from "./states";

/** What has to be true before the sender will actually send for this project. */
export function SetupChecklist({ projectId, settings, className }: { projectId: string; settings: ProjectSettings; className?: string }) {
  const { me } = useMe();
  const { data: mailboxes } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);

  const items = [
    {
      label: "Connect a mailbox",
      hint: "Emails go out from your own SMTP mailbox.",
      done: !!mailboxes?.some((m) => m.status === "active"),
      href: "/app/domains",
    },
    {
      label: "Review and start a campaign",
      hint: "Starting a campaign approves its drafts.",
      done: !!segments?.some((s) => s.status === "working" || s.status === "scaling"),
      href: `/app/p/${projectId}/segments`,
    },
    ...(me?.payments_enabled === false
      ? []
      : [
          {
            label: "Have credits on your balance",
            hint: "$0.04 per sent email.",
            done: (me?.balance_micros ?? 0) > 0,
            href: "/app/billing",
          },
        ]),
    {
      label: "Add your booking link",
      hint: "Auto-replies to interested leads include it.",
      done: !!settings.booking_link,
      href: `/app/p/${projectId}/auto-replies`,
    },
  ];
  const doneCount = items.filter((i) => i.done).length;

  return (
    <Card className={cn("p-5", className)}>
      <div className="mb-3 flex items-center justify-between">
        <MicroLabel>Get sending</MicroLabel>
        <span className="text-xs text-muted-foreground tabular-nums">
          {doneCount}/{items.length}
        </span>
      </div>
      <ul className="flex flex-col">
        {items.map((item) => (
          <li key={item.label} className="border-t border-border first:border-t-0">
            <Link href={item.href} className="group flex items-center gap-3 py-2.5">
              <span
                className={cn(
                  "inline-flex size-5 shrink-0 items-center justify-center rounded-full",
                  item.done ? "bg-brand-500 text-white" : "border border-border-strong",
                )}
              >
                {item.done && <Check className="size-3" />}
              </span>
              <span className="min-w-0 flex-1">
                <span className={cn("block text-sm font-medium", item.done && "text-muted-foreground line-through")}>{item.label}</span>
                <span className="block text-xs text-muted-foreground">{item.hint}</span>
              </span>
              {!item.done && <ArrowRight className="size-4 text-muted-foreground transition-transform group-hover:translate-x-0.5" />}
            </Link>
          </li>
        ))}
      </ul>
    </Card>
  );
}
