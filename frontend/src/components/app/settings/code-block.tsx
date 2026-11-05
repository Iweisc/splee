"use client";

import { CopyButton } from "./copy-button";

export function CodeBlock({ code, label }: { code: string; label?: string }) {
  return (
    <div className="overflow-hidden rounded-xl bg-[#0f0f0f] text-white/90">
      <div className="flex items-center justify-between border-b border-white/10 py-1.5 pr-1.5 pl-4">
        <span className="font-mono text-[11px] tracking-wide text-white/50 uppercase">{label}</span>
        <CopyButton value={code} variant="ghost" size="icon" label={null} className="size-7 text-white/70 hover:bg-white/10 hover:text-white" />
      </div>
      <pre className="overflow-x-auto px-4 py-3 font-mono text-[12.5px] leading-relaxed">{code}</pre>
    </div>
  );
}
