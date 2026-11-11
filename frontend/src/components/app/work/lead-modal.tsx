"use client";

import { useEffect, useRef, useState } from "react";
import { Briefcase, CalendarCheck, ExternalLink, Link2, Mail, MapPin, Pencil, Smartphone, Trash2 } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { XIcon } from "@/components/marketing/x-icon";
import { Address, Avatar, Badge, Favicon, Message, Modal } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { EmailMessage, Lead, LeadDetail, LeadStatus } from "@/lib/types";
import { cn, dateTime, dayTime } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { ListSkeleton, LoadError, MicroLabel, toastError } from "./states";
import { LeadDeal } from "./deal-dialog";
import { OwnerMenu } from "./owner-menu";
import { ScoreBar, ScoreReasons } from "./score";
import { searchCost } from "./email-search";
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
  const [removing, setRemoving] = useState(false);
  const [editing, setEditing] = useState(false);
  // Editing and deleting replace the whole window's content, which dropped keyboard focus on the page. Coming back, the
  // button that opened them takes it again.
  const [back, setBack] = useState<"edit" | "remove" | null>(null);

  if (error) return <LoadError error={error} retry={() => mutate()} />;
  if (!lead) return <ListSkeleton rows={5} />;

  // Shows the change at once and puts it back if the save fails, so a picked status doesn't snap back while it saves.
  const update = async (changes: Record<string, string>) => {
    await mutate(api<LeadDetail>(`/leads/${leadId}`, { method: "PATCH", body: changes }), {
      optimisticData: { ...lead, ...changes },
      revalidate: false,
    });
    mutateAll((k) => typeof k === "string" && k.includes("/leads"));
  };

  const setStatus = async (status: LeadStatus) => {
    try {
      await update({ status });
    } catch (e) {
      toastError(e);
    }
  };

  const setOwner = async (owner_id: string | null) => {
    try {
      await mutate(api<LeadDetail>(`/leads/${leadId}/owner`, { method: "PUT", body: { owner_id } }), {
        optimisticData: { ...lead, owner_id },
        revalidate: false,
      });
      mutateAll((k) => typeof k === "string" && (k.includes("/leads") || k.startsWith("/inbox") || k.startsWith("/pipeline")));
    } catch (e) {
      toastError(e);
    }
  };

  if (editing) {
    return (
      <LeadForm
        lead={lead}
        onCancel={() => {
          setEditing(false);
          setBack("edit");
        }}
        onSave={async (changes) => {
          await update(changes);
          toast.success("Lead updated");
          setEditing(false);
          setBack("edit");
        }}
      />
    );
  }

  if (removing) {
    return (
      <RemoveLead
        lead={lead}
        onCancel={() => {
          setRemoving(false);
          setBack("remove");
        }}
        onRemoved={() => {
          mutateAll(() => true);
          onClose();
        }}
      />
    );
  }

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
        <Select value={lead.status} onChange={(e) => setStatus(e.target.value as LeadStatus)} className="h-8 w-auto max-sm:grow" aria-label="Status">
          {Object.entries(LEAD_STATUS).map(([value, s]) => (
            <option key={value} value={value}>
              {s.label}
            </option>
          ))}
        </Select>
        <OwnerMenu ownerId={lead.owner_id} ownerName={lead.owner_name} onPick={setOwner} className="max-sm:grow" />
        {/* The campaign and source wrap together too, so the source never sits alone under the status. On phones the status stretches to fill the row they leave. */}
        {/* max-w-full on both, so a long campaign name truncates instead of running out of the window on a phone. */}
        <div className="flex max-w-full flex-wrap gap-2">
          {lead.segment_name && (
            <Badge className="max-w-full" title={lead.segment_name}>
              <span className="truncate">{lead.segment_name}</span>
            </Badge>
          )}
          <Badge className="capitalize">{lead.source === "linkedin" ? "LinkedIn" : lead.source}</Badge>
        </div>
        {/* Edit and Delete wrap together, so Delete never sits alone on a row. On phones they share a row of their own. */}
        <div className="ml-auto flex gap-1 max-sm:basis-full max-sm:[&>*]:flex-1">
          <Button variant="ghost" size="sm" onClick={() => setEditing(true)} autoFocus={back === "edit"}>
            <Pencil className="size-3.5" /> Edit
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setRemoving(true)} autoFocus={back === "remove"} className="text-danger">
            <Trash2 className="size-3.5" /> Delete
          </Button>
        </div>
      </div>

      <dl className="grid gap-x-6 gap-y-2 text-sm sm:grid-cols-2">
        <Detail label="Email" icon={<Mail className="size-3.5" />} wide>
          {lead.email ? (
            <>
              <Address email={lead.email} /> <span className="text-xs text-muted-foreground">({lead.email_status})</span>
            </>
          ) : (
            <span className="text-muted-foreground">{lead.email_search === "queued" ? "Finding an email…" : "No email found"}</span>
          )}
          {lead.email_search && lead.email_search !== "queued" && (
            <p className="text-xs text-muted-foreground">{[lead.email_search_note, searchCost(lead)].filter(Boolean).join(" ")}</p>
          )}
        </Detail>
        {lead.phone && (
          <Detail label="Phone" icon={<Smartphone className="size-3.5" />}>
            <a href={`tel:${lead.phone}`} className="tabular-nums hover:underline">
              {lead.phone}
            </a>
          </Detail>
        )}
        {lead.linkedin_url && (
          <Detail label="LinkedIn" icon={<Link2 className="size-3.5" />}>
            <a href={lead.linkedin_url} target="_blank" rel="noreferrer" className="hover:underline">
              LinkedIn profile
            </a>
          </Detail>
        )}
        {lead.x_url && (
          <Detail label="X" icon={<XIcon className="size-3.5" />}>
            <a href={lead.x_url} target="_blank" rel="noreferrer" className="hover:underline">
              X profile
            </a>
          </Detail>
        )}
        {lead.company_domain && (
          <Detail label="Website" icon={<Favicon domain={lead.company_domain} size={14} />}>
            <a href={`https://${lead.company_domain}`} target="_blank" rel="noreferrer" className="inline-flex items-center gap-1 hover:underline">
              {lead.company_domain} <ExternalLink className="size-3" />
            </a>
          </Detail>
        )}
        {location && (
          <Detail label="Location" icon={<MapPin className="size-3.5" />}>
            {location}
          </Detail>
        )}
        {lead.meeting_at && (
          <Detail label="Meeting" icon={<CalendarCheck className="size-3.5" />}>
            Meeting on {dayTime(lead.meeting_at)}
          </Detail>
        )}
      </dl>

      <div>
        <MicroLabel className="mb-2">Score</MicroLabel>
        <div className="mb-2 flex items-center gap-3" title={`Fit ${lead.score_fit} of 50, intent ${lead.score_intent} of 50`}>
          <span className="text-lg font-semibold tabular-nums">{lead.score}</span>
          <ScoreBar fit={lead.score_fit} intent={lead.score_intent} className="w-24" />
        </div>
        <ScoreReasons item={lead} />
      </div>

      <div className="text-sm">
        <MicroLabel className="mb-2">Deal</MicroLabel>
        <LeadDeal leadId={lead.id} deal={lead.deal} onChange={() => mutate()} />
      </div>

      <div>
        <MicroLabel className="mb-2">Notes</MicroLabel>
        <LeadNotes key={lead.id} lead={lead} onSaved={() => mutateAll((k) => typeof k === "string" && (k.includes("/leads") || k.startsWith("/inbox")))} />
      </div>

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

      {lead.hiring.length > 0 && (
        <div>
          <MicroLabel className="mb-2">Hiring</MicroLabel>
          <ul className="flex flex-col gap-1.5 text-sm">
            {lead.hiring.map((post) => (
              <li key={post.url} className="flex min-w-0 items-center gap-2">
                <Briefcase className="size-3.5 shrink-0 text-sky-700" />
                <a href={post.url} target="_blank" rel="noreferrer" className="inline-flex min-w-0 items-center gap-1 hover:underline">
                  <span className="truncate">{post.title}</span> <ExternalLink className="size-3 shrink-0" />
                </a>
              </li>
            ))}
          </ul>
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
  { key: "phone", label: "Phone", type: "tel", placeholder: "+44 7700 900123" },
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
        {FORM_FIELDS.map((f, i) => (
          <Field key={f.key} label={f.label}>
            <Input
              autoFocus={i === 0}
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

/** Asks before deleting, and offers to block the address too, for people who asked to be erased. */
function RemoveLead({ lead, onCancel, onRemoved }: { lead: LeadDetail; onCancel: () => void; onRemoved: () => void }) {
  const [forget, setForget] = useState(false);
  const [deleting, setDeleting] = useState(false);
  const name = lead.full_name || lead.email;

  const remove = async () => {
    setDeleting(true);
    try {
      await api(`/leads/${lead.id}${forget ? "?forget=true" : ""}`, { method: "DELETE" });
      toast.success(forget ? `${name} deleted and blocked` : "Lead deleted");
      onRemoved();
    } catch (e) {
      toastError(e);
      setDeleting(false);
    }
  };

  return (
    <div className="flex flex-col gap-5">
      <div className="pr-6">
        <p className="text-lg font-semibold">Delete {name}?</p>
        <p className="mt-1 text-sm text-muted-foreground">Their emails, notes and webhook history go too. You can&apos;t undo this.</p>
      </div>
      {lead.email && (
        <label className="flex items-start gap-2.5 rounded-lg border border-border px-3 py-2.5">
          <input type="checkbox" checked={forget} onChange={(e) => setForget(e.target.checked)} className="mt-0.5 accent-black" />
          <span>
            <span className="block text-sm font-medium">
              Never contact <Address email={lead.email} /> again
            </span>
            <span className="block text-xs text-muted-foreground">Adds them to Do-not-contact, so no import or search brings them back.</span>
          </span>
        </label>
      )}
      <div className="flex justify-end gap-2">
        {/* Focus starts on Cancel, so Enter right after opening this doesn't delete. */}
        <Button variant="ghost" onClick={onCancel} autoFocus>
          Cancel
        </Button>
        <Button variant="danger" onClick={remove} loading={deleting}>
          <Trash2 className="size-4" /> Delete
        </Button>
      </div>
    </div>
  );
}

/** Your team's own notes on a lead. Saves when you leave the box. */
export function LeadNotes({ lead, onSaved }: { lead: Lead; onSaved: () => unknown }) {
  const [notes, setNotes] = useState(lead.notes);
  // What the box last loaded or saved. Text that differs from it is an edit you haven't saved yet.
  const [base, setBase] = useState(lead.notes);
  // A new note from the server shows up unless you're midway through your own. Only a change to the stored note
  // counts, so the old one doesn't flash back between your save and the refresh.
  const [stored, setStored] = useState(lead.notes);
  if (lead.notes !== stored) {
    setStored(lead.notes);
    if (notes === base) {
      setBase(lead.notes);
      setNotes(lead.notes);
    }
  }

  // The newest note the server has, from loading it or from our last save. Each save compares against it when it
  // runs, so leaving the box unchanged never puts back older text, and clearing it still saves.
  const known = useRef(lead.notes);
  useEffect(() => {
    known.current = lead.notes;
  }, [lead.notes]);
  // Saves run one after another, so an earlier save that answers late can't overwrite a later one.
  const queue = useRef(Promise.resolve());
  const save = () => {
    queue.current = queue.current.then(saveNow);
  };
  const saveNow = async () => {
    const value = notes.trim();
    if (value === known.current) return;
    try {
      await api(`/leads/${lead.id}`, { method: "PATCH", body: { notes: value } });
      known.current = value;
      setBase(value);
      // Show what was saved, unless you kept typing while it saved.
      setNotes((current) => (current === notes ? value : current));
      await onSaved();
      toast.success("Notes saved");
    } catch (e) {
      toastError(e);
    }
  };

  // Grows from one line to six whole lines, then scrolls. 1.125rem is the padding plus border.
  return (
    <Textarea
      value={notes}
      onChange={(e) => setNotes(e.target.value)}
      onBlur={save}
      // Escape closes the lead window without blurring the box first, so save before it goes.
      onKeyDown={(e) => e.key === "Escape" && save()}
      aria-label="Notes"
      placeholder="Add a note for your team"
      className="max-h-[calc(6lh+1.125rem)] min-h-[calc(1lh+1.125rem)] resize-none field-sizing-content"
    />
  );
}

/** `wide` is for the email: it takes the whole row and wraps rather than being cut off, since it's the one detail people copy. */
/** One row of the details list. The icon is the term on screen; screen readers get the label instead. */
function Detail({ label, icon, wide, children }: { label: string; icon: React.ReactNode; wide?: boolean; children: React.ReactNode }) {
  return (
    <div className={cn("flex min-w-0 items-start gap-2", wide && "sm:col-span-2")}>
      {/* As tall as one line of text, so the icon lines up with the first line when the text wraps. */}
      <dt className="flex h-5 items-center text-muted-foreground">
        <span aria-hidden>{icon}</span>
        <span className="sr-only">{label}</span>
      </dt>
      <dd className={wide ? "wrap-anywhere" : "truncate"}>{children}</dd>
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
      <p className="line-clamp-4 text-sm whitespace-pre-wrap">
        <Message text={m.body} />
      </p>
      {/* The sender records why it gave up: an SMTP error, the do-not-contact list, a domain that can't take mail. */}
      {!inbound && m.error && (m.status === "failed" || m.status === "cancelled") && (
        <p className={cn("mt-2 text-xs", m.status === "failed" ? "text-red-700" : "text-muted-foreground")}>Not sent: {m.error}</p>
      )}
    </li>
  );
}
