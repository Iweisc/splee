"use client";

import { use, useState } from "react";
import { CircleCheck, Database, FileUp, Search, Sheet, Table2, Upload } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { OutreachForm, OutreachImports } from "@/components/app/import/outreach-import";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { ListSkeleton, LoadError, MicroLabel, Td, Th, toastError, Tr } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Card, EmptyState, FileName, Spinner, Tabs } from "@/components/ui/misc";
import { api } from "@/lib/api";
import { type Finders, type Import, LEAD_IMPORT_FIELDS, type ProjectSettings, type SegmentWithStats } from "@/lib/types";
import { cn, dateTime, num, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

type Preview = { filename: string; headers: string[]; rows: string[][]; suggested_mapping: Record<string, string> };

/**
 * The services read through a link and a token, sent as `{kind}_url` and `{kind}_token`. Adding one here adds its tab and
 * form, and the backend reads the same fields in routes/imports.rs.
 */
const TABLE_SOURCES = {
  airtable: {
    label: "Airtable",
    link: { label: "Table link", hint: "The table or view open in your browser.", placeholder: "https://airtable.com/app…/tbl…" },
    token: { label: "Personal access token", hint: "With data.records:read, from Builder Hub. Never saved.", placeholder: "pat…" },
    icon: Table2,
    noun: "table",
  },
  notion: {
    label: "Notion",
    link: { label: "Database link", hint: "Every row, whatever view is open.", placeholder: "https://www.notion.so/…" },
    token: { label: "Integration secret", hint: "Added to the database under ••• → Connections. Never saved.", placeholder: "ntn_…" },
    icon: Database,
    noun: "database",
  },
  baserow: {
    label: "Baserow",
    link: { label: "Table link", hint: "On baserow.io or your own server. A view's filters apply.", placeholder: "https://baserow.io/database/…/table/…" },
    token: { label: "Database token", hint: "From Settings → Database tokens, with read access. Never saved.", placeholder: "" },
    icon: Table2,
    noun: "table",
  },
  nocodb: {
    label: "NocoDB",
    link: { label: "Table link", hint: "On app.nocodb.com or your own server. A view's filters apply.", placeholder: "https://app.nocodb.com/#/…" },
    token: { label: "API token", hint: "From Account settings → Tokens. Never saved.", placeholder: "" },
    icon: Table2,
    noun: "table",
  },
  grist: {
    label: "Grist",
    link: { label: "Page link", hint: "The page with the table, on getgrist.com or your own server.", placeholder: "https://docs.getgrist.com/…/p/…" },
    token: { label: "API key", hint: "From Profile settings → API. Never saved.", placeholder: "" },
    icon: Table2,
    noun: "table",
  },
  teable: {
    label: "Teable",
    link: { label: "Table link", hint: "On app.teable.ai or your own server. A view's filters apply.", placeholder: "https://app.teable.ai/base/…/table/…" },
    token: { label: "Personal access token", hint: "With record and field read access to the base. Never saved.", placeholder: "teable_…" },
    icon: Table2,
    noun: "table",
  },
} as const;

type TableKind = keyof typeof TABLE_SOURCES;
type LinkAndToken = { url: string; token: string };

const NO_LINKS = Object.fromEntries(Object.keys(TABLE_SOURCES).map((k) => [k, { url: "", token: "" }])) as Record<TableKind, LinkAndToken>;

/**
 * Where the rows come from: an uploaded file, a Google Sheet's link, or a table's link and token. Sent again on import,
 * since the server keeps none of them.
 */
type Source = { file: File } | { sheet_url: string } | ({ kind: TableKind } & LinkAndToken);

const appendSource = (form: FormData, source: Source) => {
  if ("file" in source) form.append("file", source.file);
  else if ("sheet_url" in source) form.append("sheet_url", source.sheet_url);
  else {
    form.append(`${source.kind}_url`, source.url);
    form.append(`${source.kind}_token`, source.token);
  }
};

const sourceKind = (source: Source) => ("file" in source ? "file" : "sheet_url" in source ? "sheet" : source.kind);

type LinkSource = "sheet" | TableKind | "outreach";

const LINK_SOURCES: { value: LinkSource; label: string }[] = [
  { value: "sheet", label: "Google Sheet" },
  ...Object.entries(TABLE_SOURCES).map(([kind, { label }]) => ({ value: kind as TableKind, label })),
  { value: "outreach", label: "Another outreach tool" },
];

const chooseAnother = (source: Source) =>
  `Choose another ${"file" in source ? "file" : "sheet_url" in source ? "sheet" : TABLE_SOURCES[source.kind].noun}`;

export default function ImportPage({ params }: PageProps<"/app/p/[projectId]/import">) {
  const { projectId } = use(params);
  const [source, setSource] = useState<Source | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [result, setResult] = useState<Import | null>(null);
  const [loading, setLoading] = useState<ReturnType<typeof sourceKind> | null>(null);
  const [sheetUrl, setSheetUrl] = useState("");
  // Kept per service, so switching tabs doesn't lose what was typed.
  const [links, setLinks] = useState(NO_LINKS);
  const [linkSource, setLinkSource] = useState<LinkSource>("sheet");
  const [startedId, setStartedId] = useState<string | null>(null);

  const choose = async (next: Source) => {
    setResult(null);
    setLoading(sourceKind(next));
    try {
      const form = new FormData();
      appendSource(form, next);
      const p = await api<Preview>(`/projects/${projectId}/import/preview`, { body: form });
      setSource(next);
      setPreview(p);
      setSheetUrl("");
      setLinks(NO_LINKS);
    } catch (e) {
      toastError(e);
    } finally {
      setLoading(null);
    }
  };

  const reset = () => {
    setSource(null);
    setPreview(null);
  };

  return (
    <>
      <PageHeader
        title="Import leads"
        description="From a CSV or Excel file, a Google Sheet, Airtable, Notion, Baserow, NocoDB, Grist, Teable, Hunter, or your campaigns in Instantly, Smartlead or lemlist. Suppressed, duplicate and invalid emails are skipped."
      />
      <PageBody className="flex flex-col gap-4">
        {result && <ImportResult result={result} />}
        <OutreachImports projectId={projectId} startedId={startedId} />
        {source && preview ? (
          <MappingStep
            projectId={projectId}
            source={source}
            preview={preview}
            onCancel={reset}
            onImported={(r) => {
              setResult(r);
              reset();
            }}
          />
        ) : (
          <Card>
            <label className="flex cursor-pointer flex-col items-center gap-2 rounded-t-2xl border-b border-dashed border-border-strong px-6 py-12 text-center hover:bg-accent">
              {loading === "file" ? <Spinner className="size-5" /> : <FileUp className="size-5 text-muted-foreground" />}
              <span className="font-medium">{loading === "file" ? "Reading file…" : "Choose a CSV or Excel file"}</span>
              <span className="text-sm text-muted-foreground">You map the columns in the next step.</span>
              <input
                type="file"
                accept=".csv,text/csv,.xlsx,.xlsm,.xls,.ods"
                className="sr-only"
                disabled={!!loading}
                onChange={(e) => {
                  const f = e.target.files?.[0];
                  if (f) choose({ file: f });
                  e.target.value = "";
                }}
              />
            </label>
            <div className="flex flex-col gap-4 p-6">
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2">
                <span className="text-sm font-medium">Or import from</span>
                {/* All in a row is wider than a phone, and a scrolling row hides the last ones, so phones get them two by two.
                    "Another outreach tool" is too long for half a phone, so it takes the last row, and with an even number of the
                    others Google Sheet takes the first, so none is left alone with a gap beside it. */}
                <Tabs
                  value={linkSource}
                  onChange={setLinkSource}
                  options={LINK_SOURCES}
                  className={cn("grid w-full grid-cols-2 sm:inline-flex sm:w-auto", "*:last:col-span-2", LINK_SOURCES.length % 2 === 0 && "*:first:col-span-2")}
                />
              </div>
              {linkSource === "sheet" && (
                <form
                  className="flex flex-wrap items-start gap-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (sheetUrl.trim()) choose({ sheet_url: sheetUrl.trim() });
                  }}
                >
                  <Field label="Sheet link" hint="Shared with Anyone with the link. The tab that was open is the one read." className="min-w-56 flex-1">
                    <Input value={sheetUrl} onChange={(e) => setSheetUrl(e.target.value)} placeholder="https://docs.google.com/spreadsheets/d/…" />
                  </Field>
                  {/* Level with the field. On phones it sits under it. */}
                  <Button type="submit" variant="outline" className="sm:mt-6" loading={loading === "sheet"} disabled={!sheetUrl.trim() || !!loading}>
                    <Sheet className="size-4" /> Read sheet
                  </Button>
                </form>
              )}
              {linkSource === "outreach" && <OutreachForm projectId={projectId} onStarted={setStartedId} />}
              {linkSource !== "sheet" && linkSource !== "outreach" && (
                <TableForm
                  kind={linkSource}
                  value={links[linkSource]}
                  onChange={(value) => setLinks({ ...links, [linkSource]: value })}
                  loading={loading === linkSource}
                  disabled={!!loading}
                  onRead={(value) => choose({ kind: linkSource, ...value })}
                />
              )}
            </div>
          </Card>
        )}
        {!preview && <HunterImport projectId={projectId} onImported={setResult} />}
        <PastImports projectId={projectId} />
      </PageBody>
    </>
  );
}

/** A service's link and token, read with its button. */
function TableForm({
  kind,
  value,
  onChange,
  loading,
  disabled,
  onRead,
}: {
  kind: TableKind;
  value: LinkAndToken;
  onChange: (value: LinkAndToken) => void;
  loading: boolean;
  disabled: boolean;
  onRead: (value: LinkAndToken) => void;
}) {
  const { link, token, icon: Icon, noun } = TABLE_SOURCES[kind];
  const ready = { url: value.url.trim(), token: value.token.trim() };
  return (
    <form
      className="flex flex-wrap items-start gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (ready.url && ready.token) onRead(ready);
      }}
    >
      <Field label={link.label} hint={link.hint} className="min-w-56 flex-1">
        <Input value={value.url} onChange={(e) => onChange({ ...value, url: e.target.value })} placeholder={link.placeholder} />
      </Field>
      <Field label={token.label} hint={token.hint} className="min-w-56 flex-1">
        <Input type="password" value={value.token} onChange={(e) => onChange({ ...value, token: e.target.value })} placeholder={token.placeholder || undefined} />
      </Field>
      {/* Level with the fields. On phones it sits under them. */}
      <Button type="submit" variant="outline" className="sm:mt-6" loading={loading} disabled={!ready.url || !ready.token || disabled}>
        <Icon className="size-4" /> Read {noun}
      </Button>
    </form>
  );
}

function MappingStep({
  projectId,
  source,
  preview,
  onCancel,
  onImported,
}: {
  projectId: string;
  source: Source;
  preview: Preview;
  onCancel: () => void;
  onImported: (r: Import) => void;
}) {
  const { mutate } = useSWRConfig();
  const [mapping, setMapping] = useState<Record<string, string>>(preview.suggested_mapping);
  const [segmentId, setSegmentId] = useState("");
  const [importing, setImporting] = useState(false);

  const submit = async () => {
    setImporting(true);
    try {
      const form = new FormData();
      appendSource(form, source);
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
        {/* Sheet titles run long, so the name wraps instead of hiding behind the button on phones. */}
        <p className="min-w-0 font-medium wrap-anywhere">
          {preview.filename}
        </p>
        <Button variant="ghost" size="sm" className="shrink-0" onClick={onCancel}>
          {/* Short on phones, so a long sheet name keeps the width. */}
          <span className="sm:hidden">Change</span>
          <span className="max-sm:hidden">{chooseAnother(source)}</span>
        </Button>
      </div>
      <p className="mb-5 text-sm text-muted-foreground">{plural(preview.headers.length, "column")}. Match them to lead fields.</p>

      <div className="grid gap-x-4 gap-y-3 sm:grid-cols-2 lg:grid-cols-3">
        {LEAD_IMPORT_FIELDS.map((f) => (
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
        <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} />
        <Button onClick={submit} loading={importing} disabled={!mapping.email}>
          <Upload className="size-4" /> Import leads
        </Button>
      </div>
    </Card>
  );
}

/** Imports the people Hunter knows at a company, with the Hunter key from Email verification. */
function HunterImport({ projectId, onImported }: { projectId: string; onImported: (r: Import) => void }) {
  const { mutate } = useSWRConfig();
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${projectId}/settings`, fetcher);
  const { data: finders } = useSWR<Finders>(`/projects/${projectId}/finders`, fetcher);
  const [domain, setDomain] = useState("");
  const [segmentId, setSegmentId] = useState("");
  const [finding, setFinding] = useState(false);
  if (!settings) return null;

  const find = async () => {
    setFinding(true);
    try {
      const result = await api<Import>(`/projects/${projectId}/import/hunter`, { body: { domain, segment_id: segmentId || null } });
      mutate((k) => typeof k === "string" && (k.includes("/imports") || k.includes("/leads") || k.includes("/segments")));
      onImported(result);
      setDomain("");
    } catch (e) {
      toastError(e);
    } finally {
      setFinding(false);
    }
  };

  return (
    <Card className="p-6">
      <h2 className="font-semibold">Find people with Hunter</h2>
      <p className="mt-0.5 text-sm text-muted-foreground">Imports the people Hunter knows at a company, with their titles and LinkedIn.</p>
      {settings.verifier === "hunter" || finders?.finders.some((f) => f.provider === "hunter") ? (
        <form
          className="mt-5 flex flex-wrap items-start gap-4"
          onSubmit={(e) => {
            e.preventDefault();
            if (domain.trim()) find();
          }}
        >
          <Field label="Company website" className="min-w-40 flex-1">
            <Input value={domain} onChange={(e) => setDomain(e.target.value)} placeholder="acme.com" />
          </Field>
          <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} className="min-w-40 flex-1" />
          {/* Level with the fields beside them. On phones the fields stack and it sits under them. */}
          <Button type="submit" className="sm:mt-6" loading={finding} disabled={!domain.trim()}>
            <Search className="size-4" /> Find people
          </Button>
        </form>
      ) : (
        <div className="mt-4 flex flex-wrap items-center justify-between gap-3">
          <p className="text-sm text-muted-foreground">Connect Hunter under Email finder to use it here.</p>
          <LinkButton href={`/app/p/${projectId}/integrations`} variant="outline" size="sm">
            Open Integrations
          </LinkButton>
        </div>
      )}
    </Card>
  );
}

function ImportResult({ result }: { result: Import }) {
  return (
    <Card className="p-5">
      <p className="flex items-start gap-2 font-medium">
        <CircleCheck className="mt-1 size-4 shrink-0 text-brand-600" />
        <span className="min-w-0 wrap-anywhere">
          Imported {num(result.imported)} of {plural(result.total, "row")} from <FileName name={result.filename} />
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
