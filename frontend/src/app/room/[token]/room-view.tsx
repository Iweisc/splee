"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { ArrowRight, CalendarDays, Check, CircleAlert, FileText, ListChecks, MessageCircleQuestion } from "lucide-react";
import { Button, buttonClass } from "@/components/ui/button";
import { Field, Input, Textarea } from "@/components/ui/form";
import { Favicon, Spinner } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import type { PublicDealRoom } from "@/lib/types";
import { cn } from "@/lib/utils";
import { Frame } from "../../r/[token]/referral-page-view";

const dueLabel = (date: string) => new Date(`${date}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" });

/** The buyer's deal room: the welcome, where the plan and proposal stand, stories from teams like theirs, and questions.
 * `preview` is the team looking at it, which doesn't count as a view. */
export function RoomView({ token, preview }: { token: string; preview: boolean }) {
  const path = `/room-pages/${encodeURIComponent(token)}`;
  // Polled, so an answer published while they wait shows up without a reload.
  const { data: room, error } = useSWR<PublicDealRoom>(path, fetcher, { revalidateOnFocus: false, refreshInterval: 60000 });
  // Reported from the browser, once per load, so link checkers that never run scripts don't count as the buyer.
  const reported = useRef(false);
  useEffect(() => {
    if (!room || reported.current || preview) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [room, path, preview]);

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
  if (!room) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }

  const sender = room.sender_name.trim();
  const them = room.company.trim() || room.first_name || "You";
  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {room.from_domain && <Favicon domain={room.from_domain} size={20} />}
          {room.from_name}
        </span>
        <span aria-hidden>×</span>
        <span className="flex items-center gap-2 font-medium text-foreground">
          {room.company_domain && <Favicon domain={room.company_domain} size={20} />}
          {them}
        </span>
      </header>

      <h1 className="mt-8 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">{room.room.headline}</h1>
      <p className="mt-4 max-w-prose text-lg leading-relaxed text-pretty text-muted-foreground">{room.room.intro}</p>
      {sender && <p className="mt-3 text-sm text-muted-foreground">{sender}</p>}

      {(room.plan || room.proposal) && (
        <div className="mt-8 grid gap-3 sm:grid-cols-2">
          {room.plan && <PlanCard plan={room.plan} them={them} />}
          {room.proposal && <ProposalCard proposal={room.proposal} />}
        </div>
      )}

      {room.proof.length > 0 && (
        <section className="mt-10">
          <h2 className="font-semibold">Teams like yours</h2>
          <ul className="mt-3 flex flex-col gap-3">
            {room.proof.map((p) => (
              <li key={p.material_id} className="rounded-2xl border border-border bg-card p-5">
                <p className="font-medium text-balance">{p.title}</p>
                <p className="mt-1 flex items-start gap-1.5 text-sm text-pretty text-brand-800">
                  <Check className="mt-0.5 size-3.5 shrink-0" /> {p.why}
                </p>
                <details className="group mt-2">
                  <summary className="cursor-pointer text-sm text-muted-foreground hover:text-foreground">Read the story</summary>
                  <p className="mt-2 text-sm leading-relaxed whitespace-pre-line text-pretty">{p.content}</p>
                </details>
              </li>
            ))}
          </ul>
        </section>
      )}

      <Questions room={room} path={path} sender={sender} preview={preview} />

      {room.booking_url && (
        <section className="mt-10 flex flex-col items-start gap-4 rounded-2xl bg-primary p-6 text-primary-foreground sm:flex-row sm:items-center sm:justify-between">
          <div className="min-w-0">
            <p className="font-semibold text-balance">Rather talk it through?</p>
            <p className="mt-1 text-sm text-pretty opacity-75">Pick a time{sender ? ` with ${sender}` : ""}.</p>
          </div>
          <a href={room.booking_url} target="_blank" rel="noreferrer" className={buttonClass("brand", "lg", "shrink-0")}>
            <CalendarDays className="size-4" /> Book a call <ArrowRight className="size-4" />
          </a>
        </section>
      )}
    </Frame>
  );
}

function PlanCard({ plan, them }: { plan: NonNullable<PublicDealRoom["plan"]>; them: string }) {
  const done = plan.steps.filter((s) => s.done_at).length;
  const next = plan.steps.filter((s) => !s.done_at).slice(0, 3);
  return (
    <a href={plan.url} className="group flex flex-col rounded-2xl border border-border bg-card p-5 transition-colors hover:border-foreground/30">
      <p className="flex items-center gap-1.5 text-sm font-medium text-muted-foreground">
        <ListChecks className="size-4" /> The plan
      </p>
      <p className="mt-2 font-semibold text-balance">{plan.goal}</p>
      <div className="mt-3 flex items-center gap-3">
        <div className="h-1.5 flex-1 overflow-hidden rounded-full bg-black/5">
          <div className="h-full rounded-full bg-brand-500" style={{ width: `${plan.steps.length ? (done / plan.steps.length) * 100 : 0}%` }} />
        </div>
        <span className="text-xs text-muted-foreground tabular-nums">
          {done} of {plan.steps.length}
        </span>
      </div>
      {next.length > 0 && (
        <ul className="mt-3 flex flex-col gap-1 text-sm">
          {next.map((s) => (
            <li key={s.id} className="flex items-baseline justify-between gap-3">
              <span className="min-w-0 truncate">
                <span className={cn("mr-1.5 text-xs font-medium", s.side === "them" ? "text-sky-700" : "text-muted-foreground")}>
                  {s.side === "them" ? them : "Us"}
                </span>
                {s.title}
              </span>
              {s.due_date && <span className="shrink-0 text-xs text-muted-foreground tabular-nums">{dueLabel(s.due_date)}</span>}
            </li>
          ))}
        </ul>
      )}
      <span className="mt-auto flex items-center gap-1 pt-4 text-sm font-medium group-hover:underline">
        Open the plan <ArrowRight className="size-3.5" />
      </span>
    </a>
  );
}

function ProposalCard({ proposal }: { proposal: NonNullable<PublicDealRoom["proposal"]> }) {
  const status = proposal.accepted
    ? "You accepted it. Thank you."
    : proposal.held
      ? "We're finishing the pricing, and it'll be ready to accept soon."
      : "The scope and pricing, ready to read and accept.";
  return (
    <a href={proposal.url} className="group flex flex-col rounded-2xl border border-border bg-card p-5 transition-colors hover:border-foreground/30">
      <p className="flex items-center gap-1.5 text-sm font-medium text-muted-foreground">
        <FileText className="size-4" /> The proposal
      </p>
      <p className="mt-2 font-semibold text-balance">{proposal.title}</p>
      <p className="mt-1 text-sm text-pretty text-muted-foreground">{status}</p>
      <span className="mt-auto flex items-center gap-1 pt-4 text-sm font-medium group-hover:underline">
        {proposal.accepted || proposal.held ? "Open it" : "Read and accept"} <ArrowRight className="size-3.5" />
      </span>
    </a>
  );
}

function Questions({ room, path, sender, preview }: { room: PublicDealRoom; path: string; sender: string; preview: boolean }) {
  const [form, setForm] = useState({ name: room.first_name, question: "" });
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState<string | null>(null);
  const [failed, setFailed] = useState("");
  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setSending(true);
    setFailed("");
    try {
      await api(`${path}/questions`, { body: form });
      setSent(form.question.trim());
      setForm({ ...form, question: "" });
    } catch (err) {
      setFailed(err instanceof Error ? err.message : "Something went wrong");
    } finally {
      setSending(false);
    }
  };
  return (
    <section className="mt-10">
      <h2 className="flex items-center gap-2 font-semibold">
        <MessageCircleQuestion className="size-4" /> Questions
      </h2>
      {room.questions.length > 0 && (
        <dl className="mt-3 flex flex-col divide-y divide-border rounded-2xl border border-border bg-card">
          {room.questions.map((q, i) => (
            <div key={i} className="p-5">
              <dt className="font-medium text-pretty">{q.question}</dt>
              <dd className="mt-1.5 text-sm leading-relaxed whitespace-pre-line text-pretty text-muted-foreground">{q.answer}</dd>
            </div>
          ))}
        </dl>
      )}
      {preview && room.questions.length === 0 && (
        <p className="mt-2 text-sm text-muted-foreground">They can ask anything here, and you answer from the deal&apos;s window.</p>
      )}
      {!preview && (
        <form onSubmit={submit} className="mt-3 grid gap-3 rounded-2xl border border-border bg-card p-5 sm:grid-cols-[12rem_minmax(0,1fr)]">
          <Field label="Your name">
            <Input required value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} autoComplete="name" maxLength={120} />
          </Field>
          <Field label="Ask anything">
            <Textarea
              required
              rows={2}
              maxLength={2000}
              value={form.question}
              onChange={(e) => setForm({ ...form, question: e.target.value })}
              placeholder="Security, pricing, rollout, who else uses it…"
              className="min-h-[calc(2lh+1.125rem)] resize-none field-sizing-content"
            />
          </Field>
          <div className="flex flex-wrap items-center justify-end gap-3 sm:col-span-2">
            {sent && !failed && (
              <p className="mr-auto flex items-center gap-1.5 text-sm text-pretty text-brand-800">
                <Check className="size-4 shrink-0" /> Sent. {sender ? `${sender} will` : "We'll"} answer it here.
              </p>
            )}
            {failed && <p className="mr-auto text-sm text-danger">{failed}</p>}
            <Button type="submit" loading={sending} disabled={!form.question.trim() || !form.name.trim()}>
              Ask
            </Button>
          </div>
        </form>
      )}
    </section>
  );
}
