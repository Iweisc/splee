"use client";

import { useState } from "react";
import { ArrowRight, Gift, Mail, Send, Wallet } from "lucide-react";
import { LinkButton } from "@/components/ui/button";
import { num } from "@/lib/utils";

const EMAIL_PRICE = 0.04;
const FREE_CREDITS = 30;
// Uneven steps on purpose: most people try small amounts, so the low end gets more of the track.
const AMOUNTS = [30, 50, 75, 100, 150, 200, 300, 400, 500, 750, 1000];

export function PricingCard() {
  const [index, setIndex] = useState(0);
  const amount = AMOUNTS[index]!;
  const pct = (index / (AMOUNTS.length - 1)) * 100;

  const tiles = [
    { value: num(Math.round(amount / EMAIL_PRICE)), label: "Emails", icon: Send },
    { value: `$${EMAIL_PRICE.toFixed(2)}`, label: "Cost per email", icon: Mail },
    { value: `$${FREE_CREDITS}`, label: "Free credits on signup", icon: Gift },
    { value: "$0", label: "Monthly fee", icon: Wallet },
  ];

  return (
    <div className="mx-auto w-full max-w-[680px] rounded-2xl bg-card p-4 shadow-plaque md:px-9 md:py-6">
      <div className="relative mb-7 pt-9">
        {/* The label tracks the thumb centre: the browser insets the thumb by half its width (8px) at both ends. */}
        <span
          className="absolute -top-1 -translate-x-1/2 text-2xl font-semibold tabular-nums"
          style={{ left: `calc(${pct}% + ${8 - pct * 0.16}px)` }}
        >
          ${num(amount)}
        </span>
        <input
          type="range"
          min={0}
          max={AMOUNTS.length - 1}
          step={1}
          value={index}
          onChange={(e) => setIndex(Number(e.target.value))}
          aria-label="Amount to spend"
          aria-valuetext={`$${amount}`}
          className="block h-1.5 w-full cursor-pointer appearance-none rounded-full outline-none [&::-moz-range-thumb]:size-4 [&::-moz-range-thumb]:rounded-full [&::-moz-range-thumb]:border-[0.5px] [&::-moz-range-thumb]:border-border-strong [&::-moz-range-thumb]:bg-background [&::-webkit-slider-thumb]:size-4 [&::-webkit-slider-thumb]:appearance-none [&::-webkit-slider-thumb]:rounded-full [&::-webkit-slider-thumb]:border-[0.5px] [&::-webkit-slider-thumb]:border-border-strong [&::-webkit-slider-thumb]:bg-background [&::-webkit-slider-thumb]:shadow-sm"
          style={{ background: `linear-gradient(to right, var(--color-brand-600) ${pct}%, rgb(0 0 0 / 0.04) ${pct}%)` }}
        />
      </div>
      <div className="grid grid-cols-2 gap-2">
        {tiles.map((t) => (
          <div key={t.label} className="relative rounded-lg bg-accent p-2.5 pl-2">
            <t.icon className="absolute top-2.5 right-2.5 size-[18px] text-muted-foreground" />
            <p className="text-2xl font-semibold tabular-nums">{t.value}</p>
            <p className="mt-1 text-muted-foreground">{t.label}</p>
          </div>
        ))}
      </div>
      <div className="mt-7 flex justify-center">
        <LinkButton href="/signup" size="lg" className="h-12 rounded-lg px-8">
          Get $30 to try it
          <ArrowRight className="size-4" />
        </LinkButton>
      </div>
    </div>
  );
}
