"use client";

import { useState } from "react";
import { ChevronDown, MapPin, RotateCw, Users } from "lucide-react";
import { useSWRConfig } from "swr";
import { Button } from "@/components/ui/button";
import { Field, Textarea } from "@/components/ui/form";
import { Badge, Card, Favicon, Modal, Skeleton } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Project } from "@/lib/types";
import { cn } from "@/lib/utils";
import { MicroLabel, toastError } from "./states";

export function CompanyProfile({ project, className }: { project: Project; className?: string }) {
  const c = project.company;
  const [open, setOpen] = useState(false);
  // Collapsed by default: the full lists made this side card twice as tall as the campaigns table next to it.
  const [details, setDetails] = useState(false);
  const researched = !!c.description;
  const hasDetails = !!c.products?.length || !!c.value_props?.length;

  return (
    <Card className={cn("p-5", className)}>
      <div className="mb-3 flex items-center justify-between gap-2">
        <MicroLabel>Company profile</MicroLabel>
        <Button variant="ghost" size="sm" onClick={() => setOpen(true)} disabled={project.status === "researching"}>
          {/* No icon on phones, so the label beside it fits on one line down to 320px. */}
          <RotateCw className="size-3.5 max-sm:hidden" /> Re-run research
        </Button>
      </div>
      <div className="flex items-center gap-2.5">
        {project.domain ? <Favicon domain={project.domain} size={24} /> : <span className="size-6 rounded-md bg-black/10" />}
        <div className="min-w-0">
          <p className="truncate font-medium">{c.name || project.name || project.domain}</p>
          {project.domain && <p className="truncate text-xs text-muted-foreground">{project.domain}</p>}
        </div>
      </div>
      {researched ? (
        <>
          <p className={cn("mt-3 text-sm leading-relaxed text-foreground/80", hasDetails && !details && "line-clamp-4")}>{c.description}</p>
          <div className="mt-3 flex flex-wrap gap-1.5">
            {c.employees && (
              <Badge>
                <Users className="size-3" /> {c.employees}
              </Badge>
            )}
            {(c.city || c.country_code) && (
              <Badge>
                <MapPin className="size-3" /> {[c.city, c.country_code].filter(Boolean).join(", ")}
              </Badge>
            )}
            {c.industry && (
              <Badge className="max-w-full" title={c.industry}>
                <span className="truncate">{c.industry}</span>
              </Badge>
            )}
          </div>
          {details && !!c.products?.length && <ProfileList label="Products" items={c.products} />}
          {details && !!c.value_props?.length && <ProfileList label="Value propositions" items={c.value_props} />}
          {hasDetails && (
            <button
              type="button"
              onClick={() => setDetails(!details)}
              className="mt-3 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
            >
              <ChevronDown className={cn("size-3.5 transition-transform", details && "rotate-180")} />
              {details ? "Hide products and value propositions" : "Products and value propositions"}
            </button>
          )}
        </>
      ) : project.status === "researching" ? (
        <div className="mt-3 flex flex-col gap-2">
          <Skeleton className="h-3.5" />
          <Skeleton className="h-3.5 w-4/5" />
        </div>
      ) : (
        <p className="mt-3 text-sm text-muted-foreground">{project.business_description || "No research yet."}</p>
      )}
      <RerunModal projectId={project.id} open={open} onClose={() => setOpen(false)} />
    </Card>
  );
}

function ProfileList({ label, items }: { label: string; items: string[] }) {
  return (
    <div className="mt-4">
      <MicroLabel className="mb-1.5">{label}</MicroLabel>
      <ul className="flex flex-col gap-1 text-sm text-foreground/80">
        {items.map((item) => (
          <li key={item} className="flex gap-2">
            <span className="mt-2 size-1 shrink-0 rounded-full bg-foreground/40" />
            {item}
          </li>
        ))}
      </ul>
    </div>
  );
}

function RerunModal({ projectId, open, onClose }: { projectId: string; open: boolean; onClose: () => void }) {
  const { mutate } = useSWRConfig();
  const [note, setNote] = useState("");
  const [saving, setSaving] = useState(false);

  const submit = async () => {
    setSaving(true);
    try {
      await api(`/projects/${projectId}/research`, { body: note.trim() ? { note: note.trim() } : {} });
      mutate(() => true);
      setNote("");
      onClose();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal open={open} onClose={onClose} title="Re-run research">
      <p className="mb-4 text-sm text-muted-foreground">
        The agent runs the research again from step 1: company, competitors, campaigns, customers, people and emails.
      </p>
      <Field label="What did it get wrong?" hint="Optional. The agent uses this as a correction.">
        <Textarea value={note} onChange={(e) => setNote(e.target.value)} placeholder="We sell to hospitals, not private clinics." />
      </Field>
      <div className="mt-5 flex justify-end gap-2">
        <Button variant="ghost" onClick={onClose}>
          Cancel
        </Button>
        <Button onClick={submit} loading={saving}>
          Re-run research
        </Button>
      </div>
    </Modal>
  );
}
