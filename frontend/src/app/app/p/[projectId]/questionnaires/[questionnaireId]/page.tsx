"use client";

import { use, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import useSWR from "swr";
import { ArrowLeft, Check, Download, Loader2, RefreshCw, Trash2 } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button, buttonClass } from "@/components/ui/button";
import { Textarea } from "@/components/ui/form";
import { Badge, type BadgeTone, Card, Skeleton, Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { QuestionnaireDetail, QuestionnaireItem } from "@/lib/types";
import { plural } from "@/lib/utils";

type Filter = "review" | "all" | "approved";

const CONFIDENCE: Record<NonNullable<QuestionnaireItem["confidence"]>, { label: string; tone: BadgeTone }> = {
  library: { label: "From your library", tone: "green" },
  sourced: { label: "Sourced", tone: "green" },
  partial: { label: "Check this", tone: "amber" },
  unknown: { label: "Needs you", tone: "red" },
};

const needsReview = (i: QuestionnaireItem) => !i.approved_at;

export default function QuestionnairePage({ params }: PageProps<"/app/p/[projectId]/questionnaires/[questionnaireId]">) {
  const { projectId, questionnaireId } = use(params);
  const router = useRouter();
  const key = `/questionnaires/${questionnaireId}`;
  const { data, error, mutate } = useSWR<QuestionnaireDetail>(key, fetcher, { refreshInterval: (d) => (d?.answering ? 2500 : 0) });
  const [filter, setFilter] = useState<Filter>("review");
  const [busy, setBusy] = useState<string | null>(null);
  const back = `/app/p/${projectId}/questionnaires`;

  const act = async (what: string, call: () => Promise<QuestionnaireDetail>) => {
    setBusy(what);
    try {
      await mutate(call(), { revalidate: false });
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const remove = async () => {
    if (!window.confirm("Delete this questionnaire? Approved answers stay in your library.")) return;
    try {
      await api(key, { method: "DELETE" });
      router.push(back);
    } catch (e) {
      toastError(e);
    }
  };

  const items = data?.items ?? [];
  const shown = items.filter((i) => (filter === "review" ? needsReview(i) : filter === "approved" ? !!i.approved_at : true));
  const confident = items.filter((i) => !i.approved_at && i.answer && (i.confidence === "library" || i.confidence === "sourced")).length;

  return (
    <>
      <PageHeader
        title={
          <span className="flex items-center gap-2">
            <Link href={back} aria-label="All questionnaires" className="rounded-lg p-1 text-muted-foreground hover:bg-accent hover:text-foreground">
              <ArrowLeft className="size-5" />
            </Link>
            {data?.name ?? "Questionnaire"}
          </span>
        }
        description={
          data &&
          (data.answering
            ? `The agent is answering ${plural(data.questions, "question")} from your Materials and library…`
            : `${data.approved} of ${data.questions} approved. ${data.unknown > 0 ? `${data.unknown} couldn't be sourced and ${data.unknown === 1 ? "needs" : "need"} you.` : "Everything has a source."}`)
        }
        actions={
          data && (
            <div className="flex flex-wrap items-center gap-2">
              {data.unknown > 0 && !data.answering && (
                <Button
                  type="button"
                  variant="ghost"
                  size="sm"
                  onClick={() => act("again", () => api(`${key}/answer`, { method: "POST" }))}
                  loading={busy === "again"}
                  title="After adding to Materials"
                >
                  <RefreshCw className="size-3.5" /> Try again on {data.unknown}
                </Button>
              )}
              {confident > 0 && (
                <Button
                  type="button"
                  variant="outline"
                  size="sm"
                  onClick={() => act("approve", () => api(`${key}/approve`, { method: "POST" }))}
                  loading={busy === "approve"}
                >
                  <Check className="size-3.5" /> Approve the {confident} sourced
                </Button>
              )}
              <a href={`/api${key}/download`} download className={buttonClass("primary", "sm")}>
                <Download className="size-3.5" /> Download
              </a>
            </div>
          )
        }
      />
      <PageBody className="flex flex-col gap-4">
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <Skeleton className="h-96 rounded-2xl" />
        ) : (
          <>
            <div className="flex flex-wrap items-center justify-between gap-3">
              <Tabs
                value={filter}
                onChange={setFilter}
                options={[
                  { value: "review", label: `To review · ${items.filter(needsReview).length}` },
                  { value: "approved", label: `Approved · ${data.approved}` },
                  { value: "all", label: `All · ${items.length}` },
                ]}
              />
              <Button type="button" variant="ghost" size="sm" onClick={remove} className="text-muted-foreground">
                <Trash2 className="size-3.5" /> Delete
              </Button>
            </div>
            {shown.length === 0 ? (
              <p className="py-8 text-center text-sm text-muted-foreground">
                {filter === "review" ? "Nothing left to review. Download the file and send it back." : "Nothing here yet."}
              </p>
            ) : (
              <ol className="flex flex-col gap-3">
                {shown.map((i) => (
                  <ItemRow key={i.id} item={i} busy={busy === i.id} answering={data.answering} act={act} />
                ))}
              </ol>
            )}
          </>
        )}
      </PageBody>
    </>
  );
}

function ItemRow({
  item,
  busy,
  answering,
  act,
}: {
  item: QuestionnaireItem;
  busy: boolean;
  answering: boolean;
  act: (what: string, call: () => Promise<QuestionnaireDetail>) => Promise<void>;
}) {
  const [text, setText] = useState(item.answer);
  // The agent's answer arrives after the row does. Take it unless someone already started typing.
  const [seen, setSeen] = useState(item.answer);
  if (item.answer !== seen) {
    setSeen(item.answer);
    if (!text.trim() || text === seen) setText(item.answer);
  }
  const save = (approve: boolean) => act(item.id, () => api(`/questionnaire-items/${item.id}`, { body: { answer: text, approve } }));
  const edited = text.trim() !== item.answer.trim();
  const c = item.confidence ? CONFIDENCE[item.confidence] : null;
  return (
    <li>
      <Card className="flex flex-col gap-3 p-4">
        <div className="flex items-start gap-3">
          <span className="mt-0.5 w-7 shrink-0 text-right text-sm text-muted-foreground tabular-nums">{item.number}</span>
          <div className="min-w-0 flex-1">
            {item.section && <p className="text-xs font-medium text-muted-foreground">{item.section}</p>}
            <p className="font-medium text-pretty">{item.question}</p>
          </div>
          {item.approved_at ? (
            <Badge tone="green">
              <Check className="mr-1 inline size-3" /> Approved
            </Badge>
          ) : (
            c && <Badge tone={c.tone}>{c.label}</Badge>
          )}
        </div>
        <div className="flex flex-col gap-2 sm:pl-10">
          {!item.answer && answering ? (
            <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
              <Loader2 className="size-3.5 animate-spin" /> Answering…
            </p>
          ) : (
            <Textarea
              value={text}
              onChange={(e) => setText(e.target.value)}
              maxLength={4000}
              rows={2}
              aria-label={`Answer to question ${item.number}`}
              className="min-h-[calc(2lh+1.125rem)] resize-none text-sm field-sizing-content"
            />
          )}
          <div className="flex flex-wrap items-center justify-end gap-2">
            {item.sources.length > 0 && <p className="mr-auto text-xs text-muted-foreground">From {item.sources.join(", ")}</p>}
            {item.approved_at && !edited && (
              <Button type="button" size="sm" variant="ghost" onClick={() => save(false)} disabled={busy}>
                Reopen
              </Button>
            )}
            {edited && !item.approved_at && (
              <Button type="button" size="sm" variant="ghost" onClick={() => save(false)} disabled={busy}>
                Save
              </Button>
            )}
            {(!item.approved_at || edited) && (
              <Button type="button" size="sm" onClick={() => save(true)} loading={busy} disabled={!text.trim()}>
                <Check className="size-3.5" /> {item.approved_at ? "Approve the change" : "Approve"}
              </Button>
            )}
          </div>
        </div>
      </Card>
    </li>
  );
}
