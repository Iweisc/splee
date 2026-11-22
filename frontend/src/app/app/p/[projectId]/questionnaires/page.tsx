"use client";

import { use, useState } from "react";
import Link from "next/link";
import { useRouter } from "next/navigation";
import useSWR from "swr";
import { ChevronRight, ClipboardCheck, FileSpreadsheet, Loader2, Upload } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { LoadError, toastError } from "@/components/app/work/states";
import { Button, buttonClass } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Card, EmptyState, Skeleton, Tabs } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { QuestionnaireList, QuestionnaireSummary } from "@/lib/types";
import { cn, plural, timeAgo } from "@/lib/utils";

export default function QuestionnairesPage({ params }: PageProps<"/app/p/[projectId]/questionnaires">) {
  const { projectId } = use(params);
  const key = `/projects/${projectId}/questionnaires`;
  const { data, error, mutate } = useSWR<QuestionnaireList>(key, fetcher, {
    refreshInterval: (v) => (v?.questionnaires.some((q) => q.answering) ? 3000 : 0),
  });
  return (
    <>
      <PageHeader
        title="Questionnaires"
        description="Security reviews and RFPs, answered from your Materials and the answers you approved before. Approve them, then download the file with the answers filled in."
      />
      <PageBody className="flex flex-col gap-6">
        <NewQuestionnaire projectId={projectId} path={key} />
        {error ? (
          <Card>
            <LoadError error={error} retry={() => mutate()} />
          </Card>
        ) : !data ? (
          <Skeleton className="h-40 rounded-2xl" />
        ) : data.questionnaires.length === 0 ? (
          <Card>
            <EmptyState icon={<ClipboardCheck className="size-5" />} title="No questionnaires yet">
              {data.library > 0
                ? `Your answer library has ${plural(data.library, "approved answer")}, which answer the same questions next time.`
                : "Each answer you approve goes into your answer library, which answers the same question in the next one."}
            </EmptyState>
          </Card>
        ) : (
          <div className="flex flex-col gap-2">
            <p className="text-sm text-muted-foreground">
              Your answer library has {plural(data.library, "approved answer")}. Each one answers the same question in the next questionnaire.
            </p>
            <Card className="divide-y divide-border">
              {data.questionnaires.map((q) => (
                <Row key={q.id} q={q} projectId={projectId} />
              ))}
            </Card>
          </div>
        )}
      </PageBody>
    </>
  );
}

function Row({ q, projectId }: { q: QuestionnaireSummary; projectId: string }) {
  const pct = (n: number) => (q.questions ? (n / q.questions) * 100 : 0);
  return (
    <Link href={`/app/p/${projectId}/questionnaires/${q.id}`} className="flex items-center gap-4 p-4 hover:bg-accent/50">
      <div className="min-w-0 flex-1">
        <p className="font-medium text-pretty sm:truncate">{q.name}</p>
        <p className="mt-0.5 text-xs text-muted-foreground">
          {plural(q.questions, "question")}, added {timeAgo(q.created_at)}
        </p>
      </div>
      <div className="hidden w-48 shrink-0 sm:block">
        <div className="relative h-1.5 overflow-hidden rounded-full bg-black/5">
          <div className="absolute inset-y-0 left-0 rounded-full bg-brand-200" style={{ width: `${pct(q.answered)}%` }} />
          <div className="absolute inset-y-0 left-0 rounded-full bg-brand-600" style={{ width: `${pct(q.approved)}%` }} />
        </div>
        <p className="mt-1 text-xs text-muted-foreground tabular-nums">
          {q.approved} of {q.questions} approved
        </p>
      </div>
      <span className="shrink-0 text-right text-sm sm:w-32">
        {q.answering ? (
          <span className="inline-flex items-center gap-1.5 text-muted-foreground">
            <Loader2 className="size-3.5 animate-spin" /> Answering…
          </span>
        ) : q.unknown > 0 ? (
          <span className="font-medium text-amber-700">{q.unknown} {q.unknown === 1 ? "needs" : "need"} you</span>
        ) : q.approved === q.questions ? (
          <span className="font-medium text-brand-700">Done</span>
        ) : null}
      </span>
      <ChevronRight className="size-4 shrink-0 text-muted-foreground" />
    </Link>
  );
}

function NewQuestionnaire({ projectId, path }: { projectId: string; path: string }) {
  const router = useRouter();
  const [mode, setMode] = useState<"file" | "paste">("file");
  const [name, setName] = useState("");
  const [file, setFile] = useState<File | null>(null);
  const [text, setText] = useState("");
  const [busy, setBusy] = useState(false);
  const ready = mode === "file" ? !!file : !!text.trim();
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    const form = new FormData();
    form.set("name", name);
    if (mode === "file" && file) form.set("file", file);
    else form.set("text", text);
    setBusy(true);
    try {
      const q = await api<QuestionnaireSummary>(path, { body: form });
      router.push(`/app/p/${projectId}/questionnaires/${q.id}`);
    } catch (err) {
      toastError(err);
      setBusy(false);
    }
  };
  return (
    <Card className="p-5">
      <form onSubmit={submit} className="flex flex-col gap-4">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <h2 className="font-medium">Answer a questionnaire</h2>
          <Tabs
            value={mode}
            onChange={setMode}
            options={[
              { value: "file", label: "Upload the file" },
              { value: "paste", label: "Paste questions" },
            ]}
          />
        </div>
        <div className="grid gap-4 sm:grid-cols-[16rem_minmax(0,1fr)]">
          <Field label="Name">
            <Input value={name} onChange={(e) => setName(e.target.value)} maxLength={200} placeholder="Like Northpine security review" />
          </Field>
          {/* Hidden rather than unmounted on the Paste tab, so the input keeps its file and never says none is chosen. */}
          <Field
            label="CSV or Excel file"
            className={cn("min-w-0", mode !== "file" && "hidden")}
            hint="The questions can be in any column. The download keeps every column and fills in the answers."
          >
            <span className="flex h-10 min-w-0 items-center gap-3">
              <span
                className={buttonClass(
                  "outline",
                  "md",
                  "shrink-0 cursor-pointer has-focus-visible:outline-2 has-focus-visible:outline-offset-2 has-focus-visible:outline-brand-600",
                )}
              >
                <FileSpreadsheet className="size-4" /> {file ? "Choose another" : "Choose a file"}
                <input
                  type="file"
                  accept=".csv,.xlsx,.xlsm,.xls,.ods,text/csv"
                  className="sr-only"
                  onChange={(e) => setFile(e.target.files?.[0] ?? null)}
                />
              </span>
              <span aria-hidden title={file?.name} className={cn("min-w-0 truncate text-sm", file ? "text-foreground" : "text-muted-foreground")}>
                {file?.name ?? "No file chosen"}
              </span>
            </span>
          </Field>
          {mode === "paste" && (
            <Field label="One question a line">
              <Textarea
                value={text}
                onChange={(e) => setText(e.target.value)}
                rows={4}
                placeholder={"Do you encrypt customer data at rest?\nWhere is customer data stored?"}
                className="min-h-[calc(4lh+1.125rem)] resize-none field-sizing-content"
              />
            </Field>
          )}
        </div>
        <div className="flex justify-end">
          <Button type="submit" loading={busy} disabled={!ready}>
            <Upload className="size-4" /> Answer it
          </Button>
        </div>
      </form>
    </Card>
  );
}
