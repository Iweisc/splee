"use client";

import useSWR from "swr";
import { CircleAlert, Printer } from "lucide-react";
import { AnalyticsView, rangeText } from "@/components/app/work/analytics-view";
import { RANGE_LABELS } from "@/components/app/work/share-report";
import { MicroLabel } from "@/components/app/work/states";
import { Button } from "@/components/ui/button";
import { LogoMark } from "@/components/ui/logo";
import { Card, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError } from "@/lib/api";
import type { ReportPerson, SharedReport } from "@/lib/types";

function day(ts: string) {
  return new Date(ts).toLocaleDateString("en-US", { month: "short", day: "numeric" });
}

/** The page behind a client report link. Printing it, or Save as PDF, gives the same page without its buttons. */
export function SharedReportView({ token }: { token: string }) {
  const { data, error, mutate } = useSWR<SharedReport>(`/shared-reports/${encodeURIComponent(token)}`, fetcher, { revalidateOnFocus: false });

  if (!data) {
    return (
      <main className="flex flex-1 flex-col items-center justify-center gap-3 px-5 py-16 text-center text-balance">
        {error instanceof ApiError && error.status === 404 ? (
          <>
            <CircleAlert className="size-6 text-danger" />
            <h1 className="text-xl font-medium">This report link doesn&apos;t work</h1>
            <p className="max-w-sm text-sm text-muted-foreground">It may have expired or been turned off. Ask the person who sent it for a new one.</p>
          </>
        ) : error ? (
          <>
            <p className="text-sm text-muted-foreground">{error.message}</p>
            <Button variant="outline" onClick={() => mutate()}>
              Try again
            </Button>
          </>
        ) : (
          <Spinner />
        )}
      </main>
    );
  }

  const a = data.analytics;
  return (
    <main className="print-report mx-auto flex w-full max-w-6xl flex-col gap-6 px-4 py-6 sm:px-8 sm:py-10 print:p-0">
      <header className="flex flex-wrap items-start justify-between gap-4">
        <div className="min-w-0 max-w-3xl">
          <p className="flex items-center gap-2.5 text-sm font-medium text-muted-foreground">
            {data.logo_url && (
              // A logo from anywhere, so not next/image, which only loads from the hosts in next.config.
              // eslint-disable-next-line @next/next/no-img-element
              <img src={data.logo_url} alt="" className="size-8 shrink-0 rounded-lg bg-card object-contain" />
            )}
            {data.name}
          </p>
          <h1 className="mt-3 text-2xl font-semibold tracking-tight sm:text-3xl">{data.title || `${data.name} outreach report`}</h1>
          <p className="mt-1 text-sm text-muted-foreground">
            {RANGE_LABELS[data.range_preset]}, {rangeText(a.from, a.to)}, compared with {rangeText(a.previous_from, a.previous_to)}. Days are in {a.timezone}.
          </p>
          {data.intro && <p className="mt-4 text-[15px] whitespace-pre-line">{data.intro}</p>}
        </div>
        <Button variant="outline" onClick={() => window.print()} className="print:hidden">
          <Printer className="size-4" /> Save as PDF
        </Button>
      </header>

      <AnalyticsView data={a} shared={{ spend: data.show_spend, mailboxes: data.show_mailboxes }} />

      {data.meetings && data.hot_replies && (
        <div className="grid items-start gap-4 lg:grid-cols-2">
          <People title="Meetings booked" people={data.meetings} empty="No meetings booked in these dates." />
          <People title="Hot replies" people={data.hot_replies} empty="No hot replies in these dates." />
        </div>
      )}

      <footer className="flex items-center justify-center gap-1.5 text-xs text-muted-foreground">
        <LogoMark className="size-3.5" /> Report by Splee
      </footer>
    </main>
  );
}

function People({ title, people, empty }: { title: string; people: ReportPerson[]; empty: string }) {
  return (
    <Card className="p-5">
      <MicroLabel className="mb-2">{title}</MicroLabel>
      {people.length === 0 ? (
        <p className="text-sm text-muted-foreground">{empty}</p>
      ) : (
        <ul className="divide-y divide-border">
          {people.map((p, i) => (
            <li key={i} className="py-3 text-sm break-inside-avoid">
              <div className="flex items-baseline justify-between gap-3">
                <p className="min-w-0">
                  <span className="font-medium">{p.first_name || "Someone"}</span>
                  {p.company && <span className="text-muted-foreground"> at {p.company}</span>}
                </p>
                <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{p.meeting_at ? `Call ${day(p.meeting_at)}` : day(p.at)}</span>
              </div>
              {p.excerpt && <p className="mt-1 text-muted-foreground">&ldquo;{p.excerpt}&rdquo;</p>}
            </li>
          ))}
        </ul>
      )}
    </Card>
  );
}
