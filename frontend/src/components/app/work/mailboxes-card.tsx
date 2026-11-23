"use client";

import Link from "next/link";
import { Inbox } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { Switch } from "@/components/ui/form";
import { Card, Skeleton } from "@/components/ui/misc";
import { useAction } from "@/components/app/settings/use-action";
import { api } from "@/lib/api";
import { todayLimit } from "@/lib/mailbox";
import type { Mailbox, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { MicroLabel } from "./states";

/** Which mailboxes send the campaign's first emails: the project's, or its own pick. Each tick saves at once. The last ticked
 *  mailbox can't be unticked, because an empty pick would send nothing. Switching it off is how to go back to the project's. */
export function MailboxesCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const { data: mailboxes } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${segment.project_id}/settings`, fetcher);
  const picked = segment.mailbox_ids;

  const save = (mailbox_ids: string[] | null, success?: string) =>
    run(
      "mailbox_ids",
      () =>
        mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { mailbox_ids } }), {
          revalidate: false,
        }),
      success,
    );
  // Its own pick starts as the project's, so switching it on changes nothing until someone ticks a box.
  const active = mailboxes?.filter((m) => m.status === "active").map((m) => m.id);
  const copy = settings?.mailbox_ids ?? active;
  const toggle = (id: string) => picked && save(picked.includes(id) ? picked.filter((m) => m !== id) : [...picked, id]);

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <Inbox className="size-3.5" /> Send from
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          The mailboxes that send its first emails. Follow-ups and replies stay with the mailbox the lead already talks to. The
          project&apos;s are set on the{" "}
          <Link href={`/app/p/${segment.project_id}/daily-limit`} className="font-medium text-foreground underline underline-offset-2">
            Daily limit page
          </Link>
          .
        </p>
      </div>
      <ul className="divide-y divide-border rounded-lg border border-border">
        <li className="flex items-center gap-3 px-3 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">Its own mailboxes</p>
            <p className="text-xs text-pretty text-muted-foreground">
              {picked ? "Switching it off goes back to the project's." : "Starts with the project's mailboxes ticked."}
            </p>
          </div>
          <Switch
            checked={picked !== null}
            onChange={(on) => (on ? save(copy!, "This campaign has its own mailboxes") : save(null, "Back to the project's mailboxes"))}
            disabled={pending === "mailbox_ids" || !copy?.length}
            label="Its own mailboxes"
          />
        </li>
        {picked !== null &&
          (!mailboxes ? (
            <li className="px-3 py-2.5">
              <Skeleton className="h-16 w-full" />
            </li>
          ) : (
            mailboxes.map((m) => {
              const on = picked.includes(m.id);
              const last = on && picked.length === 1;
              return (
                <li key={m.id}>
                  <label className="flex cursor-pointer items-center gap-3 px-3 py-2.5 text-sm">
                    <input
                      type="checkbox"
                      checked={on}
                      onChange={() => toggle(m.id)}
                      disabled={pending === "mailbox_ids" || last}
                      title={last ? "Keep at least one ticked, or switch it off to use the project's" : undefined}
                      className="accent-black"
                    />
                    <span className="min-w-0 flex-1 truncate font-medium">{m.email}</span>
                    {m.status === "active" ? (
                      <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{num(todayLimit(m))}/day</span>
                    ) : (
                      <span className="shrink-0 text-xs text-muted-foreground capitalize">{m.status}</span>
                    )}
                  </label>
                </li>
              );
            })
          ))}
      </ul>
    </Card>
  );
}
