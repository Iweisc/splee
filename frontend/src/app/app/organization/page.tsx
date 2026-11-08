"use client";

import { useRouter } from "next/navigation";
import { useState } from "react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Check, Mail, Trash2, UserPlus } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CopyField } from "@/components/app/settings/copy-button";
import { Section, SettingsColumn } from "@/components/app/settings/layout";
import { LoadError } from "@/components/app/work/states";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Avatar, Badge, EmptyState, Modal, Skeleton } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Invite, Me, Member, Organization, Role } from "@/lib/types";
import { timeAgo } from "@/lib/utils";

const ROLE_LABELS: Record<Role, string> = { owner: "Owner", admin: "Admin", member: "Member" };

export default function TeamPage() {
  const { me } = useMe();
  // The shell only renders pages once `me` is loaded.
  if (!me) return null;
  const isAdmin = me.role === "owner" || me.role === "admin";

  return (
    <>
      <PageHeader title="Team" description="Everyone in an organization shares its projects and mailboxes." />
      <PageBody>
        <SettingsColumn>
          {me.orgs.length > 1 && <OrgSwitcher me={me} />}
          <OrgName me={me} isAdmin={isAdmin} />
          <Members me={me} isAdmin={isAdmin} />
          {isAdmin && <Invites />}
          {me.role === "owner" && <DeleteOrganization me={me} />}
        </SettingsColumn>
      </PageBody>
    </>
  );
}

/** Every cached project and mailbox belongs to the previous org, so drop the whole cache and start over at /app. */
function useStartOver() {
  const router = useRouter();
  const { mutate } = useSWRConfig();
  return () => {
    mutate(() => true, undefined);
    router.replace("/app");
  };
}

function OrgSwitcher({ me }: { me: Me }) {
  const { pending, run } = useAction();
  const startOver = useStartOver();
  const switchTo = (orgId: string) =>
    run(orgId, async () => {
      await api("/auth/switch-org", { body: { org_id: orgId } });
      startOver();
    });

  return (
    <Section title="Your organizations" description="Projects and mailboxes are separate for each organization.">
      <ul className="divide-y divide-border rounded-xl ring-1 ring-border">
        {me.orgs.map((o) => (
          <li key={o.id} className="flex items-center gap-3 px-4 py-3 text-sm">
            <span className="min-w-0 flex-1 truncate font-medium">{o.name}</span>
            <span className="text-muted-foreground">{ROLE_LABELS[o.role]}</span>
            {o.id === me.org.id ? (
              <Badge tone="green">
                <Check className="size-3" /> Current
              </Badge>
            ) : (
              <Button variant="outline" size="sm" onClick={() => switchTo(o.id)} loading={pending === o.id}>
                Switch
              </Button>
            )}
          </li>
        ))}
      </ul>
    </Section>
  );
}

function OrgName({ me, isAdmin }: { me: Me; isAdmin: boolean }) {
  const { mutate: mutateMe } = useMe();
  const [name, setName] = useState(me.org.name);
  const { pending, run } = useAction();
  const trimmed = name.trim();

  return (
    <Section title="Organization name" description="Shown to teammates in invites and in the sidebar.">
      <form
        className="flex gap-2"
        onSubmit={(e) => {
          e.preventDefault();
          run("name", async () => {
            await api<Organization>("/organization", { method: "PATCH", body: { name: trimmed } });
            await mutateMe();
          }, "Name saved");
        }}
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} disabled={!isAdmin} aria-label="Organization name" className="flex-1" />
        {isAdmin && (
          <Button type="submit" disabled={!trimmed || trimmed === me.org.name} loading={pending === "name"}>
            Save
          </Button>
        )}
      </form>
    </Section>
  );
}

function Members({ me, isAdmin }: { me: Me; isAdmin: boolean }) {
  const { data, error, mutate } = useSWR<Member[]>("/organization/users", fetcher);
  const { pending, run } = useAction();
  const roles: Role[] = me.role === "owner" ? ["owner", "admin", "member"] : ["admin", "member"];

  const setRole = (m: Member, role: Role) =>
    run(m.user_id, async () => {
      const updated = await api<Member>(`/organization/users/${m.user_id}`, { method: "PATCH", body: { role } });
      await mutate((list) => list?.map((x) => (x.user_id === m.user_id ? updated : x)), { revalidate: false });
    }, `${m.name || m.email} is now ${ROLE_LABELS[role].toLowerCase()}`);

  const remove = (m: Member) => {
    if (!confirm(`Remove ${m.name || m.email} from ${me.org.name}?`)) return;
    run(m.user_id, async () => {
      await api(`/organization/users/${m.user_id}`, { method: "DELETE" });
      await mutate();
    }, "Member removed");
  };

  return (
    <Section title="Members" description="Admins can invite people and change roles.">
      {error ? (
        <LoadError error={error} what="members" retry={() => mutate()} />
      ) : !data ? (
        <Skeleton className="h-24 w-full" />
      ) : (
        <ul className="divide-y divide-border rounded-xl ring-1 ring-border">
          {data.map((m) => {
            const self = m.user_id === me.user.id;
            // Admins can't demote or remove owners, only another owner can.
            const editable = isAdmin && !self && (me.role === "owner" || m.role !== "owner");
            return (
              <li key={m.user_id} className="flex items-center gap-3 px-4 py-3 text-sm">
                <Avatar name={m.name || m.email} />
                <div className="min-w-0 flex-1">
                  <p className="truncate font-medium">
                    {m.name || m.email}
                    {self && <span className="font-normal text-muted-foreground"> (you)</span>}
                  </p>
                  <p className="truncate text-xs text-muted-foreground">
                    {m.email} · joined {timeAgo(m.created_at)}
                  </p>
                </div>
                {editable ? (
                  <>
                    <Select
                      value={m.role}
                      onChange={(e) => setRole(m, e.target.value as Role)}
                      disabled={pending === m.user_id}
                      className="h-8 w-28"
                      aria-label={`Role for ${m.email}`}
                    >
                      {roles.map((r) => (
                        <option key={r} value={r}>
                          {ROLE_LABELS[r]}
                        </option>
                      ))}
                    </Select>
                    <Button variant="ghost" size="icon" className="size-8" onClick={() => remove(m)} aria-label={`Remove ${m.email}`}>
                      <Trash2 className="size-4" />
                    </Button>
                  </>
                ) : (
                  <span className="text-muted-foreground">{ROLE_LABELS[m.role]}</span>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

function Invites() {
  const { data, error, mutate } = useSWR<Invite[]>("/organization/invites", fetcher);
  const { pending, run } = useAction();
  const [open, setOpen] = useState(false);
  const pendingInvites = data?.filter((i) => !i.accepted_at);

  const revoke = (i: Invite) =>
    run(i.id, async () => {
      await api(`/organization/invites/${i.id}`, { method: "DELETE" });
      await mutate();
    }, "Invite revoked");

  return (
    <Section
      title="Invites"
      description="Invitations that haven't been accepted yet."
      actions={
        <Button onClick={() => setOpen(true)}>
          <UserPlus className="size-4" /> Invite
        </Button>
      }
    >
      {error ? (
        <LoadError error={error} what="invites" retry={() => mutate()} />
      ) : !pendingInvites ? (
        <Skeleton className="h-12 w-full" />
      ) : pendingInvites.length === 0 ? (
        <EmptyState icon={<Mail className="size-6" />} title="No pending invites" className="py-6" />
      ) : (
        <ul className="divide-y divide-border rounded-xl ring-1 ring-border">
          {pendingInvites.map((i) => (
            <li key={i.id} className="flex items-center gap-3 px-4 py-3 text-sm">
              <div className="min-w-0 flex-1">
                <p className="truncate font-medium">{i.email}</p>
                <p className="text-xs text-muted-foreground">
                  {ROLE_LABELS[i.role]} · invited {timeAgo(i.created_at)}
                </p>
              </div>
              <Button variant="outline" size="sm" onClick={() => revoke(i)} loading={pending === i.id}>
                Revoke
              </Button>
            </li>
          ))}
        </ul>
      )}
      <Modal open={open} onClose={() => setOpen(false)} title="Invite a teammate">
        {open && <InviteForm onInvited={() => mutate()} onDone={() => setOpen(false)} />}
      </Modal>
    </Section>
  );
}

function InviteForm({ onInvited, onDone }: { onInvited: () => void; onDone: () => void }) {
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<Invite["role"]>("member");
  const [link, setLink] = useState<string | null>(null);
  const { pending, run } = useAction();

  if (link) {
    return (
      <div className="grid gap-4">
        <p className="text-sm text-muted-foreground">
          Send this link to {email}. If you have an active mailbox, Splee has emailed it to them too.
        </p>
        <CopyField value={link} />
        <div className="flex justify-end">
          <Button onClick={onDone}>Done</Button>
        </div>
      </div>
    );
  }

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        run("invite", async () => {
          const res = await api<{ invite: Invite; link: string }>("/organization/invites", { body: { email: email.trim(), role } });
          setLink(res.link);
          onInvited();
        });
      }}
    >
      <Field label="Email">
        <Input type="email" value={email} onChange={(e) => setEmail(e.target.value)} placeholder="teammate@company.com" autoFocus required />
      </Field>
      <Field label="Role">
        <Select value={role} onChange={(e) => setRole(e.target.value as Invite["role"])}>
          <option value="member">Member: runs campaigns</option>
          <option value="admin">Admin: also manages the team</option>
        </Select>
      </Field>
      <div className="flex justify-end">
        <Button type="submit" disabled={!email.trim()} loading={pending === "invite"}>
          Create invite
        </Button>
      </div>
    </form>
  );
}

function DeleteOrganization({ me }: { me: Me }) {
  const [open, setOpen] = useState(false);
  const [confirmName, setConfirmName] = useState("");
  const { data: eligibility } = useSWR<{ eligible: boolean; reason: string }>(
    open ? "/organization/deletion-eligibility" : null,
    fetcher,
  );
  const { pending, run } = useAction();
  const startOver = useStartOver();

  const remove = () =>
    run("delete", async () => {
      await api("/organization", { method: "DELETE" });
      toast.success("Organization deleted");
      startOver();
    });

  return (
    <Section
      title="Delete organization"
      description="Permanently deletes every project, lead, email and mailbox. This can't be undone."
      className="ring-1 ring-red-200"
      actions={
        <Button
          variant="danger"
          onClick={() => {
            setConfirmName("");
            setOpen(true);
          }}
        >
          Delete
        </Button>
      }
    >
      <Modal open={open} onClose={() => setOpen(false)} title={`Delete ${me.org.name}?`}>
        {!eligibility ? (
          <Skeleton className="h-20 w-full" />
        ) : !eligibility.eligible ? (
          <div className="grid gap-4">
            <p className="rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">{eligibility.reason}</p>
            <div className="flex justify-end">
              <Button variant="outline" onClick={() => setOpen(false)}>
                Close
              </Button>
            </div>
          </div>
        ) : (
          <form
            className="grid gap-4"
            onSubmit={(e) => {
              e.preventDefault();
              remove();
            }}
          >
            <Field label={`Type "${me.org.name}" to confirm`}>
              <Input value={confirmName} onChange={(e) => setConfirmName(e.target.value)} autoFocus />
            </Field>
            <div className="flex justify-end gap-2">
              <Button type="button" variant="outline" onClick={() => setOpen(false)}>
                Cancel
              </Button>
              <Button type="submit" variant="danger" disabled={confirmName !== me.org.name} loading={pending === "delete"}>
                Delete forever
              </Button>
            </div>
          </form>
        )}
      </Modal>
    </Section>
  );
}
