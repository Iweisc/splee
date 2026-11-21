"use client";

import { useState } from "react";
import useSWR from "swr";
import { toast } from "sonner";
import { EyeOff, Plus, Trash2, Upload, X } from "lucide-react";
import { Section } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { LoadError } from "@/components/app/work/states";
import { ScopePicker, scopeLabel, type Scope } from "@/components/app/work/suppression-lists";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Textarea } from "@/components/ui/form";
import { EmptyState, Modal, Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { DedupList, ProjectSummary, SuppressionListColumn } from "@/lib/types";
import { cn, num, plural, timeAgo } from "@/lib/utils";

type Added = { list: DedupList; added: number; skipped: number };

/** The organization's dedup lists, each hiding its domains from the company search of all projects or a few. */
export function DedupLists() {
  const { data: lists, error, mutate } = useSWR<DedupList[]>("/dedup/companies", fetcher);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const [making, setMaking] = useState(false);
  const [scoping, setScoping] = useState<DedupList | null>(null);
  const { pending, run } = useAction();

  const remove = (l: DedupList) => {
    if (!confirm(`Delete "${l.name}" and its ${plural(l.size, "domain")}? Those companies show up in searches again.`)) return;
    run(l.id, async () => {
      await api(`/dedup/companies/${l.id}`, { method: "DELETE" });
      await mutate();
    });
  };

  return (
    <Section
      title="Lists"
      actions={
        <Button variant="outline" size="sm" className="h-9" onClick={() => setMaking(true)}>
          <Plus className="size-3.5" /> New list
        </Button>
      }
    >
      {error ? (
        <LoadError error={error} what="your lists" retry={() => mutate()} />
      ) : !lists ? (
        <Skeleton className="h-12 w-full" />
      ) : lists.length === 0 ? (
        <EmptyState icon={<EyeOff className="size-6" />} title="No lists yet">
          Paste domains or upload a CSV with a column of domains or websites.
        </EmptyState>
      ) : (
        <ul className="-mx-6 -mb-6 divide-y divide-border border-t border-border">
          {lists.map((l) => (
            <li key={l.id} className="flex flex-wrap items-center gap-x-3 gap-y-2 px-6 py-3 text-sm">
              <div className="min-w-0 flex-1 basis-48">
                <p className="font-medium wrap-anywhere">{l.name}</p>
                <p className="text-xs text-muted-foreground">
                  {plural(l.size, "domain")} · {scopeLabel(l.project_ids, projects)} · {timeAgo(l.created_at)}
                </p>
              </div>
              <div className="flex shrink-0 items-center gap-1">
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

      <Modal open={making} onClose={() => setMaking(false)} title="New list">
        {making && (
          <NewListForm
            projects={projects}
            onDone={async (added, asked) => {
              await mutate();
              setMaking(false);
              const renamed = added.list.name !== asked ? ` as "${added.list.name}", since that name was taken` : "";
              const skipped = added.skipped ? `. Skipped ${num(added.skipped)} invalid or repeated.` : "";
              toast.success(`Saved ${plural(added.added, "domain")}${renamed}${skipped}`);
            }}
          />
        )}
      </Modal>
      <Modal open={!!scoping} onClose={() => setScoping(null)} title="Projects this list hides companies in">
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
    </Section>
  );
}

type Csv = { file: string; text: string; columns: (SuppressionListColumn & { i: number })[]; column: number };

function NewListForm({ projects, onDone }: { projects: ProjectSummary[] | undefined; onDone: (added: Added, asked: string) => Promise<void> }) {
  const [name, setName] = useState("");
  const [pasted, setPasted] = useState("");
  const [csv, setCsv] = useState<Csv | null>(null);
  const [scope, setScope] = useState<Scope>(null);
  const { pending, run } = useAction();
  const domains = pasted.split(/[\s,;]+/).filter(Boolean);
  const picked = csv?.columns.find((c) => c.i === csv.column);

  const pick = (file: File | undefined) =>
    file &&
    run("read", async () => {
      const text = await file.text();
      const read = await api<{ columns: SuppressionListColumn[] }>("/suppression-lists/columns", { body: { csv: text } });
      const columns = read.columns.map((c, i) => ({ ...c, i })).filter((c) => c.kind === "companies");
      if (columns.length === 0) throw new Error("That file has no column of domains or websites");
      const best = columns.reduce((a, b) => (b.values > a.values ? b : a));
      setCsv({ file: file.name, text, columns, column: best.i });
      if (!name.trim()) setName(file.name.replace(/\.[^.]+$/, ""));
    });

  const save = () =>
    run("save", async () => {
      const body = csv ? { name, csv: csv.text, column: csv.column, project_ids: scope } : { name, domains, project_ids: scope };
      await onDone(await api<Added>("/dedup/companies", { body }), name.trim());
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
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="CRM accounts" maxLength={100} required />
      </Field>
      {csv ? (
        <Field label="Column" hint={picked && `${plural(picked.values, "domain")} from ${csv.file}`}>
          <div className="flex items-center gap-2">
            <Select value={csv.column} onChange={(e) => setCsv({ ...csv, column: Number(e.target.value) })} className="min-w-0 flex-1">
              {csv.columns.map((c) => (
                <option key={c.i} value={c.i}>
                  {c.header || `Column ${c.i + 1}`}
                </option>
              ))}
            </Select>
            <Button variant="ghost" size="icon" type="button" onClick={() => setCsv(null)} aria-label="Remove the file">
              <X className="size-4" />
            </Button>
          </div>
        </Field>
      ) : (
        <>
          <Field label="Domains or websites" hint={domains.length > 0 ? plural(domains.length, "entry", "entries") : "One per line or comma separated."}>
            <Textarea
              value={pasted}
              onChange={(e) => setPasted(e.target.value)}
              placeholder={"acme.com\nhttps://www.globex.com"}
              rows={4}
              className="font-mono text-[13px]"
            />
          </Field>
          <label
            className={cn(
              "-mt-2 inline-flex h-9 cursor-pointer items-center gap-1.5 self-start rounded-lg border border-border-strong bg-card px-3 text-sm font-medium hover:bg-accent",
              pending === "read" && "pointer-events-none opacity-50",
            )}
          >
            <Upload className="size-3.5" /> Or upload a CSV
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
        </>
      )}
      <ScopePicker scope={scope} onChange={setScope} projects={projects} />
      <div className="flex justify-end">
        <Button
          type="submit"
          loading={pending === "save"}
          disabled={!name.trim() || (!csv && domains.length === 0) || (scope !== null && scope.length === 0)}
        >
          Save list
        </Button>
      </div>
    </form>
  );
}

function ScopeForm({ list, projects, onDone }: { list: DedupList; projects: ProjectSummary[] | undefined; onDone: () => Promise<void> }) {
  const [scope, setScope] = useState<Scope>(list.project_ids);
  const { pending, run } = useAction();
  const save = () =>
    run("save", async () => {
      await api(`/dedup/companies/${list.id}`, { method: "PATCH", body: { project_ids: scope } });
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
