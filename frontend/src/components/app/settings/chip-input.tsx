"use client";

import { useState } from "react";
import { X } from "lucide-react";
import { useFieldLabelId } from "@/components/ui/form";
import { cn } from "@/lib/utils";

/**
 * Tags. Enter, comma or paste adds; Backspace on an empty input removes the last one. Put it in a `group` Field, or pass
 * `label` to name it. `parse` turns what's typed into the value kept, or null to leave it in the box marked wrong, and
 * `show` is how a kept value reads.
 */
export function ChipInput({
  value,
  onChange,
  placeholder,
  label,
  parse = (typed) => typed,
  show = (v) => v,
}: {
  value: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  label?: string;
  parse?: (typed: string) => string | null;
  show?: (v: string) => string;
}) {
  const [text, setText] = useState("");
  const [wrong, setWrong] = useState(false);
  const labelId = useFieldLabelId();

  const add = (raw: string) => {
    const next = [...value];
    const left: string[] = [];
    for (const part of raw.split(/[,\n]/)) {
      const typed = part.trim();
      const v = typed ? parse(typed) : null;
      if (v === null) {
        if (typed) left.push(typed);
      } else if (!next.some((x) => x.toLowerCase() === v.toLowerCase())) next.push(v);
    }
    if (next.length !== value.length) onChange(next);
    setText(left.join(", "));
    setWrong(left.length > 0);
  };

  return (
    <div
      className={cn(
        "flex min-h-10 flex-wrap items-center gap-1.5 rounded-lg border bg-card px-2 py-1.5 focus-within:ring-4 focus-within:ring-black/5",
        wrong ? "border-red-400" : "border-border-strong focus-within:border-foreground/40",
      )}
    >
      {value.map((v) => (
        <span key={v} className="inline-flex items-center gap-1 rounded-md bg-black/5 py-0.5 pr-1 pl-2 text-sm">
          {show(v)}
          <button
            type="button"
            onClick={() => onChange(value.filter((x) => x !== v))}
            className="rounded p-0.5 text-muted-foreground hover:bg-black/10 hover:text-foreground"
            aria-label={`Remove ${show(v)}`}
          >
            <X className="size-3" />
          </button>
        </span>
      ))}
      <input
        value={text}
        onChange={(e) => {
          setWrong(false);
          if (e.target.value.includes(",")) add(e.target.value);
          else setText(e.target.value);
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            add(text);
          } else if (e.key === "Backspace" && !text && value.length) {
            onChange(value.slice(0, -1));
          }
        }}
        onPaste={(e) => {
          e.preventDefault();
          add(text + e.clipboardData.getData("text"));
        }}
        onBlur={() => text && add(text)}
        aria-labelledby={labelId}
        aria-label={label}
        aria-invalid={wrong || undefined}
        placeholder={value.length ? undefined : placeholder}
        className="h-7 min-w-12 flex-1 bg-transparent px-1 text-sm focus:outline-hidden placeholder:text-muted-foreground"
      />
    </div>
  );
}
