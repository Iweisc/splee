"use client";

import { useState } from "react";
import type { ProjectStats } from "@/lib/types";
import { cn, num } from "@/lib/utils";

const SERIES = [
  { key: "sent", label: "Sent", color: "fill-black/12", swatch: "bg-black/15" },
  { key: "replies", label: "Replies", color: "fill-sky-500", swatch: "bg-sky-500" },
  { key: "hot", label: "Hot", color: "fill-brand-500", swatch: "bg-brand-500" },
] as const;

const W = 20;
const H = 160;

/**
 * Replies and hot leads are subsets of sent, so the three series are drawn as nested bars on one scale
 * instead of side by side. Side by side, replies would be invisible next to sent.
 */
export function DailyChart({ daily, className }: { daily: ProjectStats["daily"]; className?: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(1, ...daily.map((d) => Math.max(d.sent, d.replies, d.hot)));
  const shown = daily[hover ?? daily.length - 1];

  return (
    <div className={className}>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3 text-xs">
        <div className="flex gap-4">
          {SERIES.map((s) => (
            <span key={s.key} className="inline-flex items-center gap-1.5 text-muted-foreground">
              <span className={cn("size-2 rounded-sm", s.swatch)} />
              {s.label}
              {shown && <span className="font-medium text-foreground tabular-nums">{num(shown[s.key])}</span>}
            </span>
          ))}
        </div>
        {shown && <span className="font-mono text-muted-foreground">{formatDay(shown.date)}</span>}
      </div>
      <svg
        viewBox={`0 0 ${Math.max(1, daily.length) * W} ${H}`}
        preserveAspectRatio="none"
        className="h-40 w-full"
        onMouseLeave={() => setHover(null)}
        role="img"
        aria-label="Sent emails, replies and hot leads per day"
      >
        {daily.map((d, i) => (
          <g key={d.date} onMouseEnter={() => setHover(i)}>
            <rect x={i * W} y={0} width={W} height={H} className={hover === i ? "fill-black/[0.03]" : "fill-transparent"} />
            {SERIES.map((s, j) => {
              const h = (d[s.key] / max) * (H - 4);
              const inset = 3 + j * 3;
              return h > 0 ? <rect key={s.key} x={i * W + inset} y={H - h} width={W - inset * 2} height={h} rx={1.5} className={s.color} /> : null;
            })}
          </g>
        ))}
      </svg>
      <div className="mt-1.5 flex justify-between font-mono text-[11px] text-muted-foreground">
        <span>{daily[0] && formatDay(daily[0].date)}</span>
        <span>{daily.at(-1) && formatDay(daily.at(-1)!.date)}</span>
      </div>
    </div>
  );
}

function formatDay(date: string) {
  return new Date(`${date}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" });
}
