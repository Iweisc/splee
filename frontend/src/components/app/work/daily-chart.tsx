"use client";

import { useState } from "react";
import { cn, num } from "@/lib/utils";

/** A day, or with `weekly` the Monday that starts a week. Meetings get their own row under the bars when given. */
type Point = { date: string; sent: number; replies: number; hot: number; meetings?: number };

const SERIES = [
  { key: "sent", label: "Sent", color: "fill-black/12", swatch: "bg-black/15" },
  { key: "replies", label: "Replies", color: "fill-sky-500", swatch: "bg-sky-500" },
  { key: "hot", label: "Hot", color: "fill-brand-500", swatch: "bg-brand-500" },
] as const;

const MEETINGS = { key: "meetings", label: "Meetings", swatch: "bg-violet-500" } as const;

const W = 20;
const H = 160;
/** The meetings row under the bars. */
const ROW = 14;

/**
 * Replies and hot leads are subsets of sent, so the three series are drawn as nested bars on one scale
 * instead of side by side. Side by side, replies would be invisible next to sent. Meetings are a few a month and happen
 * days after the reply, so a fourth nested bar would be too thin to see. They get a row of marks under the bars instead.
 */
export function DailyChart({ daily, weekly, className }: { daily: Point[]; weekly?: boolean; className?: string }) {
  const [hover, setHover] = useState<number | null>(null);
  const max = Math.max(1, ...daily.map((d) => Math.max(d.sent, d.replies, d.hot)));
  const shown = daily[hover ?? daily.length - 1];
  const meetings = daily.some((d) => d.meetings !== undefined);
  const legend = meetings ? [...SERIES, MEETINGS] : SERIES;
  const height = meetings ? H + ROW : H;

  return (
    <div className={className}>
      <div className="mb-3 flex flex-wrap items-center justify-between gap-3 text-xs">
        <div className="flex flex-wrap gap-x-4 gap-y-1">
          {legend.map((s) => (
            <span key={s.key} className="inline-flex items-center gap-1.5 text-muted-foreground">
              <span className={cn("size-2 rounded-sm", s.swatch)} />
              {s.label}
              {shown && <span className="font-medium text-foreground tabular-nums">{num(shown[s.key] ?? 0)}</span>}
            </span>
          ))}
        </div>
        {shown && <span className="font-mono text-muted-foreground">{weekly ? `Week of ${formatDay(shown.date)}` : formatDay(shown.date)}</span>}
      </div>
      <svg
        viewBox={`0 0 ${Math.max(1, daily.length) * W} ${height}`}
        preserveAspectRatio="none"
        className={meetings ? "h-44 w-full" : "h-40 w-full"}
        onMouseLeave={() => setHover(null)}
        role="img"
        aria-label={`Sent emails, replies${meetings ? ", hot leads and meetings" : " and hot leads"} per ${weekly ? "week" : "day"}`}
      >
        {daily.map((d, i) => (
          <g key={d.date} onMouseEnter={() => setHover(i)}>
            <rect x={i * W} y={0} width={W} height={height} className={hover === i ? "fill-black/[0.03]" : "fill-transparent"} />
            {SERIES.map((s, j) => {
              const h = (d[s.key] / max) * (H - 4);
              const inset = 3 + j * 3;
              return h > 0 ? <rect key={s.key} x={i * W + inset} y={H - h} width={W - inset * 2} height={h} rx={1.5} className={s.color} /> : null;
            })}
            {meetings && (d.meetings ?? 0) > 0 && <rect x={i * W + 4} y={H + 5} width={W - 8} height={6} rx={3} className="fill-violet-500" />}
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

export function formatDay(date: string) {
  return new Date(`${date}T00:00:00`).toLocaleDateString("en-US", { month: "short", day: "numeric" });
}
