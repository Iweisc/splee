"use client";

import Link from "next/link";
import { useId, useState } from "react";
import { CalendarClock } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { toast } from "sonner";
import { formatDays, hour, type SendWindow, SendWindowFields, windowProblem } from "@/components/app/settings/send-window";
import { Button } from "@/components/ui/button";
import { Switch } from "@/components/ui/form";
import { Card } from "@/components/ui/misc";
import { api } from "@/lib/api";
import type { ProjectSettings, SegmentWithStats } from "@/lib/types";
import { zoneName } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { useKeepFocus } from "@/hooks/use-keep-focus";
import { useLeaveGuard } from "@/hooks/use-leave-guard";
import { MicroLabel, toastError } from "./states";

const LABEL = "Its own days and hours";

/** Whether the campaign sends in the project's days and hours or its own. Its own start as a copy of the project's, saved at once
 *  so nothing changes until someone edits them, and edits wait for Save. The page keys it on the saved hours, so a save starts the
 *  draft over. The time zone and days off stay the project's. */
export function SendHoursCard({ segment }: { segment: SegmentWithStats }) {
  const { mutate } = useSWRConfig();
  const { data: settings } = useSWR<ProjectSettings>(`/projects/${segment.project_id}/settings`, fetcher);
  const saved: SendWindow | null =
    segment.send_days && segment.send_start_hour !== null && segment.send_end_hour !== null
      ? { days: segment.send_days, start_hour: segment.send_start_hour, end_hour: segment.send_end_hour }
      : null;
  const [draft, setDraft] = useState(saved);
  const [saving, setSaving] = useState(false);
  const problemId = useId();
  const root = useKeepFocus<HTMLDivElement>(`send-hours:${segment.id}`, LABEL);
  const dirty = JSON.stringify(draft) !== JSON.stringify(saved);
  useLeaveGuard(dirty);

  const save = async (send_hours: SendWindow | null, message: string) => {
    setSaving(true);
    try {
      await mutate(`/segments/${segment.id}`, api<SegmentWithStats>(`/segments/${segment.id}`, { method: "PATCH", body: { send_hours } }), {
        revalidate: false,
      });
      toast.success(message);
    } catch (e) {
      toastError(e);
    } finally {
      setSaving(false);
    }
  };
  const project = settings && { days: settings.send_days, start_hour: settings.send_start_hour, end_hour: settings.send_end_hour };
  // The API lets a project have no send days, which a campaign's own can't copy, so those start on weekdays.
  const copy = project && { ...project, days: project.days.length ? project.days : [1, 2, 3, 4, 5] };
  const problem = draft && windowProblem(draft);
  const zone = settings && (settings.lead_time_zone ? `in each lead's time zone, else ${zoneName(settings.timezone)}` : zoneName(settings.timezone));
  const shown = draft ?? project;

  return (
    <Card ref={root} className="flex flex-col gap-4 p-5">
      <div className="min-w-0">
        <MicroLabel className="flex items-center gap-1.5">
          <CalendarClock className="size-3.5" /> Sending hours
        </MicroLabel>
        <p className="mt-1 text-sm text-pretty text-muted-foreground">
          When its first emails and follow-ups go out. The time zone and days off are the project&apos;s, set on the{" "}
          <Link href={`/app/p/${segment.project_id}/schedule`} className="font-medium text-foreground underline underline-offset-2">
            Schedule page
          </Link>
          .
        </p>
      </div>
      <ul className="divide-y divide-border rounded-lg border border-border">
        <li className="flex items-center gap-3 px-3 py-2.5">
          <div className="min-w-0 flex-1">
            <p className="text-sm font-medium">Its own days and hours</p>
            <p className="text-xs text-pretty text-muted-foreground">
              {saved ? "Switching it off goes back to the project's." : "Starts as a copy of the project's."}
            </p>
          </div>
          <Switch
            checked={!!saved}
            onChange={(on) => (on ? save(copy!, "This campaign has its own sending hours") : save(null, "Back to the project's sending hours"))}
            disabled={!project}
            loading={saving}
            label={LABEL}
          />
        </li>
      </ul>
      {draft && (
        // Disabled while saving, since the saved hours start the draft over and would drop an edit made meanwhile.
        <fieldset disabled={saving} className="flex min-w-0 flex-col">
          <SendWindowFields value={draft} onChange={setDraft} />
          {/* Always mounted, so a screen reader is already listening when an edit makes the first problem, like the Follow-ups card. */}
          <span role="status" className="sr-only">
            {problem}
          </span>
          {/* On phones the problem takes its own line above the buttons, or it squeezes to a word a line beside them. */}
          <div className="mt-4 flex flex-wrap items-center justify-end gap-2">
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
              onClick={() => save(draft, "Sending hours saved")}
              loading={saving}
              disabled={!dirty || !!problem}
              aria-describedby={problem ? problemId : undefined}
            >
              Save hours
            </Button>
          </div>
        </fieldset>
      )}
      {shown && !windowProblem(shown) && zone && (
        <p className="text-sm text-muted-foreground">
          Sends {formatDays(shown.days)}, {hour(shown.start_hour)}–{hour(shown.end_hour)} {zone}
          {!draft && ", like the rest of the project"}
        </p>
      )}
    </Card>
  );
}
