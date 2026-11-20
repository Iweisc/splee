"use client";

import { useState } from "react";
import useSWR from "swr";
import { CircleCheck, CircleAlert, DatabaseBackup } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, Section, SectionsSkeleton, SettingsColumn } from "@/components/app/settings/layout";
import { useAction } from "@/components/app/settings/use-action";
import { Button } from "@/components/ui/button";
import { Field, Input, Switch } from "@/components/ui/form";
import { Badge, Card, EmptyState } from "@/components/ui/misc";
import { fetcher, useMe } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { ExportRun, ExportSettings, ExportStatus } from "@/lib/types";
import { dateTime, fileSize, num, plural, timeAgo } from "@/lib/utils";

const KEY = "/organization/exports";

type Form = Omit<ExportSettings, "updated_at"> & { secret_access_key: string };

const EMPTY: Form = {
  endpoint: "",
  region: "",
  bucket: "",
  prefix: "splee",
  path_style: false,
  access_key_id: "",
  secret_access_key: "",
  tables: [],
  include_bodies: false,
};

export default function DataExportPage() {
  const { me } = useMe();
  const admin = me?.role === "owner" || me?.role === "admin";
  const { data, error, mutate } = useSWR<ExportStatus>(admin ? KEY : null, fetcher, {
    // While an export runs, its row fills in when it's done.
    refreshInterval: (latest) => (latest?.runs.some((r) => r.status === "running") ? 2000 : 0),
  });

  return (
    <>
      <PageHeader title="Data export" description="Splee writes your data to your own S3-compatible bucket every day, ready for your warehouse." />
      <PageBody>
        <SettingsColumn>
          {me && !admin ? (
            <Card>
              <EmptyState icon={<DatabaseBackup className="size-6" />} title="Only admins can set up the data export">
                It sends every project&apos;s leads, emails and deals out of Splee, so an admin of your organization decides where they go.
              </EmptyState>
            </Card>
          ) : error ? (
            <LoadError error={error} what="the data export" retry={() => mutate()} />
          ) : !data ? (
            <SectionsSkeleton count={2} />
          ) : (
            <>
              <BucketSection status={data} onSaved={(s) => mutate(s, { revalidate: false })} />
              <RunsSection status={data} onStarted={() => mutate()} />
            </>
          )}
        </SettingsColumn>
      </PageBody>
    </>
  );
}

function BucketSection({ status, onSaved }: { status: ExportStatus; onSaved: (s: ExportStatus) => void }) {
  const saved = status.settings;
  const initial = (): Form => (saved ? { ...saved, secret_access_key: "" } : { ...EMPTY, tables: status.tables.map((t) => t.name) });
  const [form, setForm] = useState<Form>(initial);
  const [tested, setTested] = useState<{ ok: boolean; error: string | null } | null>(null);
  const { pending, run } = useAction();
  const set = (patch: Partial<Form>) => {
    setForm((f) => ({ ...f, ...patch }));
    setTested(null);
  };

  const test = () => run("test", async () => setTested(await api<{ ok: boolean; error: string | null }>(`${KEY}/test`, { body: form })));
  const save = () => run("save", async () => onSaved(await api<ExportStatus>(KEY, { method: "PUT", body: form })), "Saved");
  const off = () => {
    if (!confirm("Stop the daily export? The files already in your bucket stay there.")) return;
    run("off", async () => {
      onSaved(await api<ExportStatus>(KEY, { method: "DELETE" }));
      setForm({ ...EMPTY, tables: status.tables.map((t) => t.name) });
    }, "Data export turned off");
  };
  const toggle = (name: string) => set({ tables: form.tables.includes(name) ? form.tables.filter((t) => t !== name) : [...form.tables, name] });

  return (
    <Section
      title="Bucket"
      description="AWS S3, Cloudflare R2, Google Cloud Storage, Backblaze B2 or MinIO. The key needs permission to put and delete objects."
      actions={
        saved ? (
          <Button size="sm" variant="outline" onClick={off} loading={pending === "off"}>
            Turn off
          </Button>
        ) : undefined
      }
    >
      <div className="grid gap-4 sm:grid-cols-2">
        <Field label="Endpoint" hint="Leave empty for AWS S3." className="sm:col-span-2">
          <Input value={form.endpoint} onChange={(e) => set({ endpoint: e.target.value })} placeholder="https://<account id>.r2.cloudflarestorage.com" />
        </Field>
        <Field label="Region" hint="auto for R2 and Google Cloud Storage.">
          <Input value={form.region} onChange={(e) => set({ region: e.target.value })} placeholder="us-east-1" />
        </Field>
        <Field label="Bucket">
          <Input value={form.bucket} onChange={(e) => set({ bucket: e.target.value })} placeholder="acme-warehouse" />
        </Field>
        <Field label="Folder" hint="Files go under this path in the bucket.">
          <Input value={form.prefix} onChange={(e) => set({ prefix: e.target.value })} placeholder="splee" />
        </Field>
        <Field label="Access key ID">
          <Input value={form.access_key_id} onChange={(e) => set({ access_key_id: e.target.value })} autoComplete="off" spellCheck={false} />
        </Field>
        <Field label="Secret access key" hint={saved ? "Saved. Type a new one only to replace it." : "Stored encrypted. Splee never shows it again."} className="sm:col-span-2">
          <Input
            type="password"
            value={form.secret_access_key}
            onChange={(e) => set({ secret_access_key: e.target.value })}
            placeholder={saved ? "••••••••••••••••" : ""}
            autoComplete="new-password"
          />
        </Field>
      </div>

      <div className="mt-5 flex items-start justify-between gap-4 text-sm">
        <span>
          <span className="block font-medium">Bucket name in the path</span>
          <span className="block text-pretty text-muted-foreground">For MinIO and other servers without a subdomain per bucket.</span>
        </span>
        <Switch checked={form.path_style} onChange={(v) => set({ path_style: v })} label="Bucket name in the path" />
      </div>

      <Field label="Tables" group className="mt-5">
        <div className="grid grid-cols-2 gap-x-4 gap-y-2 sm:grid-cols-3">
          {status.tables.map((t) => (
            <label key={t.name} className="flex items-center gap-2 text-sm">
              <input type="checkbox" checked={form.tables.includes(t.name)} onChange={() => toggle(t.name)} className="accent-black" />
              {t.label}
            </label>
          ))}
        </div>
      </Field>

      <div className="mt-5 flex items-start justify-between gap-4 text-sm">
        <span>
          <span className="block font-medium">Include email text</span>
          <span className="block text-pretty text-muted-foreground">Adds the text of each sent email and reply. Without it, only subjects and counts.</span>
        </span>
        <Switch checked={form.include_bodies} onChange={(v) => set({ include_bodies: v })} label="Include email text" />
      </div>

      {tested && (
        <p className={`mt-5 flex items-start gap-2 rounded-lg px-3 py-2.5 text-sm text-pretty ${tested.ok ? "bg-brand-50 text-brand-800" : "bg-red-50 text-red-800"}`}>
          {tested.ok ? <CircleCheck className="mt-0.5 size-4 shrink-0" /> : <CircleAlert className="mt-0.5 size-4 shrink-0" />}
          {tested.ok ? `Connected. Splee wrote a test file to ${form.bucket.trim()} and deleted it.` : tested.error}
        </p>
      )}

      <div className="mt-5 flex flex-wrap gap-2">
        <Button onClick={save} loading={pending === "save"}>
          Save
        </Button>
        <Button variant="outline" onClick={test} loading={pending === "test"}>
          Test connection
        </Button>
      </div>
    </Section>
  );
}

function RunsSection({ status, onStarted }: { status: ExportStatus; onStarted: () => void }) {
  const { pending, run } = useAction();
  const running = status.runs.some((r) => r.status === "running");
  const exportNow = () => run("run", async () => {
    await api(`${KEY}/run`, { method: "POST" });
    onStarted();
  }, "Export started");
  const last = status.runs.find((r) => r.status !== "running");

  return (
    <Section
      title="Exports"
      description={`Every day after ${String(status.quiet_hour_utc).padStart(2, "0")}:00 UTC. Each table is a gzipped JSON lines file under dt=YYYY-MM-DD, and the day's manifest is written last.`}
      actions={
        <Button size="sm" variant="outline" onClick={exportNow} loading={pending === "run" || running} disabled={!status.settings}>
          Export now
        </Button>
      }
    >
      {!status.runs.length ? (
        <p className="text-sm text-muted-foreground">{status.settings ? "The first export runs tonight, or now if you start it." : "Save a bucket to start exporting."}</p>
      ) : (
        <>
          {last && <LastRun run={last} />}
          <ul className="mt-4 divide-y divide-border rounded-lg border border-border">
            {status.runs.map((r) => (
              <RunRow key={r.id} run={r} />
            ))}
          </ul>
        </>
      )}
    </Section>
  );
}

function LastRun({ run: r }: { run: ExportRun }) {
  if (r.status === "failed")
    return (
      <p className="flex items-start gap-2 rounded-lg bg-red-50 px-3 py-2.5 text-sm text-pretty text-red-800">
        <CircleAlert className="mt-0.5 size-4 shrink-0" />
        <span>
          The last export failed {timeAgo(r.started_at)}. {r.error}
        </span>
      </p>
    );
  return (
    <p className="text-sm text-pretty">
      Last export {timeAgo(r.finished_at ?? r.started_at)}: {plural(r.files.length, "file")}, {plural(r.rows, "row")}, {fileSize(r.bytes)}.
    </p>
  );
}

function RunRow({ run: r }: { run: ExportRun }) {
  const tone = r.status === "done" ? "green" : r.status === "failed" ? "red" : "blue";
  const label = r.status === "done" ? "Done" : r.status === "failed" ? "Failed" : "Running";
  return (
    <li className="px-3 py-2.5 text-sm">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="font-medium tabular-nums">{r.dt}</span>
        <Badge tone={tone}>{label}</Badge>
        <span className="text-muted-foreground">{r.trigger === "daily" ? "Daily" : "Export now"}</span>
        {/* Its own line on phones, at the right on wider screens. */}
        <span className="w-full text-xs text-muted-foreground tabular-nums sm:ml-auto sm:w-auto">
          {r.status === "done" ? `${num(r.rows)} rows · ${fileSize(r.bytes)} · ` : ""}
          {dateTime(r.started_at)}
        </span>
      </div>
      {r.error && <p className="mt-1 text-xs text-pretty text-red-700">{r.error}</p>}
    </li>
  );
}
