"use client";

import { useId, useState } from "react";
import { Button } from "@/components/ui/button";
import { useLeaveGuard } from "@/hooks/use-leave-guard";
import { cn } from "@/lib/utils";
import type { ProjectSettingsForm } from "./use-project-settings";

/** `problem` blocks saving and replaces the "Unsaved changes" text. */
/** Takes any form with these four, not only the project settings one. */
export function SaveBar({ form, problem }: { form: Pick<ProjectSettingsForm, "dirty" | "saving" | "save" | "reset">; problem?: string | null }) {
  useLeaveGuard(form.dirty);
  const status = useId();
  // Saving or discarding from the keyboard leaves focus on the bar, so it stays on screen until focus moves on, or the focus
  // would sit on a button nobody can see. A mouse click hides it at once, as it always has. Enter or Space counts too, for
  // someone who clicked Save, saw it fail and pressed Enter to retry: that fires no new focus event.
  const [keyboard, setKeyboard] = useState(false);
  const shown = form.dirty || keyboard;
  const blocked = form.dirty && problem;
  return (
    <>
      {/* Always in the page and outside the hidden bar, so a screen reader is already listening when the first edit is
          also the first problem. Only problems are read out, not every "Unsaved changes", and the clean state once Save or
          Discard leaves focus in the bar. */}
      <span role="status" className="sr-only">
        {form.dirty ? problem : keyboard && "No unsaved changes"}
      </span>
      <div
        className={cn(
          "pointer-events-none sticky bottom-6 z-10 mt-6 flex justify-center transition-all duration-200",
          shown ? "translate-y-0 opacity-100" : "translate-y-4 opacity-0",
        )}
        aria-hidden={!shown}
        onFocus={(e) => setKeyboard(e.target.matches(":focus-visible"))}
        onKeyDown={(e) => !e.nativeEvent.isComposing && (e.key === "Enter" || e.key === " ") && setKeyboard(true)}
        onBlur={(e) => !e.currentTarget.contains(e.relatedTarget) && setKeyboard(false)}
      >
        {/* A long problem message would squeeze into a narrow column beside the buttons on phones, so the buttons wrap
            under it instead. */}
        <div
          className={cn(
            "flex flex-wrap items-center gap-x-2 gap-y-1 rounded-2xl bg-primary py-2 pr-2 pl-5 text-sm text-primary-foreground shadow-pop",
            shown && "pointer-events-auto",
          )}
        >
          {/* Describes a disabled Save too, for a screen reader user who browses to it rather than tabbing, which skips it. */}
          <span id={status} className={blocked ? "text-amber-300" : "text-white/80"}>
            {form.dirty ? (problem ?? "Unsaved changes") : "No unsaved changes"}
          </span>
          <div className="ml-auto flex gap-1">
            <Button
              variant="ghost"
              size="sm"
              className="text-white hover:bg-white/10"
              onClick={form.dirty ? form.reset : undefined}
              disabled={form.saving}
              aria-disabled={!form.dirty || undefined}
              tabIndex={shown ? 0 : -1}
            >
              Discard
            </Button>
            <Button
              size="sm"
              className="bg-white text-foreground hover:bg-white/90"
              onClick={form.dirty ? form.save : undefined}
              loading={form.saving}
              disabled={!!blocked}
              aria-disabled={!form.dirty || undefined}
              aria-describedby={blocked ? status : undefined}
              tabIndex={shown ? 0 : -1}
            >
              Save
            </Button>
          </div>
        </div>
      </div>
    </>
  );
}
