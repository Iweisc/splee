"use client";

import { useEffect, useRef } from "react";
import { cn } from "@/lib/utils";

type Direction = "right" | "left" | "up";

const LINES = 14;
const DOTS = 26;
const GREEN = "16 185 129";
const GRAY = "163 163 163";

interface Dot {
  line: number;
  t: number;
  speed: number;
  green: boolean;
}

/**
 * Curves that fan out from one edge and converge on the middle of the opposite edge, with small squares drifting
 * along them toward the convergence point. `direction` is where the lines converge to.
 * The math works in a unit square (u = along the flow, v = across it) and maps to pixels at draw time.
 */
export function ConvergingLines({ direction, className }: { direction: Direction; className?: string }) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    const ctx = canvas?.getContext("2d");
    if (!canvas || !ctx) return;

    const reduced = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    let w = 0;
    let h = 0;

    const toPx = (u: number, v: number): [number, number] => {
      if (direction === "right") return [u * w, v * h];
      if (direction === "left") return [(1 - u) * w, v * h];
      return [v * w, (1 - u) * h];
    };

    // Each line starts at v0 on the far edge and ends at the middle (v = 0.5) of the near edge.
    const starts = Array.from({ length: LINES }, (_, i) => -0.1 + (1.2 * i) / (LINES - 1));
    const point = (line: number, t: number): [number, number] => {
      const v0 = starts[line]!;
      const mt = 1 - t;
      const u = 3 * mt * mt * t * 0.45 + 3 * mt * t * t * 0.7 + t * t * t;
      const v = mt * mt * mt * v0 + 3 * mt * mt * t * v0 + 3 * mt * t * t * 0.5 + t * t * t * 0.5;
      return toPx(u, v);
    };

    const dots: Dot[] = Array.from({ length: DOTS }, () => ({
      line: Math.floor(Math.random() * LINES),
      t: Math.random(),
      speed: 0.0008 + Math.random() * 0.0014,
      green: Math.random() < 0.25,
    }));

    const resize = () => {
      const rect = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;
      w = rect.width;
      h = rect.height;
      canvas.width = Math.round(w * dpr);
      canvas.height = Math.round(h * dpr);
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    };

    const draw = () => {
      ctx.clearRect(0, 0, w, h);
      const [ex, ey] = toPx(1, 0.5);
      for (let i = 0; i < LINES; i++) {
        const [sx, sy] = toPx(0, starts[i]!);
        const grad = ctx.createLinearGradient(sx, sy, ex, ey);
        grad.addColorStop(0, "rgb(0 0 0 / 0)");
        grad.addColorStop(0.35, "rgb(0 0 0 / 0.07)");
        grad.addColorStop(1, "rgb(0 0 0 / 0.1)");
        ctx.strokeStyle = grad;
        ctx.lineWidth = 1;
        ctx.beginPath();
        for (let s = 0; s <= 48; s++) {
          const [x, y] = point(i, s / 48);
          if (s === 0) ctx.moveTo(x, y);
          else ctx.lineTo(x, y);
        }
        ctx.stroke();
      }
      for (const d of dots) {
        const [x, y] = point(d.line, d.t);
        const alpha = Math.min(1, d.t * 4) * Math.min(1, (1 - d.t) * 6);
        ctx.fillStyle = `rgb(${d.green ? GREEN : GRAY} / ${alpha * (d.green ? 0.9 : 0.7)})`;
        ctx.fillRect(x - 1.5, y - 1.5, 3, 3);
      }
    };

    let frame = 0;
    let visible = true;
    const tick = () => {
      for (const d of dots) {
        d.t += d.speed;
        if (d.t >= 1) {
          d.t = 0;
          d.line = Math.floor(Math.random() * LINES);
        }
      }
      draw();
      frame = requestAnimationFrame(tick);
    };

    resize();
    draw();
    const ro = new ResizeObserver(() => {
      resize();
      draw();
    });
    ro.observe(canvas);
    if (reduced) return () => ro.disconnect();

    const io = new IntersectionObserver(([entry]) => {
      const nowVisible = entry?.isIntersecting ?? false;
      if (nowVisible && !visible) frame = requestAnimationFrame(tick);
      if (!nowVisible) cancelAnimationFrame(frame);
      visible = nowVisible;
    });
    io.observe(canvas);
    frame = requestAnimationFrame(tick);

    return () => {
      cancelAnimationFrame(frame);
      ro.disconnect();
      io.disconnect();
    };
  }, [direction]);

  return <canvas ref={canvasRef} aria-hidden className={cn("pointer-events-none block", className)} />;
}
