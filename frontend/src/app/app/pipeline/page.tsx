"use client";

import { use, useState } from "react";
import { HandCoins, Settings2 } from "lucide-react";
import useSWR from "swr";
import { PageBody, PageHeader, ScreenPage } from "@/components/app/page-header";
import { amountsOf, DealDialog, moneyList } from "@/components/app/work/deal-dialog";
import { type Kpi, KpiCard } from "@/components/app/work/kpi-card";
import { CloseDialog, type MoveExtras, PipelineBoard, PipelineList } from "@/components/app/work/pipeline-board";
import { StageEditor } from "@/components/app/work/stage-editor";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { Select } from "@/components/ui/form";
import { Card, Skeleton, Tabs } from "@/components/ui/misc";
import { api, qs } from "@/lib/api";
import type { Deal, DealStage, Pipeline, ProjectSummary } from "@/lib/types";
import { cn, plural } from "@/lib/utils";
import { fetcher, useMe } from "@/hooks/use-me";

type View = "board" | "list";

// Phones can't drag, so they start on the list. md is where the sidebar's phone menu ends too.
const startView = (): View => (typeof window !== "undefined" && matchMedia("(max-width: 47.99rem)").matches ? "list" : "board");

export default function PipelinePage({ searchParams }: PageProps<"/app/pipeline">) {
  // Webhooks and the lead window link here with ?deal= to open that deal.
  const { deal: linked } = use(searchParams);
  const { me } = useMe();
  const [project, setProject] = useState("");
  const [view, setView] = useState<View>(startView);
  const [opened, setOpened] = useState<string | null>(typeof linked === "string" ? linked : null);
  const [closing, setClosing] = useState<{ deal: Deal; stage: DealStage } | null>(null);
  const [editing, setEditing] = useState(false);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const key = `/pipeline${qs({ project_id: project })}`;
  const { data, error, mutate } = useSWR<Pipeline>(key, fetcher, { keepPreviousData: true });
  // A deal that closed more than 90 days ago isn't on the board, so a link to one loads it by itself.
  const onBoard = data?.deals.find((d) => d.id === opened);
  const { data: linkedDeal } = useSWR<Deal>(opened && data && !onBoard ? `/deals/${opened}` : null, fetcher);
  const openDeal = onBoard ?? linkedDeal;

  // The card moves at once and the forecast catches up when the server answers.
  const move = async (deal: Deal, stage: DealStage, extras: MoveExtras = {}) => {
    const moved: Deal = {
      ...deal,
      stage_id: stage.id,
      stage: stage.name,
      status: stage.kind,
      value: extras.value ?? deal.value,
      stage_changed_at: new Date().toISOString(),
    };
    try {
      await mutate(
        api(`/deals/${deal.id}/move`, { body: { stage_id: stage.id, ...extras } }).then(() => undefined),
        {
          // The board only offers moves once it has loaded.
          optimisticData: (current) => ({ ...current!, deals: current!.deals.map((d) => (d.id === deal.id ? moved : d)) }),
          populateCache: false,
          revalidate: true,
        },
      );
    } catch (e) {
      toastError(e);
    }
  };

  // Closing a deal asks for its final value or a reason first. Moving between open stages, or between two won stages, doesn't.
  const request = (deal: Deal, stage: DealStage) => {
    if (stage.id === deal.stage_id) return;
    if (stage.kind !== "open" && stage.kind !== deal.status) setClosing({ deal, stage });
    else move(deal, stage);
  };

  const admin = me?.role === "owner" || me?.role === "admin";
  const projectPicker = (projects?.length ?? 0) > 1 && (
    <Select value={project} onChange={(e) => setProject(e.target.value)} aria-label="Project" className="h-8 w-auto max-sm:basis-full">
      <option value="">All projects</option>
      {projects?.map((p) => (
        <option key={p.id} value={p.id}>
          {p.name || p.domain}
        </option>
      ))}
    </Select>
  );

  return (
    <ScreenPage>
      <PageHeader
        title="Pipeline"
        description="Hot replies open a deal by themselves, and booked meetings move it along. Drag a deal to another stage as it moves."
        actions={
          <>
            {projectPicker}
            <Tabs
              value={view}
              onChange={setView}
              options={[
                { value: "board", label: "Board" },
                { value: "list", label: "List" },
              ]}
              className="max-sm:flex-1 max-sm:[&>button]:flex-1"
            />
            {admin && data && (
              <Button variant="outline" size="sm" onClick={() => setEditing(true)}>
                <Settings2 className="size-3.5" /> Edit stages
              </Button>
            )}
          </>
        }
      />
      <PageBody className={cn("flex min-h-0 flex-1 flex-col gap-4", view === "list" && "overflow-y-auto")}>
        {error && !data ? (
          <LoadError error={error} retry={() => mutate()} what="the pipeline" />
        ) : !data ? (
          <>
            <Skeleton className="h-[86px] rounded-2xl" />
            <div className="flex gap-3 overflow-hidden">
              {Array.from({ length: 5 }, (_, i) => (
                <Skeleton key={i} className="h-80 w-64 shrink-0 rounded-2xl" />
              ))}
            </div>
          </>
        ) : (
          <>
            <Forecast pipeline={data} />
            {data.deals.length === 0 && <EmptyHint stages={data.stages} />}
            {view === "board" ? (
              <PipelineBoard stages={data.stages} deals={data.deals} currency={data.currency} onMove={request} onOpen={(d) => setOpened(d.id)} />
            ) : (
              <PipelineList stages={data.stages} deals={data.deals} currency={data.currency} onMove={request} onOpen={(d) => setOpened(d.id)} />
            )}
          </>
        )}
      </PageBody>
      {closing && (
        <CloseDialog
          key={closing.deal.id}
          deal={closing.deal}
          stage={closing.stage}
          onCancel={() => setClosing(null)}
          onConfirm={async (extras) => {
            setClosing(null);
            await move(closing.deal, closing.stage, extras);
          }}
        />
      )}
      {openDeal && <DealDialog key={openDeal.id} deal={openDeal} onClose={() => setOpened(null)} onSaved={() => setOpened(null)} />}
      {editing && data && (
        <StageEditor
          pipeline={data}
          onClose={() => setEditing(false)}
          onSaved={() => {
            setEditing(false);
            mutate();
          }}
        />
      )}
    </ScreenPage>
  );
}

function Forecast({ pipeline: { forecast: f, currency, deals } }: { pipeline: Pipeline }) {
  const monthStart = new Date();
  monthStart.setUTCDate(1);
  monthStart.setUTCHours(0, 0, 0, 0);
  const open = deals.filter((d) => d.status === "open").length;
  const wonThisMonth = deals.filter((d) => d.status === "won" && d.closed_at && new Date(d.closed_at) >= monthStart).length;
  const closed = f.won + f.lost;
  const kpis: Kpi[] = [
    { label: "Open pipeline", value: moneyList(amountsOf(f.amounts, "open"), currency), note: plural(open, "open deal") },
    { label: "Weighted", value: moneyList(amountsOf(f.amounts, "weighted"), currency), note: "By stage probability" },
    { label: "Won this month", value: moneyList(amountsOf(f.amounts, "won_this_month"), currency), note: plural(wonThisMonth, "deal") },
    {
      label: "Win rate",
      value: f.win_rate === null ? "–" : `${Math.round(f.win_rate * 100)}%`,
      note: closed ? `${f.won} won, ${f.lost} lost in 90 days` : "None closed in 90 days",
    },
  ];
  return <KpiCard kpis={kpis} className="shrink-0 grid-cols-2 lg:grid-cols-4" />;
}

function EmptyHint({ stages }: { stages: DealStage[] }) {
  const booked = stages.find((s) => s.auto === "booked" && s.kind === "open");
  return (
    <Card className="flex shrink-0 items-start gap-3 px-4 py-3 text-sm">
      <HandCoins className="mt-0.5 size-4 shrink-0 text-brand-600" />
      <p className="text-pretty text-muted-foreground">
        No deals yet. The next hot reply opens one here{booked ? `, and a booked meeting moves it to ${booked.name}` : ""}. To start one yourself, open a lead
        or an inbox thread and choose Create deal.
      </p>
    </Card>
  );
}
