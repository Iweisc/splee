"use client";

import { useParams } from "next/navigation";
import { useState } from "react";
import useSWR from "swr";
import { BookOpen, ExternalLink, Pencil, Plus, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { isUrl } from "@/components/app/settings/is-url";
import { LoadError, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { Badge, Card, EmptyState, Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { Material } from "@/lib/types";

const KINDS: Record<Material["kind"], string> = {
  case_study: "Case study",
  doc: "Document",
  faq: "FAQ",
  link: "Link",
  note: "Note",
};

type Draft = Pick<Material, "kind" | "title" | "content" | "url">;
const EMPTY: Draft = { kind: "case_study", title: "", content: "", url: "" };

export default function MaterialsPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const key = `/projects/${projectId}/materials`;
  const { data, error, mutate } = useSWR<Material[]>(key, fetcher);
  const { pending, run } = useAction();
  // null = closed, "new" = create, otherwise the material being edited.
  const [editing, setEditing] = useState<Material | "new" | null>(null);

  const remove = (m: Material) => {
    if (!confirm(`Delete "${m.title}"?`)) return;
    run(m.id, async () => {
      await api(`/materials/${m.id}`, { method: "DELETE" });
      await mutate();
    }, "Material deleted");
  };

  const addButton = (
    <Button onClick={() => setEditing("new")}>
      <Plus className="size-4" /> Add material
    </Button>
  );

  return (
    <>
      <PageHeader
        title="Materials"
        description="Case studies, FAQs and notes the agent can quote when writing emails and answering replies."
        actions={data && data.length > 0 && addButton}
      />
      <PageBody>
        <SettingsColumn>
          {error ? (
            <LoadError error={error} what="materials" retry={() => mutate()} />
          ) : !data ? (
            <SectionsSkeleton />
          ) : data.length === 0 ? (
            <Card>
              <EmptyState icon={<BookOpen className="size-6" />} title="No materials yet" action={addButton}>
                Add a customer story or the answers to common questions. The agent uses them as facts, so it doesn&apos;t have to guess.
              </EmptyState>
            </Card>
          ) : (
            <Card className="divide-y divide-border">
              {data.map((m) => (
                <div key={m.id} className="flex items-start gap-4 p-5">
                  <div className="min-w-0 flex-1">
                    <div className="flex items-center gap-2">
                      <Badge>{KINDS[m.kind]}</Badge>
                      <p className="truncate font-medium">{m.title}</p>
                    </div>
                    {m.url && (
                      <a href={m.url} target="_blank" rel="noreferrer" className="mt-1.5 inline-flex max-w-full items-center gap-1 truncate text-sm text-muted-foreground hover:text-foreground">
                        <ExternalLink className="size-3.5 shrink-0" />
                        <span className="truncate">{m.url}</span>
                      </a>
                    )}
                    {m.content && <p className="mt-1.5 line-clamp-2 text-sm text-muted-foreground">{m.content}</p>}
                  </div>
                  <div className="flex shrink-0 items-center gap-1">
                    <Button variant="ghost" size="icon" onClick={() => setEditing(m)} aria-label="Edit">
                      <Pencil className="size-4" />
                    </Button>
                    <Button variant="ghost" size="icon" onClick={() => remove(m)} loading={pending === m.id} aria-label="Delete">
                      {pending !== m.id && <Trash2 className="size-4" />}
                    </Button>
                  </div>
                </div>
              ))}
            </Card>
          )}
        </SettingsColumn>
      </PageBody>

      <Modal open={editing !== null} onClose={() => setEditing(null)} title={editing === "new" ? "Add material" : "Edit material"}>
        {editing !== null && (
          <MaterialForm
            key={editing === "new" ? "new" : editing.id}
            initial={editing === "new" ? EMPTY : editing}
            onSubmit={async (draft) => {
              const ok = await run(
                "save",
                async () => {
                  if (editing === "new") await api(key, { body: draft });
                  else await api(`/materials/${editing.id}`, { method: "PATCH", body: draft });
                  await mutate();
                },
                editing === "new" ? "Material added" : "Material saved",
              );
              if (ok) setEditing(null);
            }}
            saving={pending === "save"}
          />
        )}
      </Modal>
    </>
  );
}

function MaterialForm({ initial, onSubmit, saving }: { initial: Draft; onSubmit: (d: Draft) => void; saving: boolean }) {
  const [d, setD] = useState<Draft>({ kind: initial.kind, title: initial.title, content: initial.content, url: initial.url });
  const isLink = d.kind === "link";
  const valid = d.title.trim() && (isLink ? isUrl(d.url.trim()) : d.content.trim());

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (valid) onSubmit({ ...d, title: d.title.trim(), url: d.url.trim() });
      }}
    >
      <div className="grid gap-4 sm:grid-cols-[10rem_1fr]">
        <Field label="Type">
          <Select value={d.kind} onChange={(e) => setD({ ...d, kind: e.target.value as Material["kind"] })}>
            {Object.entries(KINDS).map(([value, label]) => (
              <option key={value} value={value}>
                {label}
              </option>
            ))}
          </Select>
        </Field>
        <Field label="Title">
          <Input value={d.title} onChange={(e) => setD({ ...d, title: e.target.value })} placeholder="e.g. How Acme halved churn" autoFocus />
        </Field>
      </div>
      <Field label={isLink ? "URL" : "URL (optional)"}>
        <Input type="url" value={d.url} onChange={(e) => setD({ ...d, url: e.target.value })} placeholder="https://" />
      </Field>
      <Field label={isLink ? "What's on the page (optional)" : "Content"} hint="Plain facts work best: numbers, names, quotes you have permission to use.">
        <Textarea value={d.content} onChange={(e) => setD({ ...d, content: e.target.value })} rows={7} />
      </Field>
      <div className="flex justify-end">
        <Button type="submit" disabled={!valid} loading={saving}>
          Save
        </Button>
      </div>
    </form>
  );
}
