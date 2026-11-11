"use client";

import Link from "next/link";
import useSWR from "swr";
import { ArrowDown, ArrowUp } from "lucide-react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { OptionCards } from "@/components/app/settings/option-cards";
import { memberName, useMembers } from "@/components/app/work/owner-menu";
import { Button } from "@/components/ui/button";
import { Avatar, Badge, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { AssignRule, Mailbox, Member, ProjectSettings } from "@/lib/types";
import { plural } from "@/lib/utils";

const RULES: { value: AssignRule; label: string; description: string }[] = [
  { value: "off", label: "Off", description: "Leads stay unassigned until someone on the team takes one." },
  { value: "round_robin", label: "Round robin", description: "Each new lead goes to the next person you pick below, in turn." },
  { value: "mailbox", label: "By mailbox", description: "A lead goes to whoever owns the mailbox that emailed it." },
];

export default function AssignmentPage() {
  return (
    <ProjectSettingsPage
      title="Assignment"
      description="Who gets a lead when it first replies, books a meeting or leaves its email in the website chat. A lead that already has an owner keeps them."
      problem={(s) => (s.assign_rule === "round_robin" && s.assign_members.length === 0 ? "Pick at least one person for round robin" : null)}
    >
      {(s, update) => (
        <>
          <Section title="Rule" description="How this project picks an owner for a lead nobody has yet.">
            <OptionCards
              name="assign_rule"
              value={s.assign_rule}
              onChange={(assign_rule) => update({ assign_rule })}
              options={RULES}
              className="@2xl:grid-cols-3"
            />
          </Section>
          {s.assign_rule === "round_robin" && <RoundRobin settings={s} update={update} />}
          {s.assign_rule === "mailbox" && <MailboxOwners />}
        </>
      )}
    </ProjectSettingsPage>
  );
}

function RoundRobin({ settings: s, update }: { settings: ProjectSettings; update: (patch: Partial<ProjectSettings>) => void }) {
  const members = useMembers();
  // The picked come first in their turn order, then everyone else on the team.
  const picked = s.assign_members.map((id) => members?.find((m) => m.user_id === id)).filter((m): m is Member => !!m);
  const others = members?.filter((m) => !s.assign_members.includes(m.user_id)) ?? [];
  const move = (at: number, by: number) => {
    const order = [...s.assign_members];
    [order[at], order[at + by]] = [order[at + by]!, order[at]!];
    update({ assign_members: order });
  };
  const toggle = (id: string) =>
    update({ assign_members: s.assign_members.includes(id) ? s.assign_members.filter((m) => m !== id) : [...s.assign_members, id] });

  return (
    <Section title="Who takes turns" description="New leads go to these people in this order. Anyone who switched on Away in their account is skipped.">
      {!members ? (
        <Skeleton className="h-24 w-full" />
      ) : (
        <ul className="divide-y divide-border rounded-xl border border-border">
          {[...picked, ...others].map((m) => {
            const at = s.assign_members.indexOf(m.user_id);
            return (
              <li key={m.user_id} className="flex items-center gap-3 px-3 py-2.5 text-sm">
                <input
                  type="checkbox"
                  checked={at >= 0}
                  onChange={() => toggle(m.user_id)}
                  aria-label={`${memberName(m)} takes turns`}
                  className="accent-black"
                />
                <Avatar name={memberName(m)} className="size-7" />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate font-medium">{memberName(m)}</span>
                    {m.away && <Badge className="shrink-0">Away</Badge>}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">{m.email}</span>
                </span>
                {at >= 0 && (
                  <span className="flex shrink-0 items-center gap-1">
                    <span className="mr-1 w-5 text-right text-xs text-muted-foreground tabular-nums">{at + 1}</span>
                    <Button variant="ghost" size="icon" className="size-8" disabled={at === 0} onClick={() => move(at, -1)} aria-label={`Move ${memberName(m)} up`}>
                      <ArrowUp className="size-3.5" />
                    </Button>
                    <Button
                      variant="ghost"
                      size="icon"
                      className="size-8"
                      disabled={at === picked.length - 1}
                      onClick={() => move(at, 1)}
                      aria-label={`Move ${memberName(m)} down`}
                    >
                      <ArrowDown className="size-3.5" />
                    </Button>
                  </span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

/** Mailboxes without an owner leave their leads unassigned, so this lists who owns each. */
function MailboxOwners() {
  const members = useMembers();
  const { data: mailboxes } = useSWR<Mailbox[]>("/mailboxes", fetcher);
  const unowned = mailboxes?.filter((b) => !b.owner_id || !members?.some((m) => m.user_id === b.owner_id)).length ?? 0;
  return (
    <Section
      title="Mailbox owners"
      description={
        <>
          Change who owns a mailbox on the{" "}
          <Link href="/app/domains" className="font-medium whitespace-nowrap text-foreground hover:underline">
            Mailboxes page
          </Link>
          .
        </>
      }
    >
      {!mailboxes ? (
        <Skeleton className="h-24 w-full" />
      ) : mailboxes.length === 0 ? (
        <p className="text-sm text-muted-foreground">No mailboxes yet. Connect one and it belongs to you.</p>
      ) : (
        <>
          <ul className="divide-y divide-border rounded-xl border border-border">
            {mailboxes.map((b) => {
              const owner = members?.find((m) => m.user_id === b.owner_id);
              return (
                <li key={b.id} className="flex flex-wrap items-center justify-between gap-x-3 gap-y-1 px-3 py-2.5 text-sm">
                  <span className="min-w-0 truncate font-medium">{b.email}</span>
                  <span className="flex shrink-0 items-center gap-2 text-muted-foreground">
                    {owner ? <Avatar name={memberName(owner)} className="size-5 text-[9px]" /> : null}
                    {owner ? memberName(owner) : "Nobody"}
                  </span>
                </li>
              );
            })}
          </ul>
          {unowned > 0 && (
            <p className="mt-3 text-sm text-muted-foreground">
              Leads emailed from {unowned === 1 ? "the mailbox" : `the ${plural(unowned, "mailbox", "mailboxes")}`} without an owner stay unassigned.
            </p>
          )}
        </>
      )}
    </Section>
  );
}
