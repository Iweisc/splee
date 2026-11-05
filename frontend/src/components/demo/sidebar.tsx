"use client";

import { useState } from "react";
import { ChevronDown, ChevronUp, ExternalLink } from "lucide-react";
import { Logo } from "@/components/ui/logo";
import { Favicon } from "@/components/ui/misc";
import type { AgentEvent, DemoState } from "@/lib/types";
import { cn, compact } from "@/lib/utils";
import { CompanyChips, CompanyFacts, DetailsLookOff } from "./company";
import { STEP_COUNT, STEPS, segmentIcon } from "./steps";

const VISIBLE_COMPETITORS = 8;
const VISIBLE_LOG_LINES = 8;

export function Sidebar({
  state,
  finished,
  current,
  working,
  logs,
  activeSegmentId,
  onCorrect,
}: {
  state: DemoState | undefined;
  finished: Set<number>;
  current: number;
  working: boolean;
  logs: AgentEvent[];
  activeSegmentId: string | null;
  onCorrect: () => void;
}) {
  return (
    <aside className="flex w-full shrink-0 flex-col border-b border-border px-3.5 pt-3 pb-10 md:h-dvh md:w-[300px] md:overflow-y-auto md:border-r md:border-b-0">
      <Logo className="mb-3" />
      {state &&
        STEPS.filter((s) => finished.has(s.n)).map((s) => (
          <section key={s.n} className="border-t border-border py-4">
            <p className="font-mono text-xs text-muted-foreground">
              ✓ step {s.n} · {s.title}
            </p>
            {s.n === 1 && <CompanyCompact state={state} onCorrect={onCorrect} />}
            {s.n === 2 && <CompetitorsCompact state={state} />}
            {s.n === 3 && <CampaignsCompact state={state} activeSegmentId={activeSegmentId} />}
          </section>
        ))}
      {working && <WorkingLog step={current} logs={logs} />}
    </aside>
  );
}

function CompanyCompact({ state, onCorrect }: { state: DemoState; onCorrect: () => void }) {
  const [open, setOpen] = useState(true);
  const [more, setMore] = useState(false);
  const { project } = state;
  const company = project.company;
  const hasFacts = !!(company.industry || company.products?.length || company.value_props?.length);
  return (
    <div className="mt-4">
      <button type="button" onClick={() => setOpen((o) => !o)} className="flex w-full items-center gap-2 text-left">
        {project.domain && <Favicon domain={project.domain} size={20} />}
        <span className="min-w-0 flex-1 truncate">
          <span className="font-semibold">{company.name || project.name}</span>{" "}
          {project.domain && <span className="text-muted-foreground">{project.domain}</span>}
        </span>
        {open ? <ChevronUp className="size-4 text-muted-foreground" /> : <ChevronDown className="size-4 text-muted-foreground" />}
      </button>
      {open && (
        <div className="mt-3 flex flex-col gap-3">
          {company.description && <p className="text-sm leading-relaxed text-muted-foreground">{company.description}</p>}
          <DetailsLookOff onClick={onCorrect} />
          <CompanyChips company={company} />
          {hasFacts && (
            <button type="button" onClick={() => setMore((m) => !m)} className="flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground">
              <ChevronDown className={cn("size-4 transition-transform", more && "rotate-180")} />
              {more ? "Less" : "More"}
            </button>
          )}
          {more && <CompanyFacts company={company} />}
        </div>
      )}
    </div>
  );
}

function SectionLabel({ label, count }: { label: string; count: number }) {
  return (
    <p className="mt-4 mb-2.5 text-xs tracking-wider text-muted-foreground uppercase">
      {label} <span className="ml-1.5">{count}</span>
    </p>
  );
}

function CompetitorsCompact({ state }: { state: DemoState }) {
  const [all, setAll] = useState(false);
  const list = all ? state.competitors : state.competitors.slice(0, VISIBLE_COMPETITORS);
  const hidden = state.competitors.length - VISIBLE_COMPETITORS;
  return (
    <div>
      <SectionLabel label="Competitors" count={state.competitors.length} />
      <div className="grid grid-cols-2 gap-1.5">
        {list.map((c) => (
          <a
            key={c.id}
            href={`https://${c.domain}`}
            target="_blank"
            rel="noreferrer"
            title={c.name || c.domain}
            className="flex min-w-0 items-center gap-1.5 rounded-md border border-border bg-card px-2 py-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground"
          >
            <Favicon domain={c.domain} size={14} />
            <span className="min-w-0 flex-1 truncate">{c.domain}</span>
            <ExternalLink className="size-3.5 shrink-0" />
          </a>
        ))}
      </div>
      {hidden > 0 && (
        <button type="button" onClick={() => setAll((a) => !a)} className="mt-2.5 flex items-center gap-1.5 text-sm text-muted-foreground hover:text-foreground">
          <ChevronDown className={cn("size-4 transition-transform", all && "rotate-180")} />
          {all ? "Show less" : `+${hidden} more`}
        </button>
      )}
    </div>
  );
}

function CampaignsCompact({ state, activeSegmentId }: { state: DemoState; activeSegmentId: string | null }) {
  return (
    <div>
      <SectionLabel label="Campaigns" count={state.segments.length} />
      <ul className="flex flex-col gap-1.5">
        {state.segments.map((s) => {
          const Icon = segmentIcon(s.icon);
          const active = s.id === activeSegmentId;
          return (
            <li
              key={s.id}
              className={cn(
                "rounded-lg border bg-card px-2.5 py-2",
                active ? "border-foreground/80 text-foreground shadow-sm" : "border-border text-muted-foreground",
              )}
            >
              <div className="flex items-center gap-2.5">
                <span className="flex size-5 shrink-0 items-center justify-center rounded bg-accent">
                  <Icon className="size-3" />
                </span>
                <span className={cn("min-w-0 flex-1 truncate text-sm", active && "font-medium")}>{s.name}</span>
                <span className="text-sm tabular-nums">{compact(s.est_size)}</span>
              </div>
              <div className="mt-1.5 ml-[30px] h-0.5 w-16 rounded-full bg-black/[0.06]">
                <div className={cn("h-full rounded-full", active ? "bg-foreground/80" : "bg-foreground/30")} style={{ width: `${s.fit_score}%` }} />
              </div>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function WorkingLog({ step, logs }: { step: number; logs: AgentEvent[] }) {
  const lines = logs.slice(-VISIBLE_LOG_LINES);
  return (
    <div className="border-t border-border pt-4">
      <p className="flex items-center gap-2 text-[13px] font-medium">
        <span className="size-1.5 animate-pulse rounded-full bg-brand-500" />
        Splee agent is working on step {step} of {STEP_COUNT}
      </p>
      <ul className="mt-3 flex flex-col gap-1.5 font-mono text-[12.5px]">
        {lines.map((l, i) => {
          const last = i === lines.length - 1;
          return (
            <li key={l.id} className={cn("flex min-w-0 items-center gap-2", last ? "text-foreground" : "text-muted-foreground")}>
              <span className={cn("shrink-0", last && "text-brand-600")}>{last ? "›" : "✓"}</span>
              <span className="min-w-0 truncate">{l.message}</span>
              {last && <Cursor />}
            </li>
          );
        })}
        {lines.length === 0 && (
          <li className="flex items-center gap-2">
            <span className="text-brand-600">›</span>
            starting…
            <Cursor />
          </li>
        )}
      </ul>
    </div>
  );
}

function Cursor() {
  return <span className="inline-block h-3.5 w-[7px] shrink-0 animate-blink bg-foreground" aria-hidden />;
}
