"use client";

import Link from "next/link";
import useSWR from "swr";
import { ArrowDown, ArrowUp } from "lucide-react";
import { ChipInput } from "@/components/app/settings/chip-input";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { OptionCards } from "@/components/app/settings/option-cards";
import { memberName, useMembers } from "@/components/app/work/owner-menu";
import { Button } from "@/components/ui/button";
import { Avatar, Badge, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { countryCode, countryName } from "@/lib/countries";
import type { AssignRule, Mailbox, Member, ProjectSettings } from "@/lib/types";
import { plural } from "@/lib/utils";

const RULES: { value: AssignRule; label: string; description: string }[] = [
  { value: "off", label: "Off", description: "Leads stay unassigned until someone on the team takes one." },
  { value: "round_robin", label: "Round robin", description: "Each new lead goes to the next person you pick below, in turn." },
  { value: "mailbox", label: "By mailbox", description: "A lead goes to whoever owns the mailbox that emailed it." },
  { value: "territory", label: "By territory", description: "A lead goes to whoever covers its country, and the rest take turns." },
];

export default function AssignmentPage() {
  return (
    <ProjectSettingsPage
      title="Assignment"
      description="Who gets a lead when it first replies, books a meeting or leaves its email in the website chat. A lead that already has an owner keeps them."
      problem={(s) =>
        s.assign_rule === "round_robin" && s.assign_members.length === 0
          ? "Pick at least one person for round robin"
          : s.assign_rule === "territory" && s.assign_territories.length === 0 && s.assign_members.length === 0
            ? "Give someone a territory, or pick who takes turns"
            : null
      }
    >
      {(s, update) => (
        <>
          <Section title="Rule" description="How this project picks an owner for a lead nobody has yet.">
            <OptionCards
              name="assign_rule"
              value={s.assign_rule}
              onChange={(assign_rule) => update({ assign_rule })}
              options={RULES}
              className="@2xl:grid-cols-2"
            />
          </Section>
          {s.assign_rule === "round_robin" && (
            <RoundRobin
              settings={s}
              update={update}
              title="Who takes turns"
              description="New leads go to these people in this order. Anyone who switched on Away in their account is skipped."
            />
          )}
          {s.assign_rule === "territory" && (
            <>
              <Territories settings={s} update={update} />
              <RoundRobin
                settings={s}
                update={update}
                title="Who takes the rest"
                description="Leads from a country nobody covers, or from the territory of someone away, go to these people in turn. With nobody picked, they stay unassigned."
              />
            </>
          )}
          {s.assign_rule === "mailbox" && <MailboxOwners />}
        </>
      )}
    </ProjectSettingsPage>
  );
}

type Update = (patch: Partial<ProjectSettings>) => void;

/** Each member and the countries they cover. Saving drops the territory of someone who left the team. */
function Territories({ settings: s, update }: { settings: ProjectSettings; update: Update }) {
  const members = useMembers();
  const set = (owner: string, countries: string[]) => {
    const others = s.assign_territories.filter((t) => t.owner !== owner);
    // A country moves to whoever was given it last, since it can only be in one territory.
    const kept = others.map((t) => ({ ...t, countries: t.countries.filter((c) => !countries.includes(c)) })).filter((t) => t.countries.length);
    update({ assign_territories: countries.length ? [...kept, { owner, countries }] : kept });
  };
  return (
    <Section
      title="Territories"
      description="A lead's country is its own, else its company's, else its phone number's. Type a country's name or code and press Enter. A country belongs to one person."
    >
      {!members ? (
        <Skeleton className="h-24 w-full" />
      ) : (
        // A container, so each row goes side by side when the card has room, not the screen.
        <ul className="@container divide-y divide-border rounded-xl border border-border">
          {members.map((m) => (
            <li key={m.user_id} className="flex flex-col gap-2 px-3 py-2.5 text-sm @xl:flex-row @xl:items-center @xl:gap-3">
              <span className="flex min-w-0 items-center gap-3 @xl:w-56 @xl:shrink-0">
                <Avatar name={memberName(m)} className="size-7" />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-2">
                    <span className="truncate font-medium">{memberName(m)}</span>
                    {m.away && <Badge className="shrink-0">Away</Badge>}
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">{m.email}</span>
                </span>
              </span>
              <span className="min-w-0 flex-1">
                <ChipInput
                  value={s.assign_territories.find((t) => t.owner === m.user_id)?.countries ?? []}
                  onChange={(countries) => set(m.user_id, countries)}
                  parse={countryCode}
                  show={countryName}
                  label={`Countries ${memberName(m)} covers`}
                  placeholder="e.g. Germany, AT"
                />
              </span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function RoundRobin({ settings: s, update, title, description }: { settings: ProjectSettings; update: Update; title: string; description: string }) {
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
    <Section title={title} description={description}>
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
