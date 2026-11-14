"use client";

import { useEffect, useRef } from "react";
import useSWR from "swr";
import { ArrowRight, CalendarDays, CircleAlert, Mail, Quote } from "lucide-react";
import { buttonClass } from "@/components/ui/button";
import { Favicon, Spinner } from "@/components/ui/misc";
import { LogoMark } from "@/components/ui/logo";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { PublicLeadPage } from "@/lib/types";

/** A lead's personal page: what their email started, laid out to read in a minute, ending in a way to talk. */
export function LeadPageView({ token }: { token: string }) {
  const path = `/pages/${encodeURIComponent(token)}`;
  const { data: page, error } = useSWR<PublicLeadPage>(path, fetcher, {
    revalidateOnFocus: false,
  });
  // Reported from the browser, once per load, so link checkers that never run scripts don't count as the lead.
  const reported = useRef(false);
  useEffect(() => {
    if (!page || reported.current) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [page, path]);

  if (error instanceof ApiError && error.status === 404) {
    return (
      <Frame>
        <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This page isn&apos;t here anymore</h1>
          <p className="text-sm text-muted-foreground">Reply to the email it came in and we&apos;ll send what you need.</p>
        </div>
      </Frame>
    );
  }
  if (!page) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }

  const sender = page.sender_name.trim();
  const them = page.company.trim() || page.first_name;
  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {page.from_domain && <Favicon domain={page.from_domain} size={20} />}
          {page.from_name}
        </span>
        {them && (
          <>
            <span aria-hidden>×</span>
            <span className="flex items-center gap-2 font-medium text-foreground">
              {page.company_domain && <Favicon domain={page.company_domain} size={20} />}
              {them}
            </span>
          </>
        )}
      </header>

      <h1 className="mt-8 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">{page.headline}</h1>
      {page.intro && <p className="mt-4 max-w-prose text-lg leading-relaxed text-pretty text-muted-foreground">{page.intro}</p>}

      {page.points.length > 0 && (
        <ol className="mt-10 grid gap-3 sm:grid-cols-3">
          {page.points.map((p, i) => (
            <li key={i} className="rounded-2xl border border-border bg-card p-5">
              <span className="font-mono text-xs text-muted-foreground tabular-nums">0{i + 1}</span>
              <h2 className="mt-2 font-semibold text-balance">{p.title}</h2>
              <p className="mt-1.5 text-sm leading-relaxed text-pretty text-muted-foreground">{p.body}</p>
            </li>
          ))}
        </ol>
      )}

      {page.proof && (
        <blockquote className="mt-10 flex gap-3 border-l-2 border-brand-500 pl-4 text-pretty">
          <Quote className="mt-1 size-4 shrink-0 text-brand-600" aria-hidden />
          <p className="leading-relaxed">{page.proof}</p>
        </blockquote>
      )}

      <section className="mt-12 flex flex-col items-start gap-4 rounded-2xl bg-primary p-6 text-primary-foreground sm:flex-row sm:items-center sm:justify-between">
        <div className="min-w-0">
          <p className="font-semibold text-balance">
            {page.booking_url ? "Want to walk through it together?" : "Questions, or want to see it work?"}
          </p>
          <p className="mt-1 text-sm text-pretty opacity-75">
            {page.booking_url
              ? `Pick a time that suits you${sender ? ` and ${sender} will take you through it` : ""}.`
              : `Reply to the email${sender ? ` and ${sender} will get back to you` : ""}.`}
          </p>
        </div>
        {page.booking_url ? (
          <a href={page.booking_url} target="_blank" rel="noreferrer" className={buttonClass("brand", "lg", "shrink-0")}>
            <CalendarDays className="size-4" /> Book a call <ArrowRight className="size-4" />
          </a>
        ) : (
          page.reply_to && (
            <a href={`mailto:${page.reply_to}`} className={buttonClass("brand", "lg", "shrink-0")}>
              <Mail className="size-4" /> Write back
            </a>
          )
        )}
      </section>

      <p className="mt-6 text-xs text-pretty text-muted-foreground">
        Written for {page.first_name || "you"}
        {page.company ? ` at ${page.company}` : ""}
        {sender ? ` by ${sender}` : ""} at {page.from_name}.
      </p>
    </Frame>
  );
}

function Frame({ children }: { children: React.ReactNode }) {
  return (
    <main className="flex flex-1 flex-col items-center bg-muted px-4 py-10 sm:px-6 sm:py-16">
      <article className="w-full max-w-3xl">{children}</article>
      <p className="mt-12 flex items-center gap-1.5 text-xs text-muted-foreground">
        <LogoMark className="size-3.5" /> Made with Splee
      </p>
    </main>
  );
}
