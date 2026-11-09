"use client";

import { useRef } from "react";
import { Search } from "lucide-react";
import { Input } from "@/components/ui/form";
import { cn } from "@/lib/utils";
import { Kbd, useShortcut } from "./list-keys";

/**
 * The search box above a list. / jumps into it from anywhere on the page, like Gmail and GitHub, so use one per page.
 * The / hint hides once you're typing, and on phones, where there's no keyboard to press it on.
 */
export function SearchField({
  value,
  onChange,
  placeholder = "Search",
  label,
  className,
}: {
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
  label: string;
  className?: string;
}) {
  const input = useRef<HTMLInputElement>(null);
  useShortcut("/", () => input.current?.focus());
  return (
    <div className={cn("relative", className)}>
      <Search className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-muted-foreground" />
      <Input
        ref={input}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        aria-label={label}
        aria-keyshortcuts="/"
        className="peer h-9 pl-9 md:pr-8"
      />
      <span className="pointer-events-none absolute top-1/2 right-2.5 -translate-y-1/2 peer-focus:hidden peer-[:not(:placeholder-shown)]:hidden max-md:hidden">
        <Kbd>/</Kbd>
      </span>
    </div>
  );
}
