"use client";

import { useState } from "react";
import useSWR from "swr";
import { ExternalLink, Plus, TriangleAlert } from "lucide-react";
import { CopyField } from "@/components/app/settings/copy-button";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { Modal, Skeleton } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ProjectDetail, ReportLink, ReportRange } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";

export const RANGE_LABELS: Record<ReportRange, string> = {
  last_7: "Last 7 days",
  last_30: "Last 30 days",
  last_90: "Last 90 days",
  month_to_date: "This month so far",
};

const MAX_RECIPIENTS = 5;

type Draft = Omit<ReportLink, "id" | "project_id" | "recipients" | "expires_at" | "last_viewed_at" | "created_at"> & {
  recipients: string;
  /** Days from now, "never", or "keep" for an edited link's own date. */
  expiry: string;
};

function draftOf(link?: ReportLink): Draft {
  return {
    title: link?.title ?? "",
    intro: link?.intro ?? "",
    brand: link?.brand ?? "project",
    logo_url: link?.logo_url ?? "",
    range_preset: link?.range_preset ?? "last_30",
    show_spend: link?.show_spend ?? false,
    show_mailboxes: link?.show_mailboxes ?? false,
    show_leads: link?.show_leads ?? false,
    email_schedule: link?.email_schedule ?? "none",
    recipients: link?.recipients.map((r) => r.email).join(", ") ?? "",
    expiry: link ? (link.expires_at ? "keep" : "never") : "90",
  };
}

function day(ts: string) {
  return new Date(ts).toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}

/** The Analytics page's "Share with a client": the project's live links, and the form that makes or edits one. */
export function ShareReport({ open, onClose, project }: { open: boolean; onClose: () => void; project: ProjectDetail }) {
  const id = project.project.id;
  const { data: links, error, mutate } = useSWR<ReportLink[]>(open ? `/projects/${id}/report-links` : null, fetcher);
  // null shows the list, a link edits it, "new" makes one. A new link's address shows once in `made`.
  const [editing, setEditing] = useState<ReportLink | "new" | null>(null);
  const [made, setMade] = useState<string | null>(null);
  const { pending, run } = useAction();
  const { me } = useMe();
  const close = () => {
    setEditing(null);
    setMade(null);
    onClose();
  };
  const form = editing ?? (links?.length === 0 ? "new" : null);

  const revoke = (link: ReportLink) => {
    if (!confirm("Revoke this link? It stops working right away, and its emails stop too.")) return;
    run(link.id, async () => {
      await api(`/report-links/${link.id}`, { method: "DELETE" });
      await mutate();
    }, "Link revoked");
  };

  return (
    <Modal open={open} onClose={close} title={made ? "Your link is ready" : form ? (form === "new" ? "Share with a client" : "Edit link") : "Client links"} className="max-w-xl">
      {made ? (
        <div className="grid gap-4">
          <p className="flex gap-2 rounded-lg bg-amber-50 px-3 py-2 text-sm text-amber-800">
            <TriangleAlert className="mt-0.5 size-4 shrink-0" />
            Copy the link now. Splee keeps only a fingerprint of it, so it can&apos;t show it to you again.
          </p>
          <CopyField value={made}>
            <a href={made} target="_blank" rel="noreferrer" className="inline-flex h-8 items-center gap-1.5 rounded-lg border border-border-strong bg-card px-3 text-sm hover:bg-accent">
              <ExternalLink className="size-4" /> Open
            </a>
          </CopyField>
          <div className="flex justify-end">
            <Button onClick={() => setMade(null)}>Done</Button>
          </div>
        </div>
      ) : error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !links ? (
        <Skeleton className="h-32 w-full" />
      ) : form ? (
        <LinkForm
          key={form === "new" ? "new" : form.id}
          project={project}
          link={form === "new" ? undefined : form}
          onCancel={links.length > 0 ? () => setEditing(null) : close}
          onSaved={async (url) => {
            await mutate();
            setEditing(null);
            if (url) setMade(url);
          }}
        />
      ) : (
        <div className="grid gap-4">
          <p className="text-sm text-muted-foreground">
            Each link opens a read-only page of this project&apos;s results. New links show the address once, so copy it when you make it.
          </p>
          <ul className="-my-1 divide-y divide-border">
            {links.map((l) => (
              <li key={l.id} className="flex flex-col gap-3 py-3 text-sm sm:flex-row sm:items-start">
                <div className="min-w-0 flex-1">
                  <p className="font-medium break-words">{l.title || `${l.brand === "org" ? (me?.org.name ?? "") : project.project.name} outreach report`}</p>
                  <p className="mt-0.5 text-xs text-muted-foreground">
                    {RANGE_LABELS[l.range_preset]}
                    {l.email_schedule !== "none" && (
                      <>
                        {" · "}
                        <span className="whitespace-nowrap">
                          emailed {l.email_schedule} to {plural(l.recipients.filter((r) => !r.unsubscribed).length, "person", "people")}
                        </span>
                      </>
                    )}
                  </p>
                  <p className="text-xs text-muted-foreground">
                    <span className="whitespace-nowrap">Made {timeAgo(l.created_at)} ·</span>{" "}
                    <span className="whitespace-nowrap">{l.last_viewed_at ? `viewed ${timeAgo(l.last_viewed_at)}` : "not viewed yet"}</span>
                    {l.expires_at && (
                      <>
                        {" "}
                        <span className="whitespace-nowrap">· expires {day(l.expires_at)}</span>
                      </>
                    )}
                  </p>
                </div>
                <div className="flex shrink-0 gap-2">
                  <Button variant="outline" size="sm" onClick={() => setEditing(l)}>
                    Edit
                  </Button>
                  <Button variant="outline" size="sm" onClick={() => revoke(l)} loading={pending === l.id}>
                    Revoke
                  </Button>
                </div>
              </li>
            ))}
          </ul>
          <div className="flex justify-end">
            <Button onClick={() => setEditing("new")}>
              <Plus className="size-4" /> New link
            </Button>
          </div>
        </div>
      )}
    </Modal>
  );
}

function LinkForm({
  project,
  link,
  onCancel,
  onSaved,
}: {
  project: ProjectDetail;
  link?: ReportLink;
  onCancel: () => void;
  onSaved: (url?: string) => Promise<void>;
}) {
  const { me } = useMe();
  const [d, setD] = useState(() => draftOf(link));
  const { pending, run } = useAction();
  const set = <K extends keyof Draft>(key: K, value: Draft[K]) => setD((d) => ({ ...d, [key]: value }));
  const recipients = d.recipients.split(/[\s,;]+/).filter(Boolean);
  const unsubscribed = link?.recipients.filter((r) => r.unsubscribed && recipients.includes(r.email)) ?? [];

  const save = () =>
    run("save", async () => {
      const expires_at =
        d.expiry === "keep" ? (link?.expires_at ?? null) : d.expiry === "never" ? null : new Date(Date.now() + Number(d.expiry) * 86_400_000).toISOString();
      const body = { ...d, expiry: undefined, recipients: d.email_schedule === "none" ? [] : recipients, expires_at };
      if (link) {
        await api(`/report-links/${link.id}`, { method: "PATCH", body });
        await onSaved();
      } else {
        const res = await api<{ url: string }>(`/projects/${project.project.id}/report-links`, { body });
        await onSaved(res.url);
      }
    }, link ? "Link saved" : undefined);

  const check = (key: "show_spend" | "show_mailboxes" | "show_leads", label: string, hint: string) => (
    <label className="flex items-start gap-2.5 text-sm">
      <input type="checkbox" checked={d[key]} onChange={(e) => set(key, e.target.checked)} className="mt-1 shrink-0 accent-black" />
      <span>
        <span className="font-medium">{label}</span>
        <span className="block text-xs text-muted-foreground">{hint}</span>
      </span>
    </label>
  );

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Name and logo">
          <Select value={d.brand} onChange={(e) => set("brand", e.target.value as Draft["brand"])}>
            <option value="project">{project.project.name}</option>
            <option value="org">{me?.org.name ?? "Your organization"}</option>
          </Select>
        </Field>
        <Field label="Dates">
          <Select value={d.range_preset} onChange={(e) => set("range_preset", e.target.value as ReportRange)}>
            {Object.entries(RANGE_LABELS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
        </Field>
      </div>
      <Field label="Title">
        <Input value={d.title} onChange={(e) => set("title", e.target.value)} placeholder={`${project.project.name} outreach report`} maxLength={100} />
      </Field>
      <Field label="Note for your client">
        <Textarea value={d.intro} onChange={(e) => set("intro", e.target.value)} rows={2} maxLength={1000} className="min-h-0" placeholder="What you're focusing on this month, or anything they should know." />
      </Field>
      <Field label="Logo address" hint={d.brand === "project" ? "Leave it empty to use the project website's icon." : "Leave it empty to show the name only."}>
        <Input value={d.logo_url} onChange={(e) => set("logo_url", e.target.value)} placeholder="https://" inputMode="url" />
      </Field>
      <div className="grid gap-3">
        {check("show_spend", "Show spend", "Claude and email costs, and the cost per meeting.")}
        {check("show_mailboxes", "Show mailboxes", "The addresses you send from and how each one does.")}
        {check("show_leads", "List meetings and hot replies", "First name, company and the start of each reply. Lead emails never show.")}
      </div>
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Email a summary">
          <Select value={d.email_schedule} onChange={(e) => set("email_schedule", e.target.value as Draft["email_schedule"])}>
            <option value="none">Don&apos;t email</option>
            <option value="weekly">Every Monday morning</option>
            <option value="monthly">On the 1st of each month</option>
          </Select>
        </Field>
        <Field label="Link expires">
          <Select value={d.expiry} onChange={(e) => set("expiry", e.target.value)}>
            {link?.expires_at && <option value="keep">On {day(link.expires_at)}</option>}
            <option value="30">In 30 days</option>
            <option value="90">In 90 days</option>
            <option value="365">In a year</option>
            <option value="never">Never</option>
          </Select>
        </Field>
      </div>
      {d.email_schedule !== "none" && (
        <Field
          label="Send it to"
          error={recipients.length > MAX_RECIPIENTS ? `Up to ${MAX_RECIPIENTS} addresses` : null}
          hint={
            unsubscribed.length > 0
              ? `${unsubscribed.map((r) => r.email).join(", ")} unsubscribed and won't get it.`
              : `Up to ${MAX_RECIPIENTS} addresses, each with its own unsubscribe link. ${d.email_schedule === "weekly" ? "Weeks" : "Months"} with nothing sent are skipped.`
          }
        >
          <Textarea value={d.recipients} onChange={(e) => set("recipients", e.target.value)} rows={2} className="min-h-0" placeholder="client@company.com, ceo@company.com" />
        </Field>
      )}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="outline" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="submit" loading={pending === "save"} disabled={d.email_schedule !== "none" && (recipients.length === 0 || recipients.length > MAX_RECIPIENTS)}>
          {link ? "Save" : "Make link"}
        </Button>
      </div>
    </form>
  );
}
