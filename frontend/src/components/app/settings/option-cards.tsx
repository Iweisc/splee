"use client";

import { cn } from "@/lib/utils";

export interface Option<T extends string> {
  value: T;
  label: React.ReactNode;
  description?: React.ReactNode;
}

/** Radio group drawn as selectable cards. Set columns with container queries like `@2xl:grid-cols-3`, so they follow the room the cards get, not the screen. */
export function OptionCards<T extends string>({
  name,
  value,
  onChange,
  options,
  className,
}: {
  name: string;
  value: T;
  onChange: (v: T) => void;
  options: Option<T>[];
  className?: string;
}) {
  return (
    <div className="@container">
      <div role="radiogroup" className={cn("grid gap-2", className)}>
        {options.map((o) => {
          const checked = o.value === value;
          return (
            <label
              key={o.value}
              className={cn(
                "flex cursor-pointer items-start gap-3 rounded-xl border p-3.5 text-sm transition-colors",
                checked ? "border-foreground/60 bg-card ring-4 ring-black/5" : "border-border-strong hover:bg-accent",
              )}
            >
              <input type="radio" name={name} checked={checked} onChange={() => onChange(o.value)} className="mt-0.5 accent-black" />
              <span className="min-w-0">
                <span className="block font-medium">{o.label}</span>
                {o.description && <span className="mt-0.5 block text-muted-foreground">{o.description}</span>}
              </span>
            </label>
          );
        })}
      </div>
    </div>
  );
}
