"use client";

import { useState } from "react";
import useSWR from "swr";
import { ClipboardList } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { SettingsColumn } from "@/components/app/settings/layout";
import { ListSkeleton, LoadError, MicroLabel, Pager, PER_PAGE, useLastPageGuard } from "@/components/app/work/states";
import { Select } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { qs } from "@/lib/api";
import type { ApiKey, AuditEvent, Member, Paginated, Role } from "@/lib/types";
import { money, plural } from "@/lib/utils";

/** Keep in sync with the actions listed in backend/src/audit.rs. Short, so each fits its column on one line. */
const ACTIONS: { group: string; actions: Record<string, string> }[] = [
  {
    group: "Sign-in",
    actions: {
      "user.signed_in": "Signed in",
      "user.sign_in_failed": "Failed to sign in",
      "user.password_changed": "Changed password",
      "user.password_reset": "Reset password",
      "user.identity_linked": "Connected a sign-in",
      "user.identity_unlinked": "Removed a sign-in",
      "user.two_factor_enabled": "Turned on 2FA",
      "user.two_factor_disabled": "Turned off 2FA",
      "user.backup_codes_created": "Made backup codes",
      "user.passkey_added": "Added a passkey",
      "user.passkey_removed": "Removed a passkey",
    },
  },
  {
    group: "Team",
    actions: {
      "member.invited": "Invited a member",
      "member.joined": "Joined",
      "member.removed": "Removed a member",
      "member.role_changed": "Changed a role",
      "org.renamed": "Renamed the organization",
    },
  },
  {
    group: "Access",
    actions: {
      "api_key.created": "Created an API key",
      "api_key.revoked": "Revoked an API key",
      "integration.connected": "Connected an integration",
      "integration.disconnected": "Removed an integration",
    },
  },
  {
    group: "Sending",
    actions: {
      "mailbox.connected": "Connected a mailbox",
      "mailbox.removed": "Removed a mailbox",
      "sending_domain.ordered": "Ordered a domain",
      "sending_domain.removed": "Removed a domain",
      "campaign.launched": "Launched a campaign",
      "campaign.paused": "Paused a campaign",
      "project.started": "Started sending",
      "project.paused": "Paused sending",
    },
  },
  {
    group: "Data export",
    actions: {
      "export.saved": "Changed the data export",
      "export.removed": "Stopped the data export",
    },
  },
];

const LABELS: Record<string, string> = Object.assign({}, ...ACTIONS.map((g) => g.actions));
const ROLES: Record<Role, string> = { owner: "Owner", admin: "Admin", member: "Member" };
const MAILBOX_SIGN_IN: Record<string, string> = { google: "Google sign-in", microsoft: "Microsoft sign-in", smtp: "Password" };
const SSO: Record<string, string> = { google: "Google", microsoft: "Microsoft" };

function role(value: unknown): string {
  return ROLES[value as Role] ?? String(value ?? "");
}

/** The one line under what happened, from the event's details. */
function note(e: AuditEvent): string | null {
  const d = e.details;
  switch (e.action) {
    case "user.sign_in_failed":
      if (typeof d.reason === "string" && SSO[d.reason]) return `${SSO[d.reason]} didn't confirm the email`;
      if (d.reason === "passkey") return "Passkey refused";
      return d.reason === "code" ? "Wrong code" : "Wrong password";
    case "user.signed_in":
      if (d.method === "passkey") return "With a passkey";
      return typeof d.method === "string" && SSO[d.method] ? `With ${SSO[d.method]}` : null;
    case "user.password_changed":
      return d.first ? "Set a first password" : null;
    case "user.identity_linked":
    case "user.identity_unlinked":
      return SSO[String(d.provider)] ?? null;
    case "user.passkey_added":
    case "user.passkey_removed":
      return typeof d.name === "string" ? d.name : null;
    case "member.role_changed":
      return `${role(d.from)} to ${role(d.to)}`;
    case "member.invited":
    case "member.joined":
    case "member.removed":
      return d.role ? `As ${role(d.role).toLowerCase()}` : null;
    case "mailbox.connected":
      return MAILBOX_SIGN_IN[String(d.provider)] ?? null;
    case "sending_domain.ordered":
      return typeof d.price_micros === "number" ? `${money(d.price_micros)} a year` : null;
    case "campaign.launched":
      return typeof d.emails_queued === "number" ? `${plural(d.emails_queued, "email")} queued` : null;
    case "campaign.paused":
      return typeof d.emails_held === "number" ? `${plural(d.emails_held, "email")} held back` : null;
    case "integration.connected":
    case "integration.disconnected":
      return typeof d.project === "string" ? `In ${d.project}` : null;
    case "export.saved":
      return Array.isArray(d.tables) ? `${plural(d.tables.length, "table")}${d.include_bodies ? " with email bodies" : ""}` : null;
    case "org.renamed":
      return typeof d.from === "string" ? `Was ${d.from}` : null;
    default:
      return null;
  }
}

/** "Nov 21, 3:05 PM", short enough for its column. */
function when(ts: string): string {
  return new Date(ts).toLocaleString("en-US", { month: "short", day: "numeric", hour: "numeric", minute: "2-digit" });
}

export default function AuditLogPage() {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const [actor, setActor] = useState("");
  const [action, setAction] = useState("");
  const [page, setPage] = useState(1);
  const key = admin ? `/organization/audit${qs({ actor, action, page, per_page: PER_PAGE })}` : null;
  const { data, error, mutate } = useSWR<Paginated<AuditEvent>>(key, fetcher, { keepPreviousData: true });
  const { data: members } = useSWR<Member[]>(admin ? "/organization/users" : null, fetcher);
  const { data: keys } = useSWR<ApiKey[]>(admin ? "/organization/api-keys" : null, fetcher);
  useLastPageGuard(data?.total, page, setPage);
  const filtered = actor !== "" || action !== "";

  return (
    <>
      <PageHeader title="Audit log" description="Who signed in and who changed your team, keys, mailboxes and sending. Kept for a year." />
      <PageBody>
        <SettingsColumn>
          {me && !admin ? (
            <Card>
              <EmptyState icon={<ClipboardList className="size-6" />} title="Only admins can see the audit log">
                Ask an admin of your organization if you need to know who changed something.
              </EmptyState>
            </Card>
          ) : (
            <Card>
              {/* On phones each filter gets its own full-width row. */}
              <div className="flex flex-wrap items-center gap-3 border-b border-border p-4">
                <Select
                  value={actor}
                  onChange={(e) => {
                    setActor(e.target.value);
                    setPage(1);
                  }}
                  aria-label="Filter by member"
                  className="w-full sm:w-56"
                >
                  <option value="">Everyone</option>
                  {members?.map((m) => (
                    <option key={m.user_id} value={m.user_id}>
                      {m.name || m.email}
                    </option>
                  ))}
                  {keys && keys.length > 0 && (
                    <optgroup label="API keys">
                      {keys.map((k) => (
                        <option key={k.id} value={k.id}>
                          {k.name}
                        </option>
                      ))}
                    </optgroup>
                  )}
                </Select>
                <Select
                  value={action}
                  onChange={(e) => {
                    setAction(e.target.value);
                    setPage(1);
                  }}
                  aria-label="Filter by action"
                  className="w-full sm:w-56"
                >
                  <option value="">Every action</option>
                  {ACTIONS.map((g) => (
                    <optgroup key={g.group} label={g.group}>
                      {Object.entries(g.actions).map(([value, label]) => (
                        <option key={value} value={value}>
                          {label}
                        </option>
                      ))}
                    </optgroup>
                  ))}
                </Select>
                {data && <span className="text-sm text-muted-foreground tabular-nums sm:ml-auto">{plural(data.total, "event")}</span>}
              </div>

              {error ? (
                <LoadError error={error} what="the audit log" retry={() => mutate()} />
              ) : !data ? (
                <ListSkeleton rows={5} />
              ) : data.items.length === 0 ? (
                <EmptyState icon={<ClipboardList className="size-6" />} title={filtered ? "No matching events" : "Nothing logged yet"}>
                  {filtered ? "Try another member or action." : "Sign-ins and changes to members, keys, mailboxes and integrations show up here."}
                </EmptyState>
              ) : (
                <>
                  <div className="hidden grid-cols-[7rem_8rem_minmax(0,1fr)_minmax(0,1fr)] gap-x-3 border-b border-border px-4 py-2 md:grid">
                    <MicroLabel>Time</MicroLabel>
                    <MicroLabel>Who</MicroLabel>
                    <MicroLabel>What</MicroLabel>
                    <MicroLabel>Target</MicroLabel>
                  </div>
                  <ul className="divide-y divide-border">
                    {data.items.map((e) => (
                      <Row key={e.id} event={e} self={e.actor_user_id === me?.user.id} />
                    ))}
                  </ul>
                </>
              )}

              {data && <Pager page={page} total={data.total} onChange={setPage} />}
            </Card>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}

/**
 * Four columns from md up. On a phone, who and when share the first line and what and the target get a line each, so
 * nothing is squeezed into a narrow column.
 */
function Row({ event: e, self }: { event: AuditEvent; self: boolean }) {
  const who = e.actor_name ? (self ? `${e.actor_name} (you)` : e.actor_name) : e.actor_key_id ? "A deleted API key" : "A removed member";
  const extra = note(e);
  const full = new Date(e.created_at).toLocaleString();
  return (
    <li className="grid grid-cols-[minmax(0,1fr)_auto] gap-x-3 gap-y-0.5 px-4 py-3 text-sm md:grid-cols-[7rem_8rem_minmax(0,1fr)_minmax(0,1fr)]">
      <div className="col-start-2 row-start-1 text-right md:col-start-1 md:text-left">
        <time dateTime={e.created_at} title={full} className="whitespace-nowrap text-muted-foreground tabular-nums">
          {when(e.created_at)}
        </time>
        {e.ip && (
          <p className="hidden truncate font-mono text-xs text-muted-foreground md:block" title={e.ip}>
            {e.ip}
          </p>
        )}
      </div>
      <div className="col-start-1 row-start-1 min-w-0 md:col-start-2">
        <p className="truncate font-medium" title={who}>
          {who}
        </p>
        {e.actor_key_id && <p className="text-xs text-muted-foreground">API key</p>}
      </div>
      <div className="col-span-2 min-w-0 md:col-span-1">
        <p className="text-pretty">{LABELS[e.action] ?? e.action}</p>
        {extra && <p className="truncate text-xs text-muted-foreground" title={extra}>{extra}</p>}
      </div>
      <p className="col-span-2 min-w-0 truncate text-muted-foreground md:col-span-1 md:text-foreground" title={e.target_label}>
        {e.target_label || "—"}
      </p>
    </li>
  );
}
