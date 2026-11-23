"use client";

import { useState } from "react";
import { Button } from "@/components/ui/button";
import { Field, Input } from "@/components/ui/form";
import { Modal } from "@/components/ui/misc";

/** A Date as a datetime-local input's value, in the browser's time zone. */
function localInput(d: Date) {
  return new Date(d.getTime() - d.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

/** Tomorrow at 9 in the morning, where a reminder's picker starts without one set. */
export function tomorrowMorning() {
  const tomorrow = new Date();
  tomorrow.setDate(tomorrow.getDate() + 1);
  tomorrow.setHours(9, 0, 0, 0);
  return tomorrow;
}

/** Picks an exact time in the next `days`, for a reminder, a reply sent later or a campaign's start. It starts on `initial`. */
export function TimePicker({
  title,
  action,
  initial,
  days,
  onPick,
  onClose,
}: {
  title: string;
  action: string;
  initial: Date;
  days: number;
  onPick: (at: string) => Promise<void>;
  onClose: () => void;
}) {
  const [value, setValue] = useState(() => localInput(initial));
  const [saving, setSaving] = useState(false);
  const [left, setLeft] = useState(false);
  // When the picker opened. The server turns away a time that has passed since.
  const [now] = useState(() => Date.now());
  const at = value ? new Date(value) : null;
  const latest = now + days * 86_400_000;
  const valid = !!at && !Number.isNaN(at.getTime()) && at.getTime() > now && at.getTime() <= latest;
  return (
    <Modal open onClose={onClose} title={title} className="max-w-sm">
      <form
        className="grid gap-4"
        onSubmit={async (e) => {
          e.preventDefault();
          if (!valid) return;
          setSaving(true);
          await onPick(at.toISOString());
          setSaving(false);
        }}
      >
        {/* Only after leaving the field: typing a date passes through past ones, like 01/10 on the way to 11/10. */}
        <Field label="When" error={left && !valid ? `Pick a time in the next ${days} days` : null}>
          <Input
            type="datetime-local"
            value={value}
            min={localInput(new Date(now))}
            max={localInput(new Date(latest))}
            onChange={(e) => setValue(e.target.value)}
            onBlur={() => setLeft(true)}
            required
          />
        </Field>
        <div className="flex justify-end gap-2">
          <Button type="button" variant="outline" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={!valid} loading={saving}>
            {action}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
