"use client";

import { useState } from "react";
import Link from "next/link";
import { BookOpenText, Copy, ExternalLink, Eye, Loader2, PenLine } from "lucide-react";
import useSWR from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { fetcher } from "@/hooks/use-me";
import { api } from "@/lib/api";
import type { CustomerStory, Deal } from "@/lib/types";
import { plural, timeAgo } from "@/lib/utils";
import { toastError } from "./states";
import { ThreadDraft } from "./win-back";

const pageUrl = (s: CustomerStory) => `${window.location.origin}/story/${s.token}`;
/** Written and waiting on the customer, whether the email went or the link is to be sent by hand. */
const out = (s: CustomerStory) => s.status === "drafted" || s.status === "written" || s.status === "sent";

function heading(s: CustomerStory | null | undefined, company: string) {
  switch (s?.status) {
    case "drafted":
      return "Case study ready to send";
    case "written":
    case "sent":
      return `Waiting for ${company} to approve their story`;
    case "approved":
      return "Case study approved";
    case "declined":
      return "They'd rather not have a case study";
    default:
      return "Turn this win into a case study";
  }
}

function Status({ s }: { s: CustomerStory }) {
  switch (s.status) {
    case "writing":
      return <p className="text-sm text-muted-foreground">The agent is writing up how it went…</p>;
    case "skipped":
      return <p className="text-sm text-pretty text-muted-foreground">Not written: {s.skipped.replace(/\.$/, "")}.</p>;
    case "failed":
      return <p className="text-sm text-danger">The agent couldn&apos;t finish. Try again.</p>;
    case "approved":
      return (
        <p className="text-sm text-pretty text-muted-foreground">
          {s.approved_name || "They"} approved it {s.approved_at ? timeAgo(s.approved_at) : ""},{" "}
          {s.named ? "with their name on it" : "to share without their name"}. It&apos;s in Materials, so the agent uses it as proof.
        </p>
      );
    case "declined":
      return (
        <p className="text-sm text-pretty text-muted-foreground">
          {s.declined_at ? `They said no thanks ${timeAgo(s.declined_at)}. ` : ""}Nothing was shared.
        </p>
      );
    case "written":
      return (
        <p className="text-sm text-pretty text-muted-foreground">The email didn&apos;t go out. Copy their page&apos;s link to send it yourself.</p>
      );
    default:
      return null;
  }
}

/** In a won deal's window: the case study the agent drafts, the email asking them to approve it, and how it went. */
export function StorySection({ deal }: { deal: Deal }) {
  const {
    data: s,
    error,
    mutate,
  } = useSWR<CustomerStory | null>(deal.status === "won" ? `/deals/${deal.id}/story` : null, fetcher, {
    refreshInterval: (s) => (s?.status === "writing" ? 2500 : 0),
  });
  const [busy, setBusy] = useState<string | null>(null);
  if (deal.status !== "won" || error || s === undefined) return null;

  const act = async (what: string, call: () => Promise<unknown>, done?: string) => {
    setBusy(what);
    try {
      await call();
      if (done) toast.success(done);
      await mutate();
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };
  const write = () => act("write", () => api(`/deals/${deal.id}/story`, { method: "POST" }));
  const copy = (s: CustomerStory) =>
    navigator.clipboard.writeText(pageUrl(s)).then(
      () => toast.success("Copied their page's link"),
      () => toast.error("Couldn't copy the link"),
    );
  const company = s?.company_name || s?.lead_name || "them";
  const canWrite = !s || ["skipped", "failed", "declined", "written"].includes(s.status);

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-border p-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <p className="flex items-center gap-1.5 text-sm font-medium">
            <BookOpenText className="size-3.5" /> {heading(s, company)}
          </p>
          <div className="mt-1 flex flex-col gap-1">
            {s ? (
              <Status s={s} />
            ) : (
              <p className="text-sm text-pretty text-muted-foreground">
                The agent writes up how it went from the conversation and drafts an email asking them to approve it. On their page they fix the quote,
                add their numbers and choose whether their name goes on it. Once approved, it goes into Materials.
              </p>
            )}
            {s && s.views > 0 && out(s) && (
              <p className="flex items-center gap-1 text-xs text-muted-foreground">
                <Eye className="size-3.5" /> Page opened {plural(s.views, "time")}
                {s.last_viewed_at && `, last ${timeAgo(s.last_viewed_at)}`}
              </p>
            )}
          </div>
        </div>
        {s?.status === "writing" ? (
          <Loader2 className="size-4 shrink-0 animate-spin text-muted-foreground" />
        ) : (
          canWrite && (
            <Button type="button" size="sm" variant="outline" onClick={write} loading={busy === "write"}>
              <PenLine className="size-3.5" /> {s ? "Write again" : "Write it now"}
            </Button>
          )
        )}
      </div>
      {s?.story && out(s) && (
        <div className="rounded-lg bg-muted/60 p-3">
          <p className="text-sm font-medium text-balance">{s.story.title}</p>
          <p className="mt-1 text-sm text-pretty text-muted-foreground">{s.story.summary}</p>
          {s.story.quote && <p className="mt-2 text-sm text-pretty italic">&ldquo;{s.story.quote}&rdquo;</p>}
        </div>
      )}
      {s?.status === "drafted" && (
        <ThreadDraft
          subject={s.email_subject}
          body={s.email_body}
          projectId={deal.project_id}
          onAnother={write}
          onSend={() =>
            act(
              "send",
              () => api(`/emails/${s.email_id}`, { method: "PATCH", body: { status: "approved" } }),
              "Approved. It goes out in the thread shortly.",
            )
          }
          busy={busy}
        />
      )}
      {s && (out(s) || s.status === "approved") && (
        <div className="flex flex-wrap items-center gap-2">
          {s.status === "approved" ? (
            <Link href={`/app/p/${deal.project_id}/materials`} className="inline-flex items-center gap-1 text-sm text-foreground hover:underline">
              <ExternalLink className="size-3.5" /> Open Materials
            </Link>
          ) : (
            <>
              <Button type="button" size="sm" variant="ghost" onClick={() => copy(s)}>
                <Copy className="size-3.5" /> Copy their page&apos;s link
              </Button>
              <a
                href={`/story/${s.token}?preview`}
                target="_blank"
                rel="noreferrer"
                className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground hover:underline"
              >
                <ExternalLink className="size-3.5" /> See their page
              </a>
            </>
          )}
        </div>
      )}
    </section>
  );
}
