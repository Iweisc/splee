"use client";

import { useEffect, useRef, useState } from "react";
import useSWR from "swr";
import { BadgeCheck, CircleAlert } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/form";
import { Favicon, Spinner } from "@/components/ui/misc";
import { LogoMark } from "@/components/ui/logo";
import { fetcher } from "@/hooks/use-me";
import { ApiError, api } from "@/lib/api";
import { PERIODS, firstYear, monthly } from "@/lib/proposals";
import type { PublicProposal } from "@/lib/types";
import { dealMoney } from "@/lib/utils";

const longDay = (date: string) =>
  new Date(date.length === 10 ? `${date}T00:00:00` : date).toLocaleDateString("en-US", { month: "long", day: "numeric", year: "numeric" });

/** Whether the whole day has passed, wherever the buyer is. */
const pastDay = (date: string) => new Date(`${date}T23:59:59`).getTime() < Date.now();

/** The buyer's proposal: what they get, the prices, and accepting it with their name. */
export function ProposalView({ token, preview }: { token: string; preview: boolean }) {
  const path = `/proposals/${encodeURIComponent(token)}`;
  const { data: p, error, mutate } = useSWR<PublicProposal>(path, fetcher, { revalidateOnFocus: false });
  const [signer, setSigner] = useState({ name: "", title: "" });
  const [accepting, setAccepting] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  // Reported from the browser, once per load, so link checkers that never run scripts don't count as the buyer.
  const reported = useRef(false);
  useEffect(() => {
    if (!p || reported.current || preview) return;
    reported.current = true;
    api(`${path}/view`, { method: "POST" }).catch(() => {});
  }, [p, path, preview]);

  if (error instanceof ApiError && error.status === 404) {
    return (
      <Frame>
        <div className="flex min-h-64 flex-col items-center justify-center gap-2 text-center text-balance">
          <CircleAlert className="size-6 text-danger" />
          <h1 className="text-xl font-medium">This proposal isn&apos;t here anymore</h1>
          <p className="text-sm text-muted-foreground">Reply to the email it came in and we&apos;ll send the latest.</p>
        </div>
      </Frame>
    );
  }
  if (!p) {
    return (
      <Frame>
        <div className="flex min-h-64 items-center justify-center">
          {error ? <p className="text-sm text-muted-foreground">{error.message}</p> : <Spinner />}
        </div>
      </Frame>
    );
  }

  const accept = async (e: React.FormEvent) => {
    e.preventDefault();
    setAccepting(true);
    setFailed(null);
    try {
      await mutate(api<PublicProposal>(`${path}/accept`, { body: signer }), { revalidate: false });
    } catch (err) {
      setFailed(err instanceof Error ? err.message : "That didn't go through. Try again in a minute.");
    } finally {
      setAccepting(false);
    }
  };

  const money = (n: number) => dealMoney(n, p.currency);
  const sender = p.sender_name.trim();
  const them = p.company.trim() || p.first_name;
  const expired = !!p.valid_until && pastDay(p.valid_until);
  const perMonth = monthly(p.items);
  return (
    <Frame>
      <header className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm text-muted-foreground">
        <span className="flex items-center gap-2 font-medium text-foreground">
          {p.from_domain && <Favicon domain={p.from_domain} size={20} />}
          {p.from_name}
        </span>
        {them && (
          <>
            <span aria-hidden>×</span>
            <span className="flex items-center gap-2 font-medium text-foreground">
              {p.company_domain && <Favicon domain={p.company_domain} size={20} />}
              {them}
            </span>
          </>
        )}
      </header>

      <p className="mt-8 font-mono text-xs tracking-wide text-muted-foreground uppercase">Proposal</p>
      <h1 className="mt-2 text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">{p.title}</h1>
      {p.intro && <p className="mt-4 max-w-prose text-lg leading-relaxed text-pretty text-muted-foreground">{p.intro}</p>}

      {p.sections.length > 0 && (
        <div className="mt-10 flex flex-col gap-8">
          {p.sections.map((s, i) => (
            <section key={i} className="grid gap-2 sm:grid-cols-[10rem_minmax(0,1fr)] sm:gap-6">
              <h2 className="font-semibold text-balance">
                <span className="mr-2 font-mono text-xs font-normal text-muted-foreground tabular-nums">0{i + 1}</span>
                {s.title}
              </h2>
              <p className="leading-relaxed text-pretty whitespace-pre-line text-foreground/80">{s.body}</p>
            </section>
          ))}
        </div>
      )}

      {p.items.length > 0 && (
        <section className="mt-12 overflow-hidden rounded-2xl border border-border bg-card">
          <table className="w-full text-sm">
            <thead>
              <tr className="border-b border-border text-left text-xs text-muted-foreground">
                <th className="px-5 py-3 font-medium">Pricing</th>
                <th className="px-3 py-3 text-right font-medium">Qty</th>
                <th className="px-3 py-3 text-right font-medium">Price</th>
                <th className="px-5 py-3 text-right font-medium">Total</th>
              </tr>
            </thead>
            <tbody>
              {p.items.map((item, i) => (
                <tr key={i} className="border-b border-border last:border-0">
                  <td className="px-5 py-3">
                    <p className="font-medium">{item.name}</p>
                    {item.detail && <p className="text-xs text-muted-foreground">{item.detail}</p>}
                  </td>
                  <td className="px-3 py-3 text-right tabular-nums">{item.quantity}</td>
                  <td className="px-3 py-3 text-right tabular-nums">
                    {money(item.unit_price)}
                    <span className="text-muted-foreground">{PERIODS[item.period].suffix}</span>
                  </td>
                  <td className="px-5 py-3 text-right font-medium tabular-nums">
                    {money(item.quantity * item.unit_price)}
                    <span className="font-normal text-muted-foreground">{PERIODS[item.period].suffix}</span>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
          <div className="flex flex-wrap items-baseline justify-end gap-x-6 gap-y-1 border-t border-border bg-muted/50 px-5 py-4 text-sm">
            {perMonth > 0 && (
              <span className="text-muted-foreground">
                Per month <span className="ml-1 font-medium text-foreground tabular-nums">{money(perMonth)}</span>
              </span>
            )}
            <span className="text-muted-foreground">
              First year <span className="ml-1 text-lg font-semibold text-foreground tabular-nums">{money(firstYear(p.items))}</span>
            </span>
          </div>
        </section>
      )}

      <section className="mt-10 rounded-2xl bg-primary p-6 text-primary-foreground">
        {p.accepted_at ? (
          <div className="flex items-start gap-3">
            <BadgeCheck className="mt-0.5 size-6 shrink-0 text-brand-400" />
            <div>
              <p className="font-semibold">
                Accepted by {p.accepted_name} on {longDay(p.accepted_at)}
              </p>
              <p className="mt-1 text-sm opacity-75">
                {sender ? `${sender} will be in touch about next steps.` : "We'll be in touch about next steps."}
              </p>
            </div>
          </div>
        ) : p.closed || expired ? (
          <p className="text-sm opacity-75">
            {expired ? `This proposal was open until ${longDay(p.valid_until!)}.` : "This proposal is closed."} Reply to the email it came in
            {sender ? ` and ${sender} will send a new one` : " for a new one"}.
          </p>
        ) : (
          <form onSubmit={accept} className="flex flex-col gap-4">
            <div>
              <p className="font-semibold text-balance">Ready to go ahead?</p>
              <p className="mt-1 text-sm text-pretty opacity-75">
                Type your name to accept this proposal{p.valid_until ? ` by ${longDay(p.valid_until)}` : ""}.
                {sender ? ` ${sender} will be in touch about next steps.` : ""}
              </p>
            </div>
            <div className="grid gap-2 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto]">
              <Input
                value={signer.name}
                onChange={(e) => setSigner((s) => ({ ...s, name: e.target.value }))}
                placeholder="Your full name"
                required
                maxLength={120}
                className="bg-white text-foreground"
                aria-label="Your full name"
              />
              <Input
                value={signer.title}
                onChange={(e) => setSigner((s) => ({ ...s, title: e.target.value }))}
                placeholder="Your title"
                maxLength={120}
                className="bg-white text-foreground"
                aria-label="Your title"
              />
              <Button type="submit" variant="brand" loading={accepting} disabled={!signer.name.trim()}>
                Accept proposal
              </Button>
            </div>
            {failed && <p className="text-sm text-red-300">{failed}</p>}
          </form>
        )}
      </section>

      <p className="mt-6 text-xs text-pretty text-muted-foreground">
        Prepared for {p.first_name || "you"}
        {p.company ? ` at ${p.company}` : ""}
        {sender ? ` by ${sender}` : ""} at {p.from_name}.
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
