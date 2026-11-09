"use client";

import { useState } from "react";
import { X } from "lucide-react";
import { useFieldLabelId } from "@/components/ui/form";

/** Free-text tags. Enter, comma or paste adds; Backspace on an empty input removes the last one. Put it in a `group` Field, or pass `label` to name it. */
export function ChipInput({
  value,
  onChange,
  placeholder,
  label,
}: {
  value: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  label?: string;
}) {
  const [text, setText] = useState("");
  const labelId = useFieldLabelId();

  const add = (raw: string) => {
    const next = [...value];
    for (const part of raw.split(/[,\n]/)) {
      const v = part.trim();
      if (v && !next.some((x) => x.toLowerCase() === v.toLowerCase())) next.push(v);
    }
    if (next.length !== value.length) onChange(next);
    setText("");
  };

  return (
    <div className="flex min-h-10 flex-wrap items-center gap-1.5 rounded-lg border border-border-strong bg-card px-2 py-1.5 focus-within:border-foreground/40 focus-within:ring-4 focus-within:ring-black/5">
      {value.map((v) => (
        <span key={v} className="inline-flex items-center gap-1 rounded-md bg-black/5 py-0.5 pr-1 pl-2 text-sm">
          {v}
          <button
            type="button"
            onClick={() => onChange(value.filter((x) => x !== v))}
            className="rounded p-0.5 text-muted-foreground hover:bg-black/10 hover:text-foreground"
            aria-label={`Remove ${v}`}
          >
            <X className="size-3" />
          </button>
        </span>
      ))}
      <input
        value={text}
        onChange={(e) => (e.target.value.includes(",") ? add(e.target.value) : setText(e.target.value))}
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
        placeholder={value.length ? undefined : placeholder}
        className="h-7 min-w-32 flex-1 bg-transparent px-1 text-sm outline-none placeholder:text-subtle-foreground"
      />
    </div>
  );
}
