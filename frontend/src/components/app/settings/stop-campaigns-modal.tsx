"use client";

import { useState } from "react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { Field, Textarea } from "@/components/ui/form";
import { Modal } from "@/components/ui/misc";
import { api } from "@/lib/api";
import { OptionCards } from "./option-cards";
import { toastError } from "./use-action";

const REASONS = [
  "Not enough replies",
  "Leads aren't a good fit",
  "Too expensive",
  "Deliverability problems",
  "Just pausing for now",
  "Other",
] as const;

type Reason = (typeof REASONS)[number];

/** Pauses every project in the organization and records why. */
export function StopCampaignsModal({ open, onClose, onStopped }: { open: boolean; onClose: () => void; onStopped: () => void }) {
  const [reason, setReason] = useState<Reason | "">("");
  const [details, setDetails] = useState("");
  const [saving, setSaving] = useState(false);

  const stop = async () => {
    setSaving(true);
    try {
      await api("/organization/stop-campaigns", { body: { reason, details: details.trim() } });
      toast.success("All campaigns stopped. Nothing more will be sent until you start a project again.");
      onStopped();
      onClose();
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal open={open} onClose={onClose} title="Stop all campaigns">
      <p className="mb-4 text-sm text-muted-foreground">
        Every project is paused right away and scheduled emails stay unsent. Before you go, what made you stop?
      </p>
      <OptionCards
        name="stop_reason"
        value={reason}
        onChange={setReason}
        options={REASONS.map((r) => ({ value: r, label: r }))}
        className="grid-cols-2"
      />
      <Field label="Anything else?" className="mt-4">
        <Textarea value={details} onChange={(e) => setDetails(e.target.value)} rows={3} placeholder="What would have made Splee work for you?" />
      </Field>
      <div className="mt-5 flex justify-end gap-2">
        <Button variant="outline" onClick={onClose}>
          Cancel
        </Button>
        <Button variant="danger" onClick={stop} disabled={!reason} loading={saving}>
          Stop all campaigns
        </Button>
      </div>
    </Modal>
  );
}
