"use client";

import { ArrowDown, ArrowUp, Plus, Repeat, Trash2 } from "lucide-react";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { Button } from "@/components/ui/button";
import { NumberInput, Switch, Textarea } from "@/components/ui/form";
import { Card, EmptyState } from "@/components/ui/misc";
import type { FollowUp } from "@/lib/types";
import { cn } from "@/lib/utils";

const MAX_STEPS = 5;
const MAX_DELAY_DAYS = 30;

export default function FollowUpsPage() {
  return (
    <ProjectSettingsPage
      title="Follow-ups"
      description="If a lead doesn't reply, the agent writes a short follow-up in the same email thread. A reply of any kind stops the sequence."
      problem={(s) =>
        s.follow_ups.some((f) => !Number.isInteger(f.delay_days) || f.delay_days < 1 || f.delay_days > MAX_DELAY_DAYS)
          ? `Each delay must be 1 to ${MAX_DELAY_DAYS} days`
          : null
      }
    >
      {(s, update) => {
        const steps = s.follow_ups;
        const set = (next: FollowUp[]) => update({ follow_ups: next });
        const patch = (i: number, p: Partial<FollowUp>) => set(steps.map((f, j) => (j === i ? { ...f, ...p } : f)));
        const move = (i: number, to: number) => {
          const next = [...steps];
          [next[i], next[to]] = [next[to]!, next[i]!];
          set(next);
        };
        const add = (
          <Button
            variant="outline"
            size="sm"
            onClick={() => set([...steps, { delay_days: steps.at(-1)?.delay_days ?? 3, enabled: true, instructions: "" }])}
            disabled={steps.length >= MAX_STEPS}
          >
            <Plus className="size-4" /> Add follow-up
          </Button>
        );

        return (
          <Section
            title="Sequence"
            description={`Up to ${MAX_STEPS} follow-ups. Each delay counts from the previous email. Disabled steps are skipped.`}
            actions={steps.length > 0 && add}
          >
            {steps.length === 0 ? (
              <EmptyState icon={<Repeat className="size-6" />} title="No follow-ups" action={add} className="py-8">
                Only the first email is sent to each lead.
              </EmptyState>
            ) : (
              <ol className="space-y-3">
                {steps.map((f, i) => (
                  <li key={i}>
                    <Card className={cn("p-4 shadow-none ring-1 ring-border", !f.enabled && "bg-muted")}>
                      <div className="flex flex-wrap items-center gap-3">
                        <Switch checked={f.enabled} onChange={(enabled) => patch(i, { enabled })} label={`Enable follow-up ${i + 1}`} />
                        <span className="font-medium">Follow-up {i + 1}</span>
                        <span className="flex items-center gap-2 text-sm text-muted-foreground">
                          after
                          <NumberInput
                            min={1}
                            max={MAX_DELAY_DAYS}
                            value={f.delay_days}
                            onValueChange={(delay_days) => patch(i, { delay_days })}
                            className="h-8 w-16 text-center tabular-nums"
                            aria-label={`Delay for follow-up ${i + 1} in days`}
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
                          <Button variant="ghost" size="icon" onClick={() => set(steps.filter((_, j) => j !== i))} aria-label="Remove follow-up">
                            <Trash2 className="size-4" />
                          </Button>
                        </div>
                      </div>
                      {/* At least two lines, and taller when the hint or the instructions need it, as the hint does on phones. 1.125rem is the padding plus border. */}
                      <Textarea
                        value={f.instructions}
                        onChange={(e) => patch(i, { instructions: e.target.value })}
                        placeholder="Optional: what this follow-up should say, e.g. share a case study, or ask if someone else handles this."
                        className="mt-3 min-h-[calc(2lh+1.125rem)] bg-card field-sizing-content"
                      />
                    </Card>
                  </li>
                ))}
              </ol>
            )}
          </Section>
        );
      }}
    </ProjectSettingsPage>
  );
}
