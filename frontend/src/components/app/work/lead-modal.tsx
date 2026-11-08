"use client";

import { useState } from "react";
import { ExternalLink, Link2, Mail, MapPin, Pencil, Trash2 } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { XIcon } from "@/components/marketing/x-icon";
import { Avatar, Badge, Favicon, Modal } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { EmailMessage, LeadDetail, LeadStatus } from "@/lib/types";
import { cn, dateTime } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, MicroLabel, toastError } from "./states";
import { EmailStatusBadge, LEAD_STATUS } from "./status";

export function LeadModal({ leadId, onClose }: { leadId: string | null; onClose: () => void }) {
  return (
    <Modal open={!!leadId} onClose={onClose} className="max-w-2xl">
      {leadId && <LeadBody leadId={leadId} onClose={onClose} />}
    </Modal>
  );
}

function LeadBody({ leadId, onClose }: { leadId: string; onClose: () => void }) {
  const { mutate: mutateAll } = useSWRConfig();
  const { data: lead, error, mutate } = useSWR<LeadDetail>(`/leads/${leadId}`, fetcher);
  const [deleting, setDeleting] = useState(false);
  const [editing, setEditing] = useState(false);

  if (error) return <LoadError error={error} retry={() => mutate()} />;
  if (!lead) return <ListSkeleton rows={5} />;

  const update = async (changes: Record<string, string>) => {
    await mutate(api<LeadDetail>(`/leads/${leadId}`, { method: "PATCH", body: changes }), { revalidate: false });
    mutateAll((k) => typeof k === "string" && k.includes("/leads"));
  };

  const setStatus = async (status: LeadStatus) => {
    try {
      await update({ status });
    } catch (e) {
      toastError(e);
    }
  };

  if (editing) {
    return (
      <LeadForm
        lead={lead}
        onCancel={() => setEditing(false)}
        onSave={async (changes) => {
          await update(changes);
          toast.success("Lead updated");
          setEditing(false);
        }}
      />
    );
  }

  const remove = async () => {
    if (!confirm(`Delete ${lead.full_name || lead.email}? Their emails are deleted too.`)) return;
    setDeleting(true);
    try {
      await api(`/leads/${leadId}`, { method: "DELETE" });
      mutateAll(() => true);
      toast.success("Lead deleted");
      onClose();
    } catch (e) {
      toastError(e);
      setDeleting(false);
    }
  };

  const location = [lead.city, lead.country_code].filter(Boolean).join(", ");

  return (
    <div className="flex flex-col gap-5">
      <div className="flex items-start gap-3 pr-6">
        <Avatar name={lead.full_name || lead.email || "?"} className="size-10" />
        <div className="min-w-0 flex-1">
          <p className="text-lg font-semibold">{lead.full_name || lead.email}</p>
          <p className="text-sm text-muted-foreground">
            {[lead.title, lead.company_name].filter(Boolean).join(" at ")}
          </p>
        </div>
      </div>

      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Select value={lead.status} onChange={(e) => setStatus(e.target.value as LeadStatus)} className="h-8 w-auto">
          {Object.entries(LEAD_STATUS).map(([value, s]) => (
            <option key={value} value={value}>
              {s.label}
            </option>
          ))}
        </Select>
        {lead.segment_name && <Badge>{lead.segment_name}</Badge>}
        <Badge className="capitalize">{lead.source}</Badge>
        {/* Edit and Delete wrap together, so Delete never sits alone on a row. On phones they share a row of their own. */}
        <div className="ml-auto flex gap-1 max-sm:basis-full max-sm:[&>*]:flex-1">
          <Button variant="ghost" size="sm" onClick={() => setEditing(true)}>
            <Pencil className="size-3.5" /> Edit
          </Button>
          <Button variant="ghost" size="sm" onClick={remove} loading={deleting} className="text-danger">
            <Trash2 className="size-3.5" /> Delete
          </Button>
        </div>
      </div>

      <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
        <Detail icon={<Mail className="size-3.5" />} wide>
          {lead.email ? (
            <>
              {lead.email} <span className="text-xs text-muted-foreground">({lead.email_status})</span>
            </>
          ) : (
            <span className="text-muted-foreground">No email found</span>
          )}
        </Detail>
        {lead.linkedin_url && (
          <Detail icon={<Link2 className="size-3.5" />}>
            <a href={lead.linkedin_url} target="_blank" rel="noreferrer" className="hover:underline">
              LinkedIn profile
            </a>
          </Detail>
        )}
        {lead.x_url && (
          <Detail icon={<XIcon className="size-3.5" />}>
            <a href={lead.x_url} target="_blank" rel="noreferrer" className="hover:underline">
              X profile
            </a>
          </Detail>
        )}
        {lead.company_domain && (
          <Detail icon={<Favicon domain={lead.company_domain} size={14} />}>
            <a href={`https://${lead.company_domain}`} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 hover:underline">
              {lead.company_domain} <ExternalLink className="size-3" />
            </a>
          </Detail>
        )}
        {location && <Detail icon={<MapPin className="size-3.5" />}>{location}</Detail>}
      </dl>

      {(lead.personalization.hook || !!lead.personalization.facts?.length || lead.company?.fit_reason) && (
        <div className="rounded-xl bg-muted p-4">
          <MicroLabel className="mb-2">Personalization notes</MicroLabel>
          {lead.personalization.hook && <p className="text-sm">{lead.personalization.hook}</p>}
          {!!lead.personalization.facts?.length && (
            <ul className="mt-2 flex list-disc flex-col gap-1 pl-4 text-sm text-foreground/80">
              {lead.personalization.facts.map((f) => (
                <li key={f}>{f}</li>
              ))}
            </ul>
          )}
          {lead.company?.fit_reason && <p className="mt-2 text-sm text-muted-foreground">Why this company: {lead.company.fit_reason}</p>}
        </div>
      )}

      <div>
        <MicroLabel className="mb-2">Emails</MicroLabel>
        {lead.emails.length === 0 ? (
          <p className="text-sm text-muted-foreground">No emails yet.</p>
        ) : (
          <ol className="flex flex-col gap-2">
            {lead.emails.map((m) => (
              <ThreadItem key={m.id} message={m} />
            ))}
          </ol>
        )}
      </div>
    </div>
  );
}

const FORM_FIELDS = [
  { key: "first_name", label: "First name" },
  { key: "last_name", label: "Last name" },
  { key: "title", label: "Title" },
  { key: "company_name", label: "Company" },
  { key: "email", label: "Email", type: "email" },
  { key: "linkedin_url", label: "LinkedIn profile", type: "url", placeholder: "https://www.linkedin.com/in/…" },
  { key: "x_url", label: "X profile", placeholder: "@handle or https://x.com/…" },
] as const;

type FormKey = (typeof FORM_FIELDS)[number]["key"];

/** Fixes what research got wrong. */
function LeadForm({ lead, onCancel, onSave }: { lead: LeadDetail; onCancel: () => void; onSave: (changes: Record<string, string>) => Promise<void> }) {
  const [values, setValues] = useState<Record<FormKey, string>>(() => {
    const v = {} as Record<FormKey, string>;
    for (const f of FORM_FIELDS) v[f.key] = lead[f.key] ?? "";
    return v;
  });
  const [saving, setSaving] = useState(false);
  const unsent = lead.emails.some((m) => m.direction === "outbound" && ["draft", "approved", "scheduled"].includes(m.status));

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const changes: Record<string, string> = {};
    for (const f of FORM_FIELDS) {
      if (values[f.key].trim() !== (lead[f.key] ?? "")) changes[f.key] = values[f.key].trim();
    }
    if ("first_name" in changes || "last_name" in changes) {
      changes.full_name = `${values.first_name.trim()} ${values.last_name.trim()}`.trim();
    }
    if (Object.keys(changes).length === 0) return onCancel();
    setSaving(true);
    try {
      await onSave(changes);
    } catch (err) {
      toastError(err);
      setSaving(false);
    }
  };

  return (
    <form onSubmit={submit} className="flex flex-col gap-5">
      <p className="pr-6 text-lg font-semibold">Edit {lead.full_name || lead.email}</p>
      <div className="grid gap-4 sm:grid-cols-2">
        {FORM_FIELDS.map((f) => (
          <Field key={f.key} label={f.label}>
            <Input
              type={"type" in f ? f.type : "text"}
              value={values[f.key]}
              placeholder={"placeholder" in f ? f.placeholder : undefined}
              onChange={(e) => setValues((v) => ({ ...v, [f.key]: e.target.value }))}
            />
          </Field>
        ))}
      </div>
      <div className="flex items-center justify-end gap-2">
        {unsent && <p className="mr-auto text-xs text-muted-foreground">Emails already written keep the old details. Rewrite them on the Emails page.</p>}
        <Button type="button" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" loading={saving}>
          Save
        </Button>
      </div>
    </form>
  );
}

/** `wide` is for the email: it takes the whole row and wraps rather than being cut off, since it's the one detail people copy. */
function Detail({ icon, wide, children }: { icon: React.ReactNode; wide?: boolean; children: React.ReactNode }) {
  return (
    <div className={cn("flex min-w-0 items-start gap-2", wide && "sm:col-span-2")}>
      {/* As tall as one line of text, so the icon lines up with the first line when the text wraps. */}
      <span className="flex h-5 items-center text-muted-foreground">{icon}</span>
      <span className={wide ? "break-all" : "truncate"}>{children}</span>
    </div>
  );
}

function ThreadItem({ message: m }: { message: EmailMessage }) {
  const inbound = m.direction === "inbound";
  return (
    <li className={cn("rounded-xl border border-border p-3", inbound ? "bg-card" : "bg-muted/60")}>
      <div className="mb-1 flex items-center justify-between gap-2 text-xs text-muted-foreground">
        <span className="truncate">
          {inbound ? "Reply" : m.kind === "follow_up" ? `Follow-up ${m.step}` : m.kind.replace("_", " ")} · {m.subject}
        </span>
        <span className="flex shrink-0 items-center gap-2">
          {!inbound && <EmailStatusBadge status={m.status} />}
          {dateTime(m.sent_at ?? m.scheduled_for ?? m.created_at)}
        </span>
      </div>
      <p className="line-clamp-4 text-sm whitespace-pre-wrap">{m.body}</p>
      {/* The sender records why it gave up: an SMTP error, the do-not-contact list, a domain that can't take mail. */}
      {!inbound && m.error && (m.status === "failed" || m.status === "cancelled") && (
        <p className={cn("mt-2 text-xs", m.status === "failed" ? "text-red-700" : "text-muted-foreground")}>Not sent: {m.error}</p>
      )}
    </li>
  );
}
