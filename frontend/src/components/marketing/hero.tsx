"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import useSWR from "swr";
import { ArrowRight, Code } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Field, Textarea } from "@/components/ui/form";
import { Modal } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import { cn, isDomain, normalizeDomain, num } from "@/lib/utils";
import { ConvergingLines } from "./converging-lines";

interface PublicStats {
  hot_leads: number;
  companies: number;
}

export function Hero() {
  return (
    <section className="relative overflow-hidden md:min-h-[calc(100dvh-80px-180px)]">
      <div className="relative z-10 flex w-full flex-col items-center px-5 pt-10 text-center md:pt-24 md:pb-16">
        <StatPill />
        <h1 className="mx-auto mb-3 max-w-[760px] text-[clamp(1.875rem,9.5vw,2.875rem)] leading-none font-normal tracking-[-0.03em] md:text-[3.75rem]">
          Get <span className="text-brand-600">1 to 3 hot leads</span>
          <br />
          in 24 hours free
        </h1>
        <div className="mb-10 flex flex-wrap items-center justify-center gap-2">
          <span className="inline-flex items-center rounded-full bg-accent px-4 py-1.5 text-sm">No SDRs or email infra needed</span>
          <span className="inline-flex items-center gap-2 rounded-full bg-accent px-4 py-1.5 text-sm">
            <Code className="size-4" />
            Free and open source
          </span>
        </div>
        <WebsiteForm />
        <p className="mx-auto mt-8 max-w-[560px] text-lg text-muted-foreground md:mt-10">
          Our AI agent researches your market, finds the companies and people likely to buy, writes each one a personal email, and answers
          replies with your booking link
        </p>
      </div>
    </section>
  );
}

function StatPill() {
  const { data } = useSWR<PublicStats>("/public/stats", fetcher);
  const show = !!data && data.hot_leads > 0 && data.companies > 0;
  return (
    <div className="mb-6 grid min-h-9 place-items-center">
      {show && (
        <div className="inline-flex animate-fade-up items-center gap-2 rounded-full bg-card px-4 py-2 text-sm whitespace-nowrap shadow-plaque">
          <span className="font-semibold tabular-nums">{num(data.hot_leads)}</span>
          <span>
            {data.hot_leads === 1 ? "hot lead" : "hot leads"} for {num(data.companies)} {data.companies === 1 ? "company" : "companies"}
          </span>
        </div>
      )}
    </div>
  );
}

function WebsiteForm() {
  const router = useRouter();
  const [value, setValue] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [noSiteOpen, setNoSiteOpen] = useState(false);
  const valid = isDomain(value);

  const submit = (e: React.FormEvent) => {
    e.preventDefault();
    if (!valid) {
      setError("Enter a website like acme.com");
      return;
    }
    router.push(`/auto-gtm/company/${normalizeDomain(value)}/explore`);
  };

  return (
    <div className="flex w-full flex-col items-center">
      <form onSubmit={submit} className="relative w-full max-w-[480px]">
        <div
          className="pointer-events-none absolute top-1/2 right-full hidden -translate-y-1/2 md:block"
          style={{ width: "calc(50vw - 240px)", height: "calc((50vw - 240px) * 1.25)" }}
        >
          <ConvergingLines direction="right" className="size-full" />
        </div>
        <div
          className={cn(
            "relative flex w-full items-center gap-2 rounded-2xl border border-foreground bg-card py-1 pr-1 pl-5 ring-2 ring-foreground",
            error && "border-danger ring-danger",
          )}
        >
          <input
            type="text"
            autoComplete="off"
            data-1p-ignore
            data-lpignore="true"
            aria-label="Your website"
            placeholder="Your website, e.g. consolto.com"
            value={value}
            onChange={(e) => {
              setValue(e.target.value);
              setError(null);
            }}
            className="min-w-0 flex-1 bg-transparent py-3 text-left outline-none placeholder:text-muted-foreground"
          />
          <button
            type="submit"
            aria-label="Find my leads"
            className={cn(
              "flex shrink-0 items-center justify-center rounded-lg px-3.5 py-3 transition-all duration-300",
              valid ? "bg-primary text-primary-foreground hover:scale-105" : "bg-accent text-muted-foreground",
            )}
          >
            <ArrowRight className="size-[18px]" strokeWidth={2.5} />
          </button>
        </div>
        <div
          className="pointer-events-none absolute top-1/2 left-full hidden -translate-y-1/2 md:block"
          style={{ width: "calc(50vw - 240px)", height: "calc((50vw - 240px) * 1.25)" }}
        >
          <ConvergingLines direction="left" className="size-full" />
        </div>
      </form>
      {error && <p className="mt-2 text-sm text-danger">{error}</p>}
      <button
        type="button"
        onClick={() => setNoSiteOpen(true)}
        className="mt-4 text-sm text-muted-foreground transition-colors duration-300 hover:text-foreground"
      >
        I don&apos;t have a website
      </button>
      <NoWebsiteModal open={noSiteOpen} onClose={() => setNoSiteOpen(false)} />
    </div>
  );
}

function NoWebsiteModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const router = useRouter();
  const [description, setDescription] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (description.trim().length < 20) {
      setError("Add a bit more detail: what you sell and who buys it.");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const session = await api<{ project_id: string; token: string }>("/demo", { body: { description: description.trim() } });
      const slug = `p-${session.project_id}`;
      sessionStorage.setItem(`splee-demo:${slug}`, JSON.stringify(session));
      router.push(`/auto-gtm/company/${slug}/explore?token=${encodeURIComponent(session.token)}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : "Something went wrong. Try again.");
      setLoading(false);
    }
  };

  return (
    <Modal open={open} onClose={onClose} title="Describe your business">
      <form onSubmit={submit} className="flex flex-col gap-4 text-left">
        <Field label="What do you sell, and to whom?" error={error} hint="The agent uses this instead of a website.">
          <Textarea
            value={description}
            onChange={(e) => setDescription(e.target.value)}
            rows={5}
            placeholder="We make handmade ceramic tableware and sell it wholesale to restaurants and boutique hotels in the UK."
          />
        </Field>
        <Button type="submit" loading={loading} className="self-end">
          Find my leads
          <ArrowRight className="size-4" />
        </Button>
      </form>
    </Modal>
  );
}
