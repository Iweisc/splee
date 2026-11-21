"use client";

import { useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { ListX, Trash2, Upload } from "lucide-react";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError, Pager, PER_PAGE, useLastPageGuard } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Badge, EmptyState, Modal, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api, qs } from "@/lib/api";
import type { Paginated, ProjectSummary, SuppressionList, SuppressionListColumn } from "@/lib/types";
import { cn, num, plural, timeAgo } from "@/lib/utils";

export type Scope = string[] | null;

const KINDS: Record<SuppressionList["kind"], { label: string; one: string; many: string }> = {
  people: { label: "People", one: "email", many: "emails" },
  companies: { label: "Companies", one: "domain", many: "domains" },
};

export function scopeLabel(scope: Scope, projects: ProjectSummary[] | undefined) {
  if (scope === null) return "All projects";
  if (scope.length === 1) return projects?.find((p) => p.id === scope[0])?.name ?? "1 project";
  return plural(scope.length, "project");
}

/** Named lists, like CRM customers or partner agencies, each covering all projects or some. */
export function SuppressionLists() {
  const { data: lists, error, mutate } = useSWR<SuppressionList[]>("/suppression-lists", fetcher);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const [upload, setUpload] = useState<{ name: string; csv: string; columns: SuppressionListColumn[]; column: number | null } | null>(null);
  const [scoping, setScoping] = useState<SuppressionList | null>(null);
  const [viewing, setViewing] = useState<SuppressionList | null>(null);
  const { pending, run } = useAction();

  const pick = (file: File | undefined) =>
    file &&
    run("read", async () => {
      const csv = await file.text();
      const read = await api<{ columns: SuppressionListColumn[]; column: number | null }>("/suppression-lists/columns", { body: { csv } });
      if (read.column === null) throw new Error("That file has no column of emails or domains");
      setUpload({ name: file.name.replace(/\.[^.]+$/, ""), csv, ...read });
    });

  const remove = (l: SuppressionList) => {
    if (!confirm(`Delete "${l.name}" and its ${plural(l.entries, KINDS[l.kind].one, KINDS[l.kind].many)}? Splee can contact them again.`)) return;
    run(l.id, async () => {
      await api(`/suppression-lists/${l.id}`, { method: "DELETE" });
      await mutate();
    });
  };

  return (
    <Section
      title="Lists"
      description="Upload customers or partners as a list of their own, kept apart from unsubscribes and bounces. Point each list at all projects or a few, and delete it in one go."
      actions={
        <label
          className={cn(
            "inline-flex h-9 cursor-pointer items-center gap-1.5 rounded-lg border border-border-strong bg-card px-3 text-sm font-medium hover:bg-accent",
            pending === "read" && "pointer-events-none opacity-50",
          )}
        >
          <Upload className="size-3.5" /> Upload a CSV
          <input
            type="file"
            accept=".csv,text/csv"
            className="sr-only"
            onChange={(e) => {
              pick(e.target.files?.[0]);
              e.target.value = "";
            }}
          />
        </label>
      }
    >
      {error ? (
        <LoadError error={error} what="your lists" retry={() => mutate()} />
      ) : !lists ? (
        <Skeleton className="h-12 w-full" />
      ) : lists.length === 0 ? (
        <p className="text-sm text-muted-foreground text-pretty">
          No lists yet. A CSV with a column of emails blocks those people. A column of domains blocks everyone at those companies.
        </p>
      ) : (
        <ul className="-mx-6 -mb-6 divide-y divide-border border-t border-border">
          {lists.map((l) => (
            <li key={l.id} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-6 py-3 text-sm">
              <div className="min-w-0 flex-1 basis-48">
                <p className="font-medium wrap-anywhere">{l.name}</p>
                <p className="text-xs text-muted-foreground">
                  {plural(l.entries, KINDS[l.kind].one, KINDS[l.kind].many)} · {scopeLabel(l.project_ids, projects)} · {timeAgo(l.created_at)}
                </p>
              </div>
              <Badge tone={l.kind === "people" ? "neutral" : "blue"}>{KINDS[l.kind].label}</Badge>
              <div className="flex shrink-0 items-center gap-1">
                <Button variant="ghost" size="sm" onClick={() => setViewing(l)}>
                  View
                </Button>
                <Button variant="ghost" size="sm" onClick={() => setScoping(l)}>
                  Projects
                </Button>
                <Button variant="ghost" size="icon" className="size-8" onClick={() => remove(l)} loading={pending === l.id} aria-label={`Delete ${l.name}`}>
                  <Trash2 className="size-4" />
                </Button>
              </div>
            </li>
          ))}
        </ul>
      )}

      <Modal open={!!upload} onClose={() => setUpload(null)} title="New list">
        {upload && (
          <UploadForm
            upload={upload}
            projects={projects}
            onDone={async (added, asked) => {
              await mutate();
              setUpload(null);
              const { list } = added;
              const renamed = list.name !== asked ? ` as "${list.name}", since that name was taken` : "";
              toast.success(
                `Saved ${plural(added.added, KINDS[list.kind].one, KINDS[list.kind].many)}${renamed}${added.skipped ? `. Skipped ${num(added.skipped)} invalid or repeated.` : ""}`,
              );
            }}
          />
        )}
      </Modal>
      <Modal open={!!scoping} onClose={() => setScoping(null)} title="Projects this list covers">
        {scoping && (
          <ScopeForm
            list={scoping}
            projects={projects}
            onDone={async () => {
              await mutate();
              setScoping(null);
            }}
          />
        )}
      </Modal>
      <Modal open={!!viewing} onClose={() => setViewing(null)} title={viewing?.name} className="max-w-md">
        {viewing && <Entries list={viewing} />}
      </Modal>
    </Section>
  );
}

type Added = { list: SuppressionList; added: number; skipped: number };

function UploadForm({
  upload,
  projects,
  onDone,
}: {
  upload: { name: string; csv: string; columns: SuppressionListColumn[]; column: number | null };
  projects: ProjectSummary[] | undefined;
  onDone: (added: Added, asked: string) => Promise<void>;
}) {
  const [name, setName] = useState(upload.name);
  const [column, setColumn] = useState(upload.column ?? 0);
  const [scope, setScope] = useState<Scope>(null);
  const { pending, run } = useAction();
  const usable = upload.columns.map((c, i) => ({ ...c, i })).filter((c) => c.kind !== null);
  const picked = upload.columns[column];

  const save = () =>
    run("save", async () => {
      const added = await api<Added>("/suppression-lists", { body: { name, csv: upload.csv, column, project_ids: scope } });
      await onDone(added, name.trim());
    });

  return (
    <form
      className="flex flex-col gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        save();
      }}
    >
      <Field label="Name">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="CRM customers" maxLength={100} required />
      </Field>
      <Field
        label="Column"
        hint={picked?.kind && `${plural(picked.values, KINDS[picked.kind].one, KINDS[picked.kind].many)}, blocking ${picked.kind === "people" ? "those people" : "everyone at those companies"}`}
      >
        <Select value={column} onChange={(e) => setColumn(Number(e.target.value))}>
          {usable.map((c) => (
            <option key={c.i} value={c.i}>
              {c.header || `Column ${c.i + 1}`} ({c.kind && KINDS[c.kind].many})
            </option>
          ))}
        </Select>
      </Field>
      <ScopePicker scope={scope} onChange={setScope} projects={projects} />
      <div className="flex justify-end">
        <Button type="submit" loading={pending === "save"} disabled={!name.trim() || (scope !== null && scope.length === 0)}>
          Save list
        </Button>
      </div>
    </form>
  );
}

function ScopeForm({ list, projects, onDone }: { list: SuppressionList; projects: ProjectSummary[] | undefined; onDone: () => Promise<void> }) {
  const [scope, setScope] = useState<Scope>(list.project_ids);
  const { pending, run } = useAction();
  const save = () =>
    run("save", async () => {
      await api(`/suppression-lists/${list.id}`, { method: "PATCH", body: { project_ids: scope } });
      await onDone();
    });
  return (
    <div className="flex flex-col gap-4">
      <ScopePicker scope={scope} onChange={setScope} projects={projects} />
      <div className="flex justify-end">
        <Button onClick={save} loading={pending === "save"} disabled={scope !== null && scope.length === 0}>
          Save
        </Button>
      </div>
    </div>
  );
}

/** All projects, including ones made later, or the ones ticked. */
export function ScopePicker({ scope, onChange, projects }: { scope: Scope; onChange: (scope: Scope) => void; projects: ProjectSummary[] | undefined }) {
  const toggle = (id: string) => {
    const now = new Set(scope ?? []);
    if (now.has(id)) now.delete(id);
    else now.add(id);
    onChange([...now]);
  };
  return (
    <Field label="Projects" group hint={scope === null ? "New projects are covered too." : scope.length === 0 ? "Pick at least one project." : undefined}>
      <div className="flex flex-col gap-2 text-sm">
        <label className="flex items-center gap-2">
          <input type="radio" checked={scope === null} onChange={() => onChange(null)} className="accent-black" />
          All projects
        </label>
        <label className="flex items-center gap-2">
          <input type="radio" checked={scope !== null} onChange={() => onChange(scope ?? [])} className="accent-black" />
          Only some
        </label>
        {scope !== null && (
          <ul className="max-h-56 divide-y divide-border overflow-y-auto rounded-xl border border-border">
            {(projects ?? []).map((p) => (
              <li key={p.id}>
                <label className="flex cursor-pointer items-center gap-3 px-4 py-2.5 hover:bg-accent">
                  <input type="checkbox" checked={scope.includes(p.id)} onChange={() => toggle(p.id)} className="shrink-0 accent-black" />
                  <span className="min-w-0 flex-1 wrap-anywhere">{p.name}</span>
                </label>
              </li>
            ))}
          </ul>
        )}
      </div>
    </Field>
  );
}

function Entries({ list }: { list: SuppressionList }) {
  const [page, setPage] = useState(1);
  const key = `/suppression-lists/${list.id}/entries${qs({ page, per_page: PER_PAGE })}`;
  const { data, error, mutate } = useSWR<Paginated<string>>(key, fetcher, { keepPreviousData: true });
  useLastPageGuard(data?.total, page, setPage);
  if (error) return <LoadError error={error} what="the list" retry={() => mutate()} />;
  if (!data) return <Skeleton className="h-40 w-full" />;
  if (data.items.length === 0)
    return (
      <EmptyState icon={<ListX className="size-6" />} title="The list is empty">
        Nothing in the file was a valid {KINDS[list.kind].one}.
      </EmptyState>
    );
  return (
    <div className="-mx-6 -mb-6">
      <ul className="max-h-96 divide-y divide-border overflow-y-auto border-t border-border">
        {data.items.map((v) => (
          <li key={v} className="truncate px-6 py-2 font-mono text-[13px]">
            {v}
          </li>
        ))}
      </ul>
      <Pager page={page} total={data.total} onChange={setPage} />
    </div>
  );
}
