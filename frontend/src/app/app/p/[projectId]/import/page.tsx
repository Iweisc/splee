"use client";

import { use, useState } from "react";
import { CircleCheck, Database, FileUp, Search, Sheet, Table2, Upload } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CampaignField } from "@/components/app/settings/campaign-field";
import { ListSkeleton, LoadError, MicroLabel, Td, Th, toastError, Tr } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Card, EmptyState, FileName, Spinner, Tabs } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { Import, ProjectSettings, SegmentWithStats } from "@/lib/types";
import { dateTime, num, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";

type Preview = { filename: string; headers: string[]; rows: string[][]; suggested_mapping: Record<string, string> };

/**
 * Where the rows come from: an uploaded file, a Google Sheet's link, or an Airtable table's, Notion database's, Baserow
 * table's or NocoDB table's link and token. Sent again on import, since the server keeps none of them.
 */
type Source =
  | { file: File }
  | { sheet_url: string }
  | { airtable_url: string; airtable_token: string }
  | { notion_url: string; notion_token: string }
  | { baserow_url: string; baserow_token: string }
  | { nocodb_url: string; nocodb_token: string };

const appendSource = (form: FormData, source: Source) => {
  for (const [key, value] of Object.entries(source)) form.append(key, value);
};

const sourceKind = (source: Source) =>
  "file" in source
    ? "file"
    : "sheet_url" in source
      ? "sheet"
      : "airtable_url" in source
        ? "airtable"
        : "notion_url" in source
          ? "notion"
          : "baserow_url" in source
            ? "baserow"
            : "nocodb";

type LinkSource = Exclude<ReturnType<typeof sourceKind>, "file">;

const LINK_SOURCES: { value: LinkSource; label: string }[] = [
  { value: "sheet", label: "Google Sheet" },
  { value: "airtable", label: "Airtable" },
  { value: "notion", label: "Notion" },
  { value: "baserow", label: "Baserow" },
  { value: "nocodb", label: "NocoDB" },
];

const CHOOSE_ANOTHER: Record<ReturnType<typeof sourceKind>, string> = {
  file: "Choose another file",
  sheet: "Choose another sheet",
  airtable: "Choose another table",
  notion: "Choose another database",
  baserow: "Choose another table",
  nocodb: "Choose another table",
};

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
  const [source, setSource] = useState<Source | null>(null);
  const [preview, setPreview] = useState<Preview | null>(null);
  const [result, setResult] = useState<Import | null>(null);
  const [loading, setLoading] = useState<ReturnType<typeof sourceKind> | null>(null);
  const [sheetUrl, setSheetUrl] = useState("");
  const [airtable, setAirtable] = useState({ url: "", token: "" });
  const [notion, setNotion] = useState({ url: "", token: "" });
  const [baserow, setBaserow] = useState({ url: "", token: "" });
  const [nocodb, setNocodb] = useState({ url: "", token: "" });
  const [linkSource, setLinkSource] = useState<LinkSource>("sheet");

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
      setAirtable({ url: "", token: "" });
      setNotion({ url: "", token: "" });
      setBaserow({ url: "", token: "" });
      setNocodb({ url: "", token: "" });
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
        description="From a CSV or Excel file, a Google Sheet, Airtable, Notion, Baserow, NocoDB or Hunter. Suppressed, duplicate and invalid emails are skipped."
      />
      <PageBody className="flex flex-col gap-4">
        {result && <ImportResult result={result} />}
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
                {/* All in a row is wider than a phone, and a scrolling row hides the last ones, so phones get Google Sheet on a row
                    of its own and the rest two by two. */}
                <Tabs
                  value={linkSource}
                  onChange={setLinkSource}
                  options={LINK_SOURCES}
                  className="grid w-full grid-cols-2 *:first:col-span-2 sm:inline-flex sm:w-auto"
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
              {linkSource === "airtable" && (
                <form
                  className="flex flex-wrap items-start gap-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (airtable.url.trim() && airtable.token.trim()) choose({ airtable_url: airtable.url.trim(), airtable_token: airtable.token.trim() });
                  }}
                >
                  <Field label="Table link" hint="The table or view open in your browser." className="min-w-56 flex-1">
                    <Input value={airtable.url} onChange={(e) => setAirtable({ ...airtable, url: e.target.value })} placeholder="https://airtable.com/app…/tbl…" />
                  </Field>
                  <Field label="Personal access token" hint="With data.records:read, from Builder Hub. Never saved." className="min-w-56 flex-1">
                    <Input type="password" value={airtable.token} onChange={(e) => setAirtable({ ...airtable, token: e.target.value })} placeholder="pat…" />
                  </Field>
                  <Button
                    type="submit"
                    variant="outline"
                    className="sm:mt-6"
                    loading={loading === "airtable"}
                    disabled={!airtable.url.trim() || !airtable.token.trim() || !!loading}
                  >
                    <Table2 className="size-4" /> Read table
                  </Button>
                </form>
              )}
              {linkSource === "notion" && (
                <form
                  className="flex flex-wrap items-start gap-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (notion.url.trim() && notion.token.trim()) choose({ notion_url: notion.url.trim(), notion_token: notion.token.trim() });
                  }}
                >
                  <Field label="Database link" hint="Every row, whatever view is open." className="min-w-56 flex-1">
                    <Input value={notion.url} onChange={(e) => setNotion({ ...notion, url: e.target.value })} placeholder="https://www.notion.so/…" />
                  </Field>
                  <Field label="Integration secret" hint="Added to the database under ••• → Connections. Never saved." className="min-w-56 flex-1">
                    <Input type="password" value={notion.token} onChange={(e) => setNotion({ ...notion, token: e.target.value })} placeholder="ntn_…" />
                  </Field>
                  <Button
                    type="submit"
                    variant="outline"
                    className="sm:mt-6"
                    loading={loading === "notion"}
                    disabled={!notion.url.trim() || !notion.token.trim() || !!loading}
                  >
                    <Database className="size-4" /> Read database
                  </Button>
                </form>
              )}
              {linkSource === "baserow" && (
                <form
                  className="flex flex-wrap items-start gap-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (baserow.url.trim() && baserow.token.trim()) choose({ baserow_url: baserow.url.trim(), baserow_token: baserow.token.trim() });
                  }}
                >
                  <Field label="Table link" hint="On baserow.io or your own server. A view's filters apply." className="min-w-56 flex-1">
                    <Input value={baserow.url} onChange={(e) => setBaserow({ ...baserow, url: e.target.value })} placeholder="https://baserow.io/database/…/table/…" />
                  </Field>
                  <Field label="Database token" hint="From Settings → Database tokens, with read access. Never saved." className="min-w-56 flex-1">
                    <Input type="password" value={baserow.token} onChange={(e) => setBaserow({ ...baserow, token: e.target.value })} />
                  </Field>
                  <Button
                    type="submit"
                    variant="outline"
                    className="sm:mt-6"
                    loading={loading === "baserow"}
                    disabled={!baserow.url.trim() || !baserow.token.trim() || !!loading}
                  >
                    <Table2 className="size-4" /> Read table
                  </Button>
                </form>
              )}
              {linkSource === "nocodb" && (
                <form
                  className="flex flex-wrap items-start gap-4"
                  onSubmit={(e) => {
                    e.preventDefault();
                    if (nocodb.url.trim() && nocodb.token.trim()) choose({ nocodb_url: nocodb.url.trim(), nocodb_token: nocodb.token.trim() });
                  }}
                >
                  <Field label="Table link" hint="On app.nocodb.com or your own server. A view's filters apply." className="min-w-56 flex-1">
                    <Input value={nocodb.url} onChange={(e) => setNocodb({ ...nocodb, url: e.target.value })} placeholder="https://app.nocodb.com/#/…" />
                  </Field>
                  <Field label="API token" hint="From Account settings → Tokens. Never saved." className="min-w-56 flex-1">
                    <Input type="password" value={nocodb.token} onChange={(e) => setNocodb({ ...nocodb, token: e.target.value })} />
                  </Field>
                  <Button
                    type="submit"
                    variant="outline"
                    className="sm:mt-6"
                    loading={loading === "nocodb"}
                    disabled={!nocodb.url.trim() || !nocodb.token.trim() || !!loading}
                  >
                    <Table2 className="size-4" /> Read table
                  </Button>
                </form>
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
          <span className="max-sm:hidden">{CHOOSE_ANOTHER[sourceKind(source)]}</span>
        </Button>
      </div>
      <p className="mb-5 text-sm text-muted-foreground">{plural(preview.headers.length, "column")}. Match them to lead fields.</p>

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
      {settings.verifier === "hunter" || settings.finder === "hunter" ? (
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
