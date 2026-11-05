"use client";

import { Button } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import type { ProjectSettingsForm } from "./use-project-settings";

/** `problem` blocks saving and replaces the "Unsaved changes" text. */
export function SaveBar({ form, problem }: { form: ProjectSettingsForm; problem?: string | null }) {
  return (
    <div
      className={cn(
        "pointer-events-none sticky bottom-6 z-10 mt-6 flex justify-center transition-all duration-200",
        form.dirty ? "translate-y-0 opacity-100" : "translate-y-4 opacity-0",
      )}
      aria-hidden={!form.dirty}
    >
      <div className={cn("flex items-center gap-3 rounded-2xl bg-primary py-2 pr-2 pl-5 text-sm text-primary-foreground shadow-pop", form.dirty && "pointer-events-auto")}>
        <span className={cn(problem ? "text-amber-300" : "text-white/80")}>{problem ?? "Unsaved changes"}</span>
        <Button variant="ghost" size="sm" className="text-white hover:bg-white/10" onClick={form.reset} disabled={form.saving} tabIndex={form.dirty ? 0 : -1}>
          Discard
        </Button>
        <Button size="sm" className="bg-white text-foreground hover:bg-white/90" onClick={form.save} loading={form.saving} disabled={!!problem} tabIndex={form.dirty ? 0 : -1}>
          Save
        </Button>
      </div>
    </div>
  );
}
