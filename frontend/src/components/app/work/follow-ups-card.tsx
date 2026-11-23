"use client";

import Link from "next/link";
import { useId, useState } from "react";
import { Repeat } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { AddStepButtons, delayProblem, FollowUpSteps } from "@/components/app/settings/follow-up-steps";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { FollowUp, ProjectSettings, SegmentSteps, SegmentWithStats } from "@/lib/types";
import { cn, plural } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useLeaveGuard } from "@/hooks/use-leave-guard";
import { MicroLabel, toastError } from "./states";

/** Whether the campaign follows the project's follow-ups or has its own. Its own start as a copy of the project's, saved at
 *  once so nothing changes until someone edits them, and edits wait for Save. The page keys it on the saved steps, so a save
 *  starts the draft over. A campaign brought over from another tool sends the steps it came with and shows nothing here. */
export function FollowUpsCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { data: imported } = useSWR<SegmentSteps>(`/segments/${segment.id}/steps`, fetcher);
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${segment.project_id}/settings`, fetcher);
  // The API and MCP may leave a step's instructions out, and the editor needs text.
  const saved = segment.follow_ups?.map((f) => ({ ...f, instructions: f.instructions || "" })) ?? null;
  const [draft, setDraft] = useState(saved);
  const [saving, setSaving] = useState(false);
  const problemId = useId();
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);
  useLeaveGuard(dirty);
  if (!imported || imported.steps.length > 0) return null;

  const save = async (follow_ups: FollowUp[] | null, message: string) => {
    setSaving(true);
    try {
      await mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { follow_ups } }), {
        revalidate: false,
      });
      toast.success(message);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };
  const problem = draft && delayProblem(draft);
  const projectPage = (
    <Link href={`/app/p/${segment.project_id}/follow-ups`} className="font-medium text-foreground underline underline-offset-2">
      Follow-ups page
    </Link>
  );

  return (
    <Card className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <Repeat className="size-3.5" /> Follow-ups
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          {saved ? (
            "This campaign sends its own follow-ups. A change applies to the next step each lead reaches."
          ) : (
            <>Every campaign follows the project&apos;s follow-ups, set on the {projectPage}, unless it has its own.</>
          )}
        </p>
      </div>
      <ul className="divide-y divide-border rounded-lg border border-border">
        <li className="flex items-center gap-3 px-3 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">Its own follow-ups</p>
            <p className="text-xs text-pretty text-muted-foreground">
              {saved ? "Switching it off goes back to the project's and drops these." : "Starts as a copy of the project's."}
            </p>
          </div>
          <Switch
            checked={!!saved}
            onChange={(on) => (on ? save(settings!.follow_ups, "This campaign has its own follow-ups") : save(null, "Back to the project's follow-ups"))}
            disabled={saving || !settings}
            label="Its own follow-ups"
          />
        </li>
      </ul>
      {draft ? (
        // Disabled while saving, since the saved steps start the draft over and would drop an edit made meanwhile.
        <fieldset disabled={saving} className="flex min-w-0 flex-col gap-4">
          <FollowUpSteps steps={draft} onChange={setDraft} />
          {/* Always mounted, so a screen reader is already listening when an edit makes the first problem, like the settings
              save bar. */}
          <span role="status" className="sr-only">
            {problem}
          </span>
          <div className="flex flex-wrap items-center gap-2">
            {draft.length > 0 && <AddStepButtons steps={draft} onChange={setDraft} />}
            {/* On phones the problem takes its own line above the buttons, or it squeezes to a word a line beside them. */}
            <div className="ml-auto flex flex-wrap items-center justify-end gap-2">
              {problem && (
                <p id={problemId} className="text-sm text-red-700 max-sm:w-full max-sm:text-right">
                  {problem}
                </p>
              )}
              {dirty && (
                <Button variant="ghost" onClick={() => setDraft(saved)}>
                  Discard
                </Button>
              )}
              <Button
                onClick={() => save(draft, "Follow-ups saved")}
                loading={saving}
                disabled={!dirty || !!problem}
                aria-describedby={problem ? problemId : undefined}
              >
                Save follow-ups
              </Button>
            </div>
          </div>
        </fieldset>
      ) : (
        settings && <ProjectSteps steps={settings.follow_ups} />
      )}
    </Card>
  );
}

/** The project's follow-ups as this campaign gets them, to read here and change on the Follow-ups page. */
function ProjectSteps({ steps }: { steps: FollowUp[] }) {
  if (steps.length === 0) return <p className="text-sm text-muted-foreground">The project sends no follow-ups, only the first email.</p>;
  return (
    <ol className="divide-y divide-border rounded-lg border border-border text-sm">
      {steps.map((f, i) => (
        <li key={i} className={cn("px-3 py-2.5", !f.enabled && "text-muted-foreground")}>
          <p>
            <span className="font-medium">
              {f.kind === "call" ? "Call" : "Follow-up"} {i + 1}
            </span>
            <span className="text-muted-foreground">
              {" "}
              · {plural(f.delay_days, "day")} later{!f.enabled && ", switched off"}
            </span>
          </p>
          {f.instructions.trim() && <p className="mt-0.5 line-clamp-2 text-pretty text-muted-foreground">{f.instructions}</p>}
        </li>
      ))}
    </ol>
  );
}
