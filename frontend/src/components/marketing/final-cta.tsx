"use client";

import { useEffect, useState } from "react";
import { LinkButton } from "@/components/ui/button";
import { cn } from "@/lib/utils";
import { ConvergingLines } from "./converging-lines";

const LINES = ["You’ve heard it all before", "This one really works"];

export function FinalCta() {
  const [shown, setShown] = useState(0);

  useEffect(() => {
    if (window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    const id = setInterval(() => setShown((s) => (s + 1) % LINES.length), 3200);
    return () => clearInterval(id);
  }, []);

  return (
    <section className="relative mt-40 flex flex-col items-center overflow-hidden px-5">
      <h2 className="grid text-center text-[clamp(2.5rem,7vw,4rem)] leading-tight font-normal tracking-[-0.02em]">
        {LINES.map((line, i) => (
          <span
            key={line}
            aria-hidden={i !== shown}
            className={cn(
              "col-start-1 row-start-1 transition-all duration-700",
              i === shown ? "translate-y-0 opacity-100 blur-none" : "translate-y-3 opacity-0 blur-sm",
            )}
          >
            {line}
          </span>
        ))}
      </h2>
      <LinkButton href="/signup" size="lg" className="relative z-10 mt-8 h-12 rounded-lg px-8">
        Get started
      </LinkButton>
      <ConvergingLines direction="up" className="-mt-6 h-[300px] w-full max-w-[1440px] [mask-image:linear-gradient(#000_55%,transparent)]" />
    </section>
  );
}
