"use client";

import { useState } from "react";
import { ExternalLink, Link2, Mail, MapPin, Trash2 } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
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

  if (error) return <LoadError error={error} retry={() => mutate()} />;
  if (!lead) return <ListSkeleton rows={5} />;

  const setStatus = async (status: LeadStatus) => {
    try {
      await mutate(api<LeadDetail>(`/leads/${leadId}`, { method: "PATCH", body: { status } }), { revalidate: false });
      mutateAll((k) => typeof k === "string" && k.includes("/leads"));
    } catch (e) {
      toastError(e);
    }
  };

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
        <span className="flex-1" />
        <Button variant="ghost" size="sm" onClick={remove} loading={deleting} className="text-danger">
          <Trash2 className="size-3.5" /> Delete
        </Button>
      </div>

      <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
        <Detail icon={<Mail className="size-3.5" />}>
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

function Detail({ icon, children }: { icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <div className="flex min-w-0 items-center gap-2">
      <span className="text-muted-foreground">{icon}</span>
      <span className="truncate">{children}</span>
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
    </li>
  );
}
