"use client";

import { Fragment, useState } from "react";
import { ArrowRightLeft, CircleAlert, CircleCheck, KeyRound } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toastError } from "@/components/app/work/states";
import { Button, LinkButton } from "@/components/ui/button";
import { Field, Input, Select } from "@/components/ui/form";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { OutreachCampaign, OutreachImport, OutreachImportCampaign, OutreachTool } from "@/lib/types";
import { cn, num, plural } from "@/lib/utils";

const TOOLS: Record<OutreachTool, { label: string; hint: string }> = {
  instantly: { label: "Instantly", hint: "From Settings → Integrations → API keys, with read access." },
  smartlead: { label: "Smartlead", hint: "From Settings → Your profile → API key." },
  lemlist: { label: "lemlist", hint: "From Settings → Integrations → API." },
};

const active = (i: OutreachImport) => i.status === "queued" || i.status === "running";

/** The project's latest imports from other tools, polled while one runs. */
export function useOutreachImports(projectId: string) {
  return useSWR<OutreachImport[]>(`/projects/${projectId}/outreach/imports`, fetcher, {
    refreshInterval: (list) => (list?.some(active) ? 2_000 : 0),
  });
}

/**
 * The "Another outreach tool" tab: a key, then the tool's campaigns to pick. The key is sent again to start the import,
 * since the server only keeps it until the import ends.
 */
export function OutreachForm({ projectId, onStarted }: { projectId: string; onStarted: (id: string) => void }) {
  const { mutate } = useSWRConfig();
  const [tool, setTool] = useState<OutreachTool>("instantly");
  const [key, setKey] = useState("");
  const [campaigns, setCampaigns] = useState<OutreachCampaign[] | null>(null);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [blocklist, setBlocklist] = useState(true);
  const [busy, setBusy] = useState(false);
  const { label, hint } = TOOLS[tool];

  const check = async () => {
    setBusy(true);
    try {
      const { campaigns } = await api<{ campaigns: OutreachCampaign[] }>(`/projects/${projectId}/outreach/campaigns`, { body: { tool, api_key: key.trim() } });
      setCampaigns(campaigns);
      setPicked(new Set(campaigns.map((c) => c.id)));
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  const start = async () => {
    if (!campaigns) return;
    setBusy(true);
    try {
      const chosen = campaigns.filter((c) => picked.has(c.id)).map(({ id, name, leads }) => ({ id, name, leads }));
      const started = await api<OutreachImport>(`/projects/${projectId}/outreach/imports`, { body: { tool, api_key: key.trim(), campaigns: chosen, blocklist } });
      await mutate(`/projects/${projectId}/outreach/imports`);
      onStarted(started.id);
      setCampaigns(null);
      setKey("");
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(false);
    }
  };

  if (!campaigns) {
    return (
      <form
        className="flex flex-wrap items-start gap-4"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) check();
        }}
      >
        <Field label="Tool" className="w-full sm:w-40">
          <Select value={tool} onChange={(e) => setTool(e.target.value as OutreachTool)}>
            {Object.entries(TOOLS).map(([value, t]) => (
              <option key={value} value={value}>
                {t.label}
              </option>
            ))}
          </Select>
        </Field>
        <Field label={`${label} API key`} hint={`${hint} Kept only until the import ends.`} className="min-w-56 flex-1">
          <Input type="password" value={key} onChange={(e) => setKey(e.target.value)} autoComplete="off" />
        </Field>
        {/* Level with the fields. On phones it sits under them. */}
        <Button type="submit" variant="outline" className="sm:mt-6" loading={busy} disabled={!key.trim()}>
          <KeyRound className="size-4" /> Check key
        </Button>
      </form>
    );
  }

  const all = campaigns.length > 0 && picked.size === campaigns.length;
  const toggle = (id: string) => {
    const next = new Set(picked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    setPicked(next);
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <p className="min-w-0 text-sm text-muted-foreground">
          {campaigns.length === 0
            ? `${label} has no campaigns on this key.`
            : "Each campaign comes over as a draft with its emails and leads. Nothing is sent until you start it."}
        </p>
        <Button variant="ghost" size="sm" className="shrink-0" onClick={() => setCampaigns(null)}>
          Change key
        </Button>
      </div>
      {campaigns.length > 0 && (
        <div className="overflow-hidden rounded-xl border border-border">
          <label className="flex items-center gap-3 border-b border-border bg-muted px-4 py-2.5 text-sm font-medium">
            <input type="checkbox" checked={all} onChange={() => setPicked(all ? new Set() : new Set(campaigns.map((c) => c.id)))} className="accent-black" />
            {all ? "All campaigns" : `${num(picked.size)} of ${plural(campaigns.length, "campaign")}`}
          </label>
          <ul className="max-h-80 divide-y divide-border overflow-y-auto">
            {campaigns.map((c) => (
              <li key={c.id}>
                <label className="flex cursor-pointer items-center gap-3 px-4 py-2.5 text-sm hover:bg-accent">
                  <input type="checkbox" checked={picked.has(c.id)} onChange={() => toggle(c.id)} className="shrink-0 accent-black" />
                  <span className="min-w-0 flex-1 font-medium wrap-anywhere">{c.name || "Untitled campaign"}</span>
                  {c.status && <span className="text-muted-foreground max-sm:hidden">{c.status}</span>}
                  <span className="w-20 shrink-0 text-right text-muted-foreground tabular-nums">{c.leads === null ? "" : plural(c.leads, "lead")}</span>
                </label>
              </li>
            ))}
          </ul>
        </div>
      )}
      <div className="flex flex-wrap items-center justify-between gap-4">
        <label className="flex items-start gap-2 text-sm">
          <input type="checkbox" checked={blocklist} onChange={(e) => setBlocklist(e.target.checked)} className="mt-0.5 accent-black" />
          <span>
            Add {label}&apos;s blocklist to your <span className="whitespace-nowrap">do-not-contact list</span>
          </span>
        </label>
        <Button onClick={start} loading={busy} disabled={picked.size === 0}>
          <ArrowRightLeft className="size-4" /> Bring over {plural(picked.size, "campaign")}
        </Button>
      </div>
    </div>
  );
}

/** What an import from another tool did, campaign by campaign, live while it runs. */
export function OutreachProgress({ projectId, item }: { projectId: string; item: OutreachImport }) {
  const label = TOOLS[item.tool].label;
  const running = active(item);
  const done = item.campaigns.filter((c) => c.status === "done").length;
  const title = running
    ? `Bringing ${plural(item.campaigns.length, "campaign")} over from ${label}`
    : item.status === "done"
      ? `Brought ${plural(done, "campaign")} over from ${label}`
      : `Couldn't bring campaigns over from ${label}`;
  return (
    <Card className="p-5">
      <p className="flex items-start gap-2 font-medium">
        {running ? (
          <Spinner className="mt-1 size-4 shrink-0" />
        ) : item.status === "done" ? (
          <CircleCheck className="mt-1 size-4 shrink-0 text-brand-600" />
        ) : (
          <CircleAlert className="mt-1 size-4 shrink-0 text-danger" />
        )}
        <span className="min-w-0">{title}</span>
      </p>
      <p className="mt-1 text-sm text-muted-foreground">
        {running ? "You can leave this page. The import keeps going." : "Drafts aren't started. Check each one, then start it from its campaign page."}
      </p>
      <ul className="mt-4 divide-y divide-border rounded-xl border border-border">
        {item.campaigns.map((c) => (
          <CampaignRow key={c.id} projectId={projectId} c={c} />
        ))}
      </ul>
      {item.include_blocklist && !running && !item.error && (
        <p className="mt-3 text-sm text-muted-foreground">
          {item.blocklist_added > 0 ? `${plural(item.blocklist_added, "address", "addresses")} from ${label}'s blocklist added to do-not-contact.` : `${label}'s blocklist had nothing new for do-not-contact.`}
        </p>
      )}
      {item.error && <p className="mt-3 text-sm text-danger">{item.error}</p>}
    </Card>
  );
}

function CampaignRow({ projectId, c }: { projectId: string; c: OutreachImportCampaign }) {
  const counts = [
    `${num(c.added)} added`,
    c.duplicates > 0 && `${num(c.duplicates)} already in Splee`,
    c.suppressed > 0 && `${num(c.suppressed)} on do-not-contact`,
    c.invalid > 0 && `${num(c.invalid)} without a valid email`,
  ].filter(Boolean);
  return (
    <li className="flex flex-wrap items-center gap-x-4 gap-y-1 px-4 py-3 text-sm">
      <div className="min-w-0 flex-1 basis-56">
        <p className="font-medium wrap-anywhere">{c.name || "Untitled campaign"}</p>
        <p className={cn("text-muted-foreground", c.status === "failed" && "text-danger")}>
          {c.status === "queued"
            ? c.leads === null
              ? "Waiting"
              : `Waiting, ${plural(c.leads, "lead")}`
            : c.status === "running"
              ? "Copying emails and leads…"
              : c.status === "failed"
                ? c.error || "Couldn't be brought over"
                : // Each count wraps whole, so "2 on do-not-contact" never splits across lines.
                  [plural(c.steps, "email"), ...counts].map((text, i) => (
                    <Fragment key={i}>
                      {i > 0 && " · "}
                      <span className="whitespace-nowrap">{text}</span>
                    </Fragment>
                  ))}
        </p>
      </div>
      {c.status === "done" && c.segment_id && (
        <LinkButton href={`/app/p/${projectId}/segments/${c.segment_id}`} variant="outline" size="sm" className="shrink-0">
          Open draft
        </LinkButton>
      )}
      {c.status === "running" && <Spinner className="size-4 shrink-0" />}
    </li>
  );
}

/** Shows an import that is running, or one started from this page, above the sources. */
export function OutreachImports({ projectId, startedId }: { projectId: string; startedId: string | null }) {
  const { data } = useOutreachImports(projectId);
  const shown = data?.filter((i) => active(i) || i.id === startedId) ?? [];
  if (shown.length === 0) return null;
  return (
    <>
      {shown.map((i) => (
        <OutreachProgress key={i.id} projectId={projectId} item={i} />
      ))}
    </>
  );
}
