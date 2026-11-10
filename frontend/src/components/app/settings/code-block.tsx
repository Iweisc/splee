"use client";

import { cn } from "@/lib/utils";
import { CopyButton } from "./copy-button";

/** `wrap` breaks long lines, at spaces first, for snippets people copy whole rather than read side by side. */
export function CodeBlock({ code, label, wrap }: { code: string; label?: string; wrap?: boolean }) {
  return (
    <div className="overflow-hidden rounded-xl bg-[#0f0f0f] text-white/90">
      <div className="flex items-center justify-between border-b border-white/10 py-1.5 pr-1.5 pl-4">
        <span className="font-mono text-[11px] tracking-wide text-white/50 uppercase">{label}</span>
        <CopyButton value={code} variant="ghost" size="icon" label={null} className="size-7 text-white/70 hover:bg-white/10 hover:text-white" />
      </div>
      {/* Focusable so a keyboard can scroll a long line into view. The ring is inset and light: the default one is dark,
          so it vanished on this background, and the card's overflow-hidden would clip one drawn outside. */}
      <pre
        tabIndex={0}
        className={cn(
          "overflow-x-auto px-4 py-3 font-mono text-[12.5px] leading-relaxed focus-visible:outline-2 focus-visible:-outline-offset-2 focus-visible:outline-white/60",
          wrap && "whitespace-pre-wrap [overflow-wrap:anywhere]",
        )}
      >
        {code}
      </pre>
    </div>
  );
}
