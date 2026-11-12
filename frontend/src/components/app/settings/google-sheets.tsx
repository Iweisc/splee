"use client";

import { useEffect, useState } from "react";
import Link from "next/link";
import { FileSpreadsheet, Trash2 } from "lucide-react";
import { toast } from "sonner";
import useSWR, { useSWRConfig } from "swr";
import { LoadError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Field, Input, Select, Switch } from "@/components/ui/form";
import { Badge, Message, Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { type Import, LEAD_IMPORT_FIELDS, SHEET_EVENTS, type SheetConnection, type SheetEvent, type SheetsStatus } from "@/lib/types";
import { num, plural, timeAgo } from "@/lib/utils";
import { CampaignField } from "./campaign-field";
import { Section } from "./layout";
import { useAction } from "./use-action";

type Opened = { spreadsheet_id: string; name: string; tabs: { id: number; title: string }[]; tab: string | null };
type Preview = { headers: string[]; rows: string[][]; suggested_mapping: Record<string, string> };
type Created = SheetsStatus & { import: Import | null; error: string | null };
type Result = { name: string; import: Import | null; error: string | null };

const ALL_EVENTS = Object.keys(SHEET_EVENTS) as SheetEvent[];

/** Sends the browser to Google. It comes back to this page with `sheets=connected` or `sheets_error`. */
async function signIn(projectId: string) {
  const { url } = await api<{ url: string }>(`/projects/${projectId}/sheets/oauth`, { method: "POST" });
  window.location.assign(url);
}

/** The Google account the project signed in with for Sheets, its import and export tabs, and the form to add one. */
export function GoogleSheetsSection({ projectId }: { projectId: string }) {
  const key = `/projects/${projectId}/sheets`;
  const { data, error, mutate } = useSWR<SheetsStatus>(key, fetcher);
  const { pending, run } = useAction();

  useEffect(() => {
    const params = new URLSearchParams(window.location.search);
    const done = params.get("sheets");
    const failed = params.get("sheets_error");
    if (!done && !failed) return;
    // Cleared first, so a reload doesn't show it again.
    window.history.replaceState(null, "", window.location.pathname);
    if (failed) toast.error(failed);
    if (done) {
      toast.success("Google Sheets connected");
      mutate();
      document.getElementById("google-sheets")?.scrollIntoView();
    }
  }, [mutate]);

  if (error) return <LoadError error={error} what="Google Sheets" retry={() => mutate()} />;
  if (!data || (!data.available && !data.account)) return null;
  const account = data.account;
  const show = (fresh: SheetsStatus) => mutate(fresh, { revalidate: false });
  const disconnect = () => {
    if (confirm("Disconnect Google Sheets? Your tabs stay set up, but nothing imports or exports until you connect again."))
      run("off", async () => show(await api<SheetsStatus>(key, { method: "DELETE" })), "Google Sheets disconnected");
  };
  return (
    <div id="google-sheets" className="scroll-mt-20">
      <Section
        title={
          <span className="flex items-center gap-2">
            <FileSpreadsheet className="size-4" /> Google Sheets
          </span>
        }
        description="Import leads from a sheet, and add a row to one for every hot lead, booked meeting and won deal."
        actions={
          account ? (
            <Button size="sm" variant="outline" onClick={disconnect} loading={pending === "off"}>
              Disconnect
            </Button>
          ) : undefined
        }
      >
        {!account ? (
          <div className="flex flex-col items-start gap-3">
            <p className="text-sm text-pretty text-muted-foreground">
              Google asks to let Splee see and edit your spreadsheets, and nothing else. It&apos;s a separate sign-in from any mailbox you connected.
            </p>
            <Button onClick={() => run("connect", () => signIn(projectId))} loading={pending === "connect"}>
              Connect Google Sheets
            </Button>
          </div>
        ) : (
          <div className="flex flex-col gap-5">
            {account.needs_reconnect ? (
              <div className="flex flex-col items-start gap-3 rounded-lg bg-amber-50 px-3 py-3 text-sm text-amber-800">
                <p className="text-pretty">{account.error ?? "Google signed Splee out of Sheets."}</p>
                <Button size="sm" onClick={() => run("connect", () => signIn(projectId))} loading={pending === "connect"}>
                  Reconnect {account.email}
                </Button>
              </div>
            ) : (
              <p className="text-sm text-pretty">
                Signed in as <span className="font-medium wrap-anywhere">{account.email}</span>. Share a sheet with this account to use it here.
              </p>
            )}
            {account.held_until && (
              <p className="text-xs text-pretty text-amber-700">Google asked Splee to slow down. It tries again {timeAgo(account.held_until)}.</p>
            )}
            {!!data.connections.length && (
              <ul className="divide-y divide-border rounded-lg border border-border">
                {data.connections.map((c) => (
                  <ConnectionRow key={c.id} projectId={projectId} connection={c} onChange={show} />
                ))}
              </ul>
            )}
            {!account.needs_reconnect && <SetUp projectId={projectId} onDone={show} />}
          </div>
        )}
      </Section>
    </div>
  );
}

function ConnectionRow({ projectId, connection: c, onChange }: { projectId: string; connection: SheetConnection; onChange: (s: SheetsStatus) => void }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const path = `/projects/${projectId}/sheets/connections/${c.id}`;
  const isImport = c.kind === "import";
  const patch = (body: Record<string, unknown>, done: string) => run("patch", async () => onChange(await api<SheetsStatus>(path, { method: "PATCH", body })), done);
  const remove = () => {
    if (confirm(`Stop ${isImport ? "importing from" : "sending lead events to"} ${c.tab}? Nothing in the sheet changes.`))
      run("remove", async () => onChange(await api<SheetsStatus>(path, { method: "DELETE" })), "Removed");
  };
  const importNow = () =>
    run("import", async () => {
      const done = await api<{ import: Import | null }>(`${path}/import`, { method: "POST" });
      await mutate(`/projects/${projectId}/sheets`);
      mutate((k) => typeof k === "string" && (k.includes("/imports") || k.includes("/leads") || k.includes("/segments")));
      toast.success(done.import ? `Added ${plural(done.import.imported, "lead")}` : "No new rows since the last read");
    });
  const done = isImport ? `${plural(c.rows_added, "lead")} added` : `${plural(c.rows_added, "row")} written`;
  const when = c.last_sync_at ? `last ${isImport ? "read" : "write"} ${timeAgo(c.last_sync_at)}` : isImport ? "not read yet" : "nothing to write yet";
  return (
    <li className="px-3 py-3">
      <div className="flex items-start gap-2">
        <div className="min-w-0 flex-1">
          <div className="flex flex-wrap items-center gap-x-2 gap-y-1">
            <Badge tone={isImport ? "blue" : "green"}>{isImport ? "Import" : "Export"}</Badge>
            <a
              href={`https://docs.google.com/spreadsheets/d/${c.spreadsheet_id}/edit`}
              target="_blank"
              rel="noreferrer"
              className="min-w-0 text-sm font-medium wrap-anywhere underline-offset-4 hover:underline"
            >
              {c.spreadsheet_name} › {c.tab}
            </a>
          </div>
          <p className="mt-1 text-xs text-muted-foreground">
            {done}, {when}.
          </p>
        </div>
        <Button size="icon" variant="ghost" onClick={remove} loading={pending === "remove"} aria-label={`Remove ${c.tab}`} title="Remove" className="-mt-1.5 -mr-1.5 shrink-0">
          <Trash2 className="size-4" />
        </Button>
      </div>
      {isImport ? (
        <div className="mt-3 flex flex-wrap items-center justify-between gap-3">
          <div className="flex min-w-0 items-center gap-3">
            <Switch
              checked={c.keep_importing}
              onChange={(on) => patch({ keep_importing: on }, on ? "New rows import every 15 minutes" : "Stopped importing new rows")}
              disabled={pending === "patch"}
              label="Keep importing"
            />
            <span className="text-sm text-pretty">Keep importing new rows every 15 minutes</span>
          </div>
          <Button size="sm" variant="outline" onClick={importNow} loading={pending === "import"}>
            Import now
          </Button>
        </div>
      ) : (
        <EventChecks value={c.events} onChange={(events) => patch({ events }, "Saved")} disabled={pending === "patch"} />
      )}
      {c.error && (
        <p className="mt-3 rounded-lg bg-red-50 px-3 py-2 text-xs text-pretty text-red-700">
          <Message text={c.error} />
        </p>
      )}
    </li>
  );
}

function EventChecks({ value, onChange, disabled }: { value: SheetEvent[]; onChange: (events: SheetEvent[]) => void; disabled?: boolean }) {
  return (
    <fieldset className="mt-3">
      <legend className="sr-only">Add a row for</legend>
      <div className="flex flex-wrap gap-x-5 gap-y-2">
        {ALL_EVENTS.map((e) => (
          <label key={e} className="flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              className="accent-black"
              checked={value.includes(e)}
              // One stays on, since a tab that gets nothing should be removed instead.
              disabled={disabled || (value.length === 1 && value.includes(e))}
              onChange={(ev) => onChange(ALL_EVENTS.filter((x) => (x === e ? ev.target.checked : value.includes(x))))}
            />
            {SHEET_EVENTS[e]}
          </label>
        ))}
      </div>
    </fieldset>
  );
}

/** Opens a sheet by its link, picks a tab, and sets it up to import from, with its columns mapped, or to send lead events to. */
function SetUp({ projectId, onDone }: { projectId: string; onDone: (s: SheetsStatus) => void }) {
  const { mutate } = useSWRConfig();
  const { pending, run } = useAction();
  const [mode, setMode] = useState<"import" | "export">("import");
  const [url, setUrl] = useState("");
  const [opened, setOpened] = useState<Opened | null>(null);
  const [tab, setTab] = useState("");
  const [preview, setPreview] = useState<Preview | null>(null);
  const [mapping, setMapping] = useState<Record<string, string>>({});
  const [segmentId, setSegmentId] = useState("");
  const [keep, setKeep] = useState(true);
  const [events, setEvents] = useState<SheetEvent[]>(ALL_EVENTS);
  const [result, setResult] = useState<Result | null>(null);

  const readTab = async (t: string) => {
    setPreview(null);
    const p = await api<Preview>(`/projects/${projectId}/sheets/preview`, { method: "POST", body: { url, tab: t } });
    setPreview(p);
    setMapping(p.suggested_mapping);
  };
  const open = () =>
    run("open", async () => {
      const o = await api<Opened>(`/projects/${projectId}/sheets/open`, { method: "POST", body: { url } });
      setOpened(o);
      setTab(o.tab ?? "");
      setResult(null);
      setPreview(null);
      if (mode === "import" && o.tab) await readTab(o.tab);
    });
  const pickTab = (t: string) => {
    setTab(t);
    if (mode === "import") run("preview", () => readTab(t));
  };
  const switchMode = (m: "import" | "export") => {
    setMode(m);
    if (m === "import" && opened && tab && !preview) run("preview", () => readTab(tab));
  };
  const create = () =>
    run(
      "create",
      async () => {
        const body =
          mode === "import"
            ? { kind: "import", url, tab, mapping: Object.fromEntries(Object.entries(mapping).filter(([, h]) => h)), segment_id: segmentId || null, keep_importing: keep }
            : { kind: "export", url, tab, events };
        const created = await api<Created>(`/projects/${projectId}/sheets/connections`, { method: "POST", body });
        onDone(created);
        if (mode === "import") {
          setResult({ name: `${opened?.name} › ${tab}`, import: created.import, error: created.error });
          mutate((k) => typeof k === "string" && (k.includes("/imports") || k.includes("/leads") || k.includes("/segments")));
        }
        setUrl("");
        setOpened(null);
        setPreview(null);
      },
      mode === "export" ? `Splee adds a row to ${tab} for each new event` : undefined,
    );

  return (
    <div className="rounded-lg border border-border p-4">
      <Tabs
        value={mode}
        onChange={switchMode}
        options={[
          { value: "import", label: "Import leads" },
          { value: "export", label: "Send lead events" },
        ]}
      />
      <p className="mt-3 text-sm text-pretty text-muted-foreground">
        {mode === "import"
          ? "The first row names the columns, and each row under it becomes a lead. Suppressed addresses and leads you already have are skipped."
          : "Each hot lead, booked meeting and won deal adds a row with their name, email, company, campaign, status, reply and a link to them in Splee."}
      </p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          open();
        }}
        className="mt-4 flex flex-col gap-2 sm:flex-row sm:items-end"
      >
        <Field label="Sheet link" className="min-w-0 flex-1">
          <Input value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://docs.google.com/spreadsheets/d/…" inputMode="url" />
        </Field>
        <Button type="submit" variant="outline" loading={pending === "open"} disabled={!url.trim()} className="sm:self-end">
          Open sheet
        </Button>
      </form>

      {opened && (
        <div className="mt-4 flex flex-col gap-4">
          <Field label={`Tab in ${opened.name}`} className="sm:w-80">
            <Select value={tab} onChange={(e) => pickTab(e.target.value)}>
              {opened.tabs.map((t) => (
                <option key={t.id} value={t.title}>
                  {t.title}
                </option>
              ))}
            </Select>
          </Field>
          {mode === "export" ? (
            <>
              <div>
                <p className="text-sm font-medium">Add a row for</p>
                <EventChecks value={events} onChange={setEvents} />
                <p className="mt-2 text-xs text-pretty text-muted-foreground">An empty tab gets a header row first. Each lead gets one row per event, never two.</p>
              </div>
              <Button onClick={create} loading={pending === "create"} disabled={!tab} className="sm:self-start">
                Send events to this tab
              </Button>
            </>
          ) : preview ? (
            <>
              <div>
                <p className="text-sm font-medium">Columns</p>
                <p className="text-xs text-pretty text-muted-foreground">Guessed from the headers. Change any that are wrong.</p>
                <div className="mt-3 grid gap-x-4 gap-y-3 sm:grid-cols-2">
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
              </div>
              <CampaignField projectId={projectId} value={segmentId} onChange={setSegmentId} className="sm:w-80" />
              <div className="flex items-center gap-3">
                <Switch checked={keep} onChange={setKeep} label="Keep importing" />
                <span className="text-sm text-pretty">Keep importing new rows every 15 minutes</span>
              </div>
              <Button onClick={create} loading={pending === "create"} disabled={!mapping.email} className="sm:self-start">
                Import leads
              </Button>
            </>
          ) : (
            pending === "preview" && <p className="text-sm text-muted-foreground">Reading the tab…</p>
          )}
        </div>
      )}

      {result && <ImportResult projectId={projectId} result={result} />}
    </div>
  );
}

function ImportResult({ projectId, result: { name, import: done, error } }: { projectId: string; result: Result }) {
  const errors = done?.errors ?? [];
  return (
    <div className={`mt-4 rounded-lg px-3 py-3 text-sm ${error ? "bg-red-50 text-red-800" : "bg-brand-50 text-brand-800"}`} role="status">
      <p className="font-medium wrap-anywhere">{name}</p>
      <p className="mt-1 text-pretty">
        {error
          ? `Set up, but the first read didn't go through: ${error}`
          : done
            ? `Added ${plural(done.imported, "lead")}${done.skipped ? `, skipped ${num(done.skipped)}` : ""}.`
            : "No rows with leads yet. New ones import as they come in."}
      </p>
      {!!errors.length && (
        <ul className="mt-2 flex flex-col gap-0.5 text-xs">
          {errors.slice(0, 3).map((e) => (
            <li key={e.row}>
              Row {e.row}: {e.error}
            </li>
          ))}
          {errors.length > 3 && <li>and {plural(errors.length - 3, "more row")}</li>}
        </ul>
      )}
      {done && (
        <Link href={`/app/p/${projectId}/leads`} className="mt-2 inline-block text-xs font-medium underline underline-offset-4">
          See the leads
        </Link>
      )}
    </div>
  );
}
