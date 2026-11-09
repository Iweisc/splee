"use client";

import { use, useState } from "react";
import { CircleCheck, FileUp, Upload } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { ListSkeleton, LoadError, MicroLabel, Td, Th, toastError, Tr } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Select } from "@/components/ui/form";
import { Card, EmptyState, FileName, Spinner } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Import, SegmentWithStats } from "@/lib/types";
import { dateTime, num } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

type Preview = { headers: string[]; rows: string[][]; suggested_mapping: Record<string, string> };

// Lead columns the backend import accepts as mapping keys.
const FIELDS: { key: string; label: string }[] = [
  { key: "email", label: "Email" },
  { key: "full_name", label: "Full name" },
  { key: "first_name", label: "First name" },
  { key: "last_name", label: "Last name" },
  { key: "title", label: "Job title" },
  { key: "company_name", label: "Company" },
  { key: "company_domain", label: "Company domain" },
  { key: "linkedin_url", label: "LinkedIn URL" },
  { key: "city", label: "City" },
  { key: "country_code", label: "Country code" },
];

export default function ImportPage({ params }: PageProps<"/app/p/[projectId]/import">) {
  const { projectId } = use(params);
  const [file, setFile] = useState<File | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [result, setResult] = useState<Import | null>(null);
  const [loading, setLoading] = useState(false);

  const choose = async (f: File | undefined) => {
    if (!f) return;
    setResult(null);
    setLoading(true);
    try {
      const form = new FormData();
      form.append("file", f);
      const p = await api<Preview>(`/projects/${projectId}/import/preview`, { body: form });
      setFile(f);
      setPreview(p);
    } catch (e) {
      toastError(e);
    } finally {
      setLoading(false);
    }
  };

  const reset = () => {
    setFile(null);
    setPreview(null);
  };

  return (
    <>
      <PageHeader
        title="Import leads"
        description="Upload a CSV with at least an email column. Suppressed, duplicate and invalid emails are skipped."
      />
      <PageBody className="flex flex-col gap-4">
        {result && <ImportResult result={result} />}
        {file && preview ? (
          <MappingStep
            projectId={projectId}
            file={file}
            preview={preview}
            onCancel={reset}
            onImported={(r) => {
              setResult(r);
              reset();
            }}
          />
        ) : (
          <Card>
            <label className="flex cursor-pointer flex-col items-center gap-2 rounded-2xl border border-dashed border-border-strong px-6 py-12 text-center hover:bg-accent">
              {loading ? <Spinner className="size-5" /> : <FileUp className="size-5 text-muted-foreground" />}
              <span className="font-medium">{loading ? "Reading file…" : "Choose a CSV file"}</span>
              <span className="text-sm text-muted-foreground">You map the columns in the next step.</span>
              <input
                type="file"
                accept=".csv,text/csv"
                className="sr-only"
                disabled={loading}
                onChange={(e) => {
                  choose(e.target.files?.[0]);
                  e.target.value = "";
                }}
              />
            </label>
          </Card>
        )}
        <PastImports projectId={projectId} />
      </PageBody>
    </>
  );
}

function MappingStep({
  projectId,
  file,
  preview,
  onCancel,
  onImported,
}: {
  projectId: string;
  file: File;
  preview: Preview;
  onCancel: () => void;
  onImported: (r: Import) => void;
}) {
  const { mutate } = useSWRConfig();
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const [mapping, setMapping] = useState<Record<string, string>>(preview.suggested_mapping);
  const [segmentId, setSegmentId] = useState("");
  const [importing, setImporting] = useState(false);

  const submit = async () => {
    setImporting(true);
    try {
      const form = new FormData();
      form.append("file", file);
      if (segmentId) form.append("segment_id", segmentId);
      form.append("mapping", JSON.stringify(Object.fromEntries(Object.entries(mapping).filter(([, header]) => header))));
      const result = await api<Import>(`/projects/${projectId}/import`, { body: form });
      mutate((k) => typeof k === "string" && (k.includes("/imports") || k.includes("/leads") || k.includes("/segments")));
      onImported(result);
    } catch (e) {
      toastError(e);
      setImporting(false);
    }
  };

  return (
    <Card className="p-6">
      <div className="flex items-center justify-between gap-3">
        <p className="truncate font-medium" title={file.name}>
          {file.name}
        </p>
        <Button variant="ghost" size="sm" onClick={onCancel}>
          Choose another file
        </Button>
      </div>
      <p className="mb-5 text-sm text-muted-foreground">{preview.headers.length} columns. Match them to lead fields.</p>

      <div className="grid gap-x-4 gap-y-3 sm:grid-cols-2 lg:grid-cols-3">
        {FIELDS.map((f) => (
          <Field key={f.key} label={f.key === "email" ? `${f.label} *` : f.label}>
            <Select value={mapping[f.key] ?? ""} onChange={(e) => setMapping({ ...mapping, [f.key]: e.target.value })}>
              <option value="">Don&apos;t import</option>
              {preview.headers.map((h) => (
                <option key={h} value={h}>
                  {h}
                </option>
              ))}
            </Select>
          </Field>
        ))}
      </div>

      <MicroLabel className="mt-6 mb-2">First rows</MicroLabel>
      <div className="overflow-x-auto rounded-xl border border-border">
        <table className="w-full text-sm">
          <thead>
            <tr>
              {preview.headers.map((h) => (
                <Th key={h} className="whitespace-nowrap">
                  {h}
                </Th>
              ))}
            </tr>
          </thead>
          <tbody>
            {preview.rows.map((row, i) => (
              <Tr key={i}>
                {row.map((cell, j) => (
                  <Td key={j} className="max-w-48 truncate whitespace-nowrap text-muted-foreground">
                    {cell}
                  </Td>
                ))}
              </Tr>
            ))}
          </tbody>
        </table>
      </div>

      <div className="mt-6 flex flex-wrap items-end justify-between gap-4">
        <Field label="Add to campaign" className="w-72" hint="Optional. Leads without a campaign stay in the project.">
          <Select value={segmentId} onChange={(e) => setSegmentId(e.target.value)}>
            <option value="">No campaign</option>
            {segments?.map((s) => (
              <option key={s.id} value={s.id}>
                {s.name}
              </option>
            ))}
          </Select>
        </Field>
        <Button onClick={submit} loading={importing} disabled={!mapping.email}>
          <Upload className="size-4" /> Import leads
        </Button>
      </div>
    </Card>
  );
}

function ImportResult({ result }: { result: Import }) {
  return (
    <Card className="p-5">
      <p className="flex items-start gap-2 font-medium">
        <CircleCheck className="mt-1 size-4 shrink-0 text-brand-600" />
        <span className="min-w-0 wrap-anywhere">
          Imported {num(result.imported)} of {num(result.total)} rows from <FileName name={result.filename} />
        </span>
      </p>
      {result.skipped > 0 && <p className="mt-1 text-sm text-muted-foreground">{num(result.skipped)} skipped.</p>}
      {result.errors.length > 0 && (
        <ul className="mt-3 max-h-48 overflow-y-auto rounded-xl bg-muted p-3 font-mono text-xs">
          {result.errors.map((e) => (
            <li key={`${e.row}:${e.error}`}>
              row {e.row}: {e.error}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}

function PastImports({ projectId }: { projectId: string }) {
  const { data, error, mutate } = useSWR<Import[]>(`/projects/${projectId}/imports`, fetcher);
  const { data: segments } = useSWR<SegmentWithStats[]>(`/projects/${projectId}/segments`, fetcher);
  const segmentName = (id: string | null) => segments?.find((s) => s.id === id)?.name ?? "—";

  return (
    <Card className="overflow-hidden">
      <MicroLabel className="px-5 pt-4 pb-2">Past imports</MicroLabel>
      {error ? (
        <LoadError error={error} retry={() => mutate()} />
      ) : !data ? (
        <ListSkeleton rows={3} />
      ) : data.length === 0 ? (
        <EmptyState title="No imports yet" className="py-8" />
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-sm">
            <thead>
              <tr>
                <Th>File</Th>
                <Th>Campaign</Th>
                <Th className="text-right">Rows</Th>
                <Th className="text-right">Imported</Th>
                <Th className="text-right">Skipped</Th>
                <Th className="text-right">Date</Th>
              </tr>
            </thead>
            <tbody>
              {data.map((i) => (
                <Tr key={i.id}>
                  <Td className="font-medium whitespace-nowrap">{i.filename}</Td>
                  <Td className="whitespace-nowrap text-muted-foreground">{segmentName(i.segment_id)}</Td>
                  <Td className="text-right tabular-nums">{num(i.total)}</Td>
                  <Td className="text-right tabular-nums">{num(i.imported)}</Td>
                  <Td className="text-right tabular-nums">{num(i.skipped)}</Td>
                  <Td className="text-right whitespace-nowrap text-muted-foreground">{dateTime(i.created_at)}</Td>
                </Tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
