"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { ArrowRight, CalendarDays, Check, CircleAlert } from "lucide-react";
import { Button, buttonClass } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Favicon, Spinner } from "@/components/ui/misc";
import { LogoMark } from "@/components/ui/logo";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { PublicReferralPage } from "@/lib/types";

const EMPTY = { name: "", email: "", company: "", note: "" };

/** The page a customer forwards: who sent it, what we do, and a way to leave your details or book a call. */
export function ReferralPageView({ token }: { token: string }) {
  const path = `/referral-pages/${encodeURIComponent(token)}`;
  const { data: page, error } = useSWR<PublicReferralPage>(path, fetcher, { revalidateOnFocus: false });
  const [form, setForm] = useState(EMPTY);
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const [failed, setFailed] = useState("");
  // Reported from the browser, once per load, so link checkers that never run scripts don't count.
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
          <p className="text-sm text-muted-foreground">Ask whoever sent it for a new link.</p>
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
  const from = page.company.trim() ? `${page.first_name || page.full_name} at ${page.company}` : page.full_name;
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSending(true);
    setFailed("");
    try {
      await api(`${path}/signup`, { body: form });
      setSent(true);
    } catch (err) {
      setFailed(err instanceof Error ? err.message : "Something went wrong");
    } finally {
      setSending(false);
    }
  };

  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {page.from_domain && <Favicon domain={page.from_domain} size={20} />}
          {page.from_name}
        </span>
        <span aria-hidden>·</span>
        <span className="flex items-center gap-2">
          {page.company_domain && <Favicon domain={page.company_domain} size={16} />}
          Shared by {from}
        </span>
      </header>

      <h1 className="mt-8 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">
        {page.first_name || page.full_name} thought you should meet {page.from_name}
      </h1>
      {page.description && <p className="mt-4 max-w-prose text-lg leading-relaxed text-pretty text-muted-foreground">{page.description}</p>}

      {page.value_props.length > 0 && (
        <ul className="mt-8 grid gap-3 sm:grid-cols-3">
          {page.value_props.slice(0, 3).map((v, i) => (
            <li key={i} className="flex gap-2.5 rounded-2xl border border-border bg-card p-5 text-sm leading-relaxed text-pretty">
              <Check className="mt-0.5 size-4 shrink-0 text-brand-600" aria-hidden />
              {v}
            </li>
          ))}
        </ul>
      )}

      <section className="mt-10 rounded-2xl border border-border bg-card p-6">
        {sent ? (
          <div className="flex flex-col items-start gap-2">
            <p className="flex items-center gap-2 font-semibold">
              <Check className="size-4 text-brand-600" /> Thanks, {form.name.trim().split(" ")[0]}
            </p>
            <p className="text-sm text-pretty text-muted-foreground">
              {sender || page.from_name} will write to you at {form.email.trim()} soon, and {page.first_name || "they"} will know you came by.
            </p>
          </div>
        ) : (
          <>
            <h2 className="font-semibold text-balance">Leave your details and {sender ? `${sender.split(" ")[0]} will` : "we'll"} get in touch</h2>
            <form onSubmit={submit} className="mt-4 grid gap-4 sm:grid-cols-2">
              <Field label="Your name">
                <Input required value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} autoComplete="name" />
              </Field>
              <Field label="Work email">
                <Input required type="email" value={form.email} onChange={(e) => setForm({ ...form, email: e.target.value })} autoComplete="email" />
              </Field>
              <Field label="Company" className="sm:col-span-2">
                <Input value={form.company} onChange={(e) => setForm({ ...form, company: e.target.value })} autoComplete="organization" />
              </Field>
              <Field label="What you're looking for (optional)" className="sm:col-span-2">
                <Textarea rows={3} value={form.note} onChange={(e) => setForm({ ...form, note: e.target.value })} />
              </Field>
              {failed && <p className="text-sm text-danger sm:col-span-2">{failed}</p>}
              <div className="flex flex-wrap items-center justify-end gap-3 sm:col-span-2">
                {page.booking_url && (
                  <a href={page.booking_url} target="_blank" rel="noreferrer" className={buttonClass("outline", "md")}>
                    <CalendarDays className="size-4" /> Or book a call
                  </a>
                )}
                <Button type="submit" loading={sending}>
                  Send <ArrowRight className="size-4" />
                </Button>
              </div>
            </form>
          </>
        )}
      </section>
    </Frame>
  );
}

/** The plain page around what a customer or someone they forwarded it to sees. */
export function Frame({ children }: { children: React.ReactNode }) {
  return (
    <main className="flex flex-1 flex-col items-center bg-muted px-4 py-10 sm:px-6 sm:py-16">
      <article className="w-full max-w-3xl">{children}</article>
      <p className="mt-12 flex items-center gap-1.5 text-xs text-muted-foreground">
        <LogoMark className="size-3.5" /> Made with Splee
      </p>
    </main>
  );
}
