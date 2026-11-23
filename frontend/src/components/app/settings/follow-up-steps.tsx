"use client";

import { ArrowDown, ArrowUp, Phone, Plus, Repeat, Trash2 } from "lucide-react";
import { Button } from "@/components/ui/button";
import { NumberInput, Switch, Textarea } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import type { FollowUp } from "@/lib/types";
import { cn } from "@/lib/utils";

export const MAX_STEPS = 5;
export const MAX_DELAY_DAYS = 30;

/** Why the steps can't be saved, or null. The backend refuses the same delays. */
export function delayProblem(steps: { delay_days: number }[]): string | null {
  return steps.some((f) => !Number.isInteger(f.delay_days) || f.delay_days < 1 || f.delay_days > MAX_DELAY_DAYS)
    ? `Each delay must be 1 to ${MAX_DELAY_DAYS} days`
    : null;
}

/** Add follow-up and Add call, each a step after the last, as many days after it as the last one was. */
export function AddStepButtons({ steps, onChange }: { steps: FollowUp[]; onChange: (steps: FollowUp[]) => void }) {
  const delay_days = steps.at(-1)?.delay_days ?? 3;
  return (
    <>
      <Button
        variant="outline"
        size="sm"
        onClick={() => onChange([...steps, { delay_days, enabled: true, instructions: "" }])}
        disabled={steps.length >= MAX_STEPS}
      >
        <Plus className="size-4" /> Add follow-up
      </Button>
      <Button
        variant="outline"
        size="sm"
        onClick={() => onChange([...steps, { kind: "call", delay_days, enabled: true, instructions: "" }])}
        disabled={steps.length >= MAX_STEPS}
      >
        <Phone className="size-4" /> Add call
      </Button>
    </>
  );
}

/** A sequence after the first email, the project's on the Follow-ups page or a campaign's own on its page. Each step is a
 *  follow-up email or a call, switched on or off, moved, removed, and given instructions. */
export function FollowUpSteps({ steps, onChange }: { steps: FollowUp[]; onChange: (steps: FollowUp[]) => void }) {
  const patch = (i: number, p: Partial<FollowUp>) => onChange(steps.map((f, j) => (j === i ? { ...f, ...p } : f)));
  const move = (i: number, to: number) => {
    const next = [...steps];
    [next[i], next[to]] = [next[to]!, next[i]!];
    onChange(next);
  };

  if (steps.length === 0) {
    return (
      <EmptyState
        icon={<Repeat className="size-6" />}
        title="No follow-ups"
        action={
          <div className="flex flex-wrap justify-center gap-2">
            <AddStepButtons steps={steps} onChange={onChange} />
          </div>
        }
        className="py-8"
      >
        Only the first email is sent to each lead.
      </EmptyState>
    );
  }
  return (
    <ol className="space-y-3">
      {steps.map((f, i) => (
        <li key={i}>
          <Card className={cn("p-4 shadow-none ring-1 ring-border", !f.enabled && "bg-muted")}>
            <div className="flex flex-wrap items-center gap-3">
              <Switch checked={f.enabled} onChange={(enabled) => patch(i, { enabled })} label={`Enable step ${i + 1}`} />
              {f.kind === "call" ? (
                <span className="flex items-center gap-1.5 font-medium">
                  <Phone className="size-4 text-muted-foreground" /> Call
                </span>
              ) : (
                <span className="font-medium">Follow-up {i + 1}</span>
              )}
              <span className="flex items-center gap-2 text-sm text-muted-foreground">
                after
                <NumberInput
                  min={1}
                  max={MAX_DELAY_DAYS}
                  value={f.delay_days}
                  onValueChange={(delay_days) => patch(i, { delay_days })}
                  className="h-8 w-16 text-center tabular-nums"
                  aria-label={`Delay for step ${i + 1} in days`}
                />
                {f.delay_days === 1 ? "day" : "days"}
              </span>
              <div className="ml-auto flex items-center gap-1">
                <Button variant="ghost" size="icon" onClick={() => move(i, i - 1)} disabled={i === 0} aria-label="Move up">
                  <ArrowUp className="size-4" />
                </Button>
                <Button variant="ghost" size="icon" onClick={() => move(i, i + 1)} disabled={i === steps.length - 1} aria-label="Move down">
                  <ArrowDown className="size-4" />
                </Button>
                <Button variant="ghost" size="icon" onClick={() => onChange(steps.filter((_, j) => j !== i))} aria-label="Remove step">
                  <Trash2 className="size-4" />
                </Button>
              </div>
            </div>
            {/* At least two lines, and taller when the hint or the instructions need it, as the hint does on phones. 1.125rem is the padding plus border. */}
            <Textarea
              value={f.instructions}
              onChange={(e) => patch(i, { instructions: e.target.value })}
              placeholder={
                f.kind === "call"
                  ? "Optional: what to talk about on the call, e.g. ask how they book appointments today."
                  : "Optional: what this follow-up should say, e.g. share a case study, or ask if someone else handles this."
              }
              className="mt-3 min-h-[calc(2lh+1.125rem)] bg-card field-sizing-content"
            />
          </Card>
        </li>
      ))}
    </ol>
  );
}
