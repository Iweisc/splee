"use client";

import { useEffect, useMemo, useState } from "react";
import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import useSWR from "swr";
import { CircleAlert, RotateCcw } from "lucide-react";
import { SupportChat } from "@/components/support-chat";
import { Button } from "@/components/ui/button";
import { Field, Textarea } from "@/components/ui/form";
import { Logo } from "@/components/ui/logo";
import { Card, Modal } from "@/components/ui/misc";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { fetcher, useMe } from "@/hooks/use-me";
import { api, ApiError, qs } from "@/lib/api";
import type { AgentEvent, DemoState } from "@/lib/types";
import { isDomain, normalizeDomain } from "@/lib/utils";
import { Sidebar } from "./sidebar";
import { StepDetail } from "./step-detail";
import { StartBanner, Stepper } from "./stepper";
import { STEP_COUNT, STEPS } from "./steps";

interface Session {
  project_id: string;
  token: string;
}

const BANNER_FROM_STEP = 4;

const storageKey = (slug: string) => `splee-demo:${slug}`;

function readStored(slug: string): Session | null {
  try {
    const raw = sessionStorage.getItem(storageKey(slug));
    return raw ? (JSON.parse(raw) as Session) : null;
  } catch {
    return null;
  }
}

async function startSession(slug: string, urlToken: string | null): Promise<Session> {
  if (slug.startsWith("p-")) {
    const token = urlToken ?? readStored(slug)?.token;
    if (!token) throw new Error("This demo link is missing its access token. Start a new demo from the home page.");
    return { project_id: slug.slice(2), token };
  }
  const stored = readStored(slug);
  if (stored) return stored;
  if (!isDomain(slug)) throw new Error(`"${slug}" doesn't look like a website. Try something like acme.com.`);
  return api<Session>("/demo", { body: { domain: normalizeDomain(slug) } });
}

function describeError(e: unknown): string {
  if (e instanceof ApiError && e.status === 429) return "You've started a lot of demos from this network. Wait a little and try again.";
  if (e instanceof ApiError && e.status >= 500) return "The agent service isn't responding right now.";
  return e instanceof Error ? e.message : "Something went wrong.";
}

export function DemoPage({ slug, urlToken }: { slug: string; urlToken: string | null }) {
  const router = useRouter();
  const pathname = usePathname();
  const { me } = useMe();
  const [session, setSession] = useState<Session | null>(null);
  const [startError, setStartError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [selected, setSelected] = useState<number | null>(null);
  const [correcting, setCorrecting] = useState(false);
  const [claiming, setClaiming] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    startSession(slug, urlToken).then(
      (s) => {
        if (cancelled) return;
        sessionStorage.setItem(storageKey(slug), JSON.stringify(s));
        setSession(s);
      },
      (e) => !cancelled && setStartError(describeError(e)),
    );
    return () => {
      cancelled = true;
    };
  }, [slug, urlToken, attempt]);

  const tokenQs = session ? qs({ token: session.token }) : "";
  const { data: state, error: stateError, mutate } = useSWR<DemoState>(session ? `/demo/${session.project_id}${tokenQs}` : null, fetcher);
  const run = state?.run ?? null;

  // The run id is in the URL so the stream reconnects when "Details look off?" starts a new run.
  const streamUrl = session && run ? `/api/demo/${session.project_id}/events${qs({ token: session.token, run: run.id })}` : null;
  const { events: live } = useAgentEvents(streamUrl, (e) => {
    if (e.kind === "phase" || e.kind === "done" || e.kind === "error") mutate();
  });

  const events = useMemo(() => {
    const byId = new Map<number, AgentEvent>();
    for (const e of [...(state?.events ?? []), ...live]) if (!run || e.run_id === run.id) byId.set(e.id, e);
    return [...byId.values()].sort((a, b) => a.id - b.id);
  }, [state?.events, live, run]);

  const succeeded = run?.status === "succeeded" || events.some((e) => e.kind === "done");
  const errorEvent = events.findLast((e) => e.kind === "error");
  const failure =
    !succeeded && (run?.status === "failed" || run?.status === "cancelled" || errorEvent)
      ? run?.error || errorEvent?.message || "The run didn't finish."
      : null;

  const finished = new Set<number>();
  if (succeeded) STEPS.forEach((s) => finished.add(s.n));
  for (let n = 1; n < (run?.step ?? 1); n++) finished.add(n);
  for (const e of events) if (e.kind === "phase" && e.data.status === "finished") finished.add(Number(e.data.step ?? e.step));

  const current = STEPS.find((s) => !finished.has(s.n))?.n ?? STEP_COUNT;
  const working = !!state && !succeeded && !failure;
  const viewed = selected !== null && finished.has(selected) ? selected : current;
  const viewedLoading = viewed === current && !finished.has(current);
  const logs = events.filter((e) => (e.kind === "log" || e.kind === "tool") && e.step === current && e.message);
  const activeSegmentId = state?.companies[0]?.segment_id ?? state?.segments[0]?.id ?? null;

  const phase = `${STEPS[viewed - 1]!.phase}${viewedLoading && working ? "_loading" : ""}`;
  useEffect(() => {
    if (!state) return;
    const params = new URLSearchParams(window.location.search);
    if (params.get("phase") === phase) return;
    params.set("phase", phase);
    router.replace(`${pathname}?${params}`, { scroll: false });
  }, [state, phase, pathname, router]);

  const rerun = async (note: string) => {
    if (!session) return;
    await api(`/demo/${session.project_id}/correct${tokenQs}`, { body: { note } });
    setSelected(null);
    await mutate();
  };

  const startOutreach = async () => {
    if (!session) return;
    if (!me) {
      router.push(`/signup${qs({ claim: session.project_id, token: session.token })}`);
      return;
    }
    setClaiming(true);
    setActionError(null);
    try {
      await api("/projects/claim", { body: { project_id: session.project_id, token: session.token } });
      router.push(`/app/p/${session.project_id}`);
    } catch (e) {
      setActionError(describeError(e));
      setClaiming(false);
    }
  };

  const restart = () => {
    if (!slug.startsWith("p-")) sessionStorage.removeItem(storageKey(slug));
    setStartError(null);
    setSession(null);
    setAttempt((a) => a + 1);
  };

  if (startError || (stateError && !state)) {
    return (
      <div className="flex min-h-dvh flex-col">
        <div className="px-5 py-4">
          <Logo />
        </div>
        <div className="flex flex-1 items-center justify-center px-5 pb-24">
          <Card className="flex w-full max-w-md flex-col items-center gap-3 p-8 text-center">
            <CircleAlert className="size-6 text-danger" />
            <p className="font-medium">We couldn&apos;t start the demo</p>
            <p className="text-sm text-muted-foreground">{startError ?? describeError(stateError)}</p>
            <div className="mt-3 flex gap-2">
              <Button onClick={restart}>
                <RotateCcw className="size-4" />
                Try again
              </Button>
              <Link href="/" className="inline-flex h-10 items-center px-4 text-sm text-muted-foreground hover:text-foreground">
                Back to home
              </Link>
            </div>
          </Card>
        </div>
        <SupportChat />
      </div>
    );
  }

  return (
    <div className="flex min-h-dvh flex-col md:h-dvh md:flex-row md:overflow-hidden">
      <Sidebar
        state={state}
        finished={finished}
        current={current}
        working={working || !state}
        logs={logs}
        activeSegmentId={activeSegmentId}
        onCorrect={() => setCorrecting(true)}
      />
      <main className="flex min-w-0 flex-1 flex-col md:overflow-y-auto">
        {(current >= BANNER_FROM_STEP || succeeded) && <StartBanner onStart={startOutreach} loading={claiming} />}
        {actionError && <p className="border-b border-border bg-red-50 px-5 py-2 text-sm text-red-700">{actionError}</p>}
        <Stepper
          finished={finished}
          viewed={viewed}
          running={viewedLoading && (working || !state)}
          failed={viewedLoading && !!failure}
          onSelect={(n) => setSelected(n === current ? null : n)}
        />
        <div className="flex-1 px-5 pb-24 md:px-9">
          {viewedLoading && failure ? (
            <Card className="mx-auto mt-2 flex w-full max-w-md flex-col items-center gap-3 p-8 text-center">
              <CircleAlert className="size-6 text-danger" />
              <p className="font-medium">The agent stopped on step {current}</p>
              <p className="text-sm text-muted-foreground">{failure}</p>
              <Button className="mt-3" onClick={() => rerun("").catch((e) => setActionError(describeError(e)))}>
                <RotateCcw className="size-4" />
                Try again
              </Button>
            </Card>
          ) : (
            <StepDetail step={viewed} state={state} loading={viewedLoading} activeSegmentId={activeSegmentId} onCorrect={() => setCorrecting(true)} />
          )}
        </div>
      </main>
      <CorrectionModal open={correcting} onClose={() => setCorrecting(false)} onSubmit={rerun} />
      <SupportChat />
    </div>
  );
}

function CorrectionModal({ open, onClose, onSubmit }: { open: boolean; onClose: () => void; onSubmit: (note: string) => Promise<void> }) {
  const [note, setNote] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!note.trim()) return;
    setSaving(true);
    setError(null);
    try {
      await onSubmit(note.trim());
      setNote("");
      onClose();
    } catch (err) {
      setError(describeError(err));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal open={open} onClose={onClose} title="What did the agent get wrong?">
      <form onSubmit={submit} className="flex flex-col gap-4">
        <Field label="Correction" hint="The agent starts over from step 1 with your note." error={error}>
          <Textarea
            value={note}
            onChange={(e) => setNote(e.target.value)}
            rows={4}
            placeholder="We sell to restaurants, not hotels. We're based in Leeds and have about 40 people."
          />
        </Field>
        <Button type="submit" loading={saving} disabled={!note.trim()} className="self-end">
          Re-run research
        </Button>
      </form>
    </Modal>
  );
}
