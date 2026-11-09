"use client";

import { useState } from "react";
import { Check, RefreshCw, Send, Sparkles, X } from "lucide-react";
import { useSWRConfig } from "swr";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Input, Textarea } from "@/components/ui/form";
import { Address, Message, Spinner } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { EmailRow } from "@/lib/types";
import { dateTime } from "@/lib/utils";
import { useAgentEvents } from "@/hooks/use-agent-events";
import { MicroLabel, toastError } from "./states";
import { EmailStatusBadge } from "./status";

const KINDS: Record<Exclude<EmailRow["kind"], "follow_up">, string> = {
  initial: "First email",
  auto_reply: "Auto-reply",
  manual_reply: "Manual reply",
  inbound: "Lead's reply",
};

/** Read-only preview for sent/scheduled emails, inline editor for drafts. Remount it (key) when the email changes. */
export function EmailPreview({ email, showProject }: { email: EmailRow; showProject?: boolean }) {
  const { mutate } = useSWRConfig();
  const [subject, setSubject] = useState(email.subject);
  const [body, setBody] = useState(email.body);
  const [busy, setBusy] = useState<"save" | "approve" | "cancel" | "regenerate" | "test" | null>(null);
  const [instructions, setInstructions] = useState<string | null>(null);
  const [rewriteRun, setRewriteRun] = useState<string | null>(null);

  useAgentEvents(rewriteRun ? `/api/runs/${rewriteRun}/events` : null, (e) => {
    if (e.kind === "done" || e.kind === "error") {
      setRewriteRun(null);
      if (e.kind === "error") toast.error(e.message || "Rewrite failed");
      mutate(() => true);
    }
  });

  const isDraft = email.status === "draft";
  const dirty = subject !== email.subject || body !== email.body;
  const pending = email.status === "approved" || email.status === "scheduled";

  const patch = async (kind: "save" | "approve" | "cancel", changes: Record<string, unknown>, done: string) => {
    setBusy(kind);
    try {
      await api(`/emails/${email.id}`, { method: "PATCH", body: changes });
      await mutate(() => true);
      toast.success(done);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  const regenerate = async () => {
    setBusy("regenerate");
    try {
      const { run_id } = await api<{ run_id: string }>(`/emails/${email.id}/regenerate`, {
        body: instructions?.trim() ? { instructions: instructions.trim() } : {},
      });
      setRewriteRun(run_id);
      setInstructions(null);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  const edits = dirty ? { subject, body } : {};

  const sendTest = async () => {
    setBusy("test");
    try {
      const { sent_to } = await api<{ sent_to: string }>(`/emails/${email.id}/test`, { body: edits });
      toast.success(`Test sent to ${sent_to}`);
    } catch (e) {
      toastError(e);
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0 text-sm">
          <p className="font-medium">
            {email.lead_name || email.lead_email} <span className="font-normal text-muted-foreground">{email.lead_email && <>&lt;<Address email={email.lead_email} />&gt;</>}</span>
          </p>
          <p className="truncate text-muted-foreground">
            {[email.company_name, email.segment_name, showProject && email.project_name].filter(Boolean).join(" · ")}
          </p>
        </div>
        <EmailStatusBadge status={email.status} />
      </div>

      {/* The test button sits by the details, not with the actions below, which already fill their row on laptops. On a
          small phone there's no room beside them, so it wraps under them instead of running out of the card. */}
      <div className="flex flex-wrap items-start justify-between gap-3">
        <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs text-muted-foreground">
          <dt>Type</dt>
          <dd className="text-foreground">{email.kind === "follow_up" ? `Follow-up ${email.step}` : KINDS[email.kind]}</dd>
          {email.from_email && (
            <>
              <dt>From</dt>
              <dd className="text-foreground">
                <Address email={email.from_email} />
              </dd>
            </>
          )}
          {email.sent_at ? (
            <>
              <dt>Sent</dt>
              <dd className="text-foreground">{dateTime(email.sent_at)}</dd>
            </>
          ) : (
            email.scheduled_for && (
              <>
                <dt>Scheduled</dt>
                <dd className="text-foreground">{dateTime(email.scheduled_for)}</dd>
              </>
            )
          )}
        </dl>
        {isDraft && !rewriteRun && (
          <Button size="sm" variant="ghost" className="-my-2 ml-auto shrink-0" onClick={sendTest} loading={busy === "test"}>
            {busy !== "test" && <Send className="size-3.5" />} Send me a test
          </Button>
        )}
      </div>

      {email.status === "failed" && email.error && <p className="rounded-lg bg-red-50 px-3 py-2 text-sm text-red-700">{email.error}</p>}

      {rewriteRun ? (
        <div className="flex items-center gap-2 rounded-xl bg-muted px-4 py-6 font-mono text-xs text-muted-foreground">
          <Spinner /> Rewriting this email…
        </div>
      ) : isDraft ? (
        <div className="flex flex-col gap-2">
          <Input value={subject} onChange={(e) => setSubject(e.target.value)} aria-label="Subject" className="font-medium" />
          {/* Grows to show the whole email. The 20-line cap keeps the card, which is sticky on laptops, inside an 800px-tall screen. */}
          <Textarea value={body} onChange={(e) => setBody(e.target.value)} aria-label="Body" className="max-h-[calc(20lh+1.125rem)] min-h-72 field-sizing-content" />
        </div>
      ) : (
        <div className="rounded-xl border border-border">
          <p className="border-b border-border px-4 py-2.5 text-sm">
            <span className="text-muted-foreground">Subject </span>
            {email.subject}
          </p>
          <p className="px-4 py-3 text-sm leading-relaxed whitespace-pre-wrap">
            <Message text={email.body} />
          </p>
        </div>
      )}

      {isDraft && !rewriteRun && instructions !== null && (
        <div className="flex flex-col gap-2 rounded-xl bg-muted p-3">
          <MicroLabel>Rewrite instructions</MicroLabel>
          <Textarea
            autoFocus
            value={instructions}
            onChange={(e) => setInstructions(e.target.value)}
            className="min-h-16"
            placeholder="Shorter, mention their new location, no question in the subject."
          />
          <div className="flex justify-end gap-2">
            <Button size="sm" variant="ghost" onClick={() => setInstructions(null)}>
              Cancel
            </Button>
            <Button size="sm" onClick={regenerate} loading={busy === "regenerate"}>
              <Sparkles className="size-3.5" /> Rewrite
            </Button>
          </div>
        </div>
      )}

      {isDraft && !rewriteRun && (
        // On phones the buttons stretch to fill their row and Approve gets a full row, so it never sits alone at the edge.
        <div className="flex flex-wrap justify-end gap-2 max-sm:[&>*]:grow">
          <Button size="sm" variant="ghost" onClick={() => patch("cancel", { status: "cancelled" }, "Draft cancelled")} loading={busy === "cancel"}>
            <X className="size-3.5" /> Cancel email
          </Button>
          {instructions === null && (
            <Button size="sm" variant="outline" onClick={() => setInstructions("")}>
              <RefreshCw className="size-3.5" /> Regenerate
            </Button>
          )}
          {dirty && (
            <Button size="sm" variant="outline" onClick={() => patch("save", edits, "Draft saved")} loading={busy === "save"}>
              Save
            </Button>
          )}
          <Button size="sm" className="max-sm:basis-full" onClick={() => patch("approve", { ...edits, status: "approved" }, "Approved for sending")} loading={busy === "approve"}>
            <Check className="size-3.5" /> Approve
          </Button>
        </div>
      )}

      {email.status === "failed" && (
        <div className="flex justify-end">
          <Button size="sm" variant="outline" onClick={() => patch("approve", { status: "approved" }, "Queued to send again")} loading={busy === "approve"}>
            <Send className="size-3.5" /> Send again
          </Button>
        </div>
      )}

      {pending && (
        <div className="flex justify-end">
          <Button size="sm" variant="outline" onClick={() => patch("cancel", { status: "cancelled" }, "Email cancelled")} loading={busy === "cancel"}>
            <X className="size-3.5" /> Cancel sending
          </Button>
        </div>
      )}
    </div>
  );
}
