"use client";

import { useState } from "react";
import { X } from "lucide-react";
import { Field, Input, Textarea } from "@/components/ui/form";
import type { Segment, Targeting } from "@/lib/types";
import { cn } from "@/lib/utils";
import { SEGMENT_ICONS } from "./status";

export type SegmentDraft = Pick<Segment, "name" | "description" | "icon" | "angle" | "pain_points" | "targeting">;

export const EMPTY_SEGMENT: SegmentDraft = { name: "", description: "", icon: "building", angle: "", pain_points: "", targeting: {} };

const TARGETING_FIELDS: { key: keyof Targeting; label: string; placeholder: string }[] = [
  { key: "industries", label: "Industries", placeholder: "Wedding planning" },
  { key: "roles", label: "Roles", placeholder: "Owner, Founder" },
  { key: "company_sizes", label: "Company sizes", placeholder: "1-10, 11-50" },
  { key: "countries", label: "Countries", placeholder: "US, GB" },
  { key: "keywords", label: "Keywords", placeholder: "luxury weddings" },
];

export function SegmentFields({ value, onChange }: { value: SegmentDraft; onChange: (v: SegmentDraft) => void }) {
  const set = <K extends keyof SegmentDraft>(key: K, v: SegmentDraft[K]) => onChange({ ...value, [key]: v });

  return (
    <div className="flex flex-col gap-4">
      <Field label="Name">
        <Input value={value.name} onChange={(e) => set("name", e.target.value)} placeholder="Wedding planners" />
      </Field>
      {/* Its own row: beside the name, three rows of icons left a gap under the input. */}
      <Field label="Icon">
        <div className="flex flex-wrap gap-1">
          {Object.entries(SEGMENT_ICONS).map(([name, Icon]) => (
            <button
              key={name}
              type="button"
              title={name}
              onClick={() => set("icon", name)}
              className={cn(
                "inline-flex size-7 items-center justify-center rounded-md border",
                value.icon === name ? "border-foreground bg-card" : "border-transparent text-muted-foreground hover:bg-accent",
              )}
            >
              <Icon className="size-3.5" />
            </button>
          ))}
        </div>
      </Field>
      <Field label="Who they are">
        <Textarea value={value.description} onChange={(e) => set("description", e.target.value)} className="min-h-16 field-sizing-content" />
      </Field>
      <Field label="Pain points" hint="What hurts for them today. The agent uses this when writing emails.">
        <Textarea value={value.pain_points} onChange={(e) => set("pain_points", e.target.value)} className="min-h-16 field-sizing-content" />
      </Field>
      <Field label="Angle" hint="Why your product matters to this segment in one or two sentences.">
        <Textarea value={value.angle} onChange={(e) => set("angle", e.target.value)} className="min-h-16 field-sizing-content" />
      </Field>
      <div className="grid gap-4 sm:grid-cols-2">
        {TARGETING_FIELDS.map((f) => (
          <Field key={f.key} label={f.label}>
            <ChipsInput
              value={value.targeting[f.key] ?? []}
              onChange={(v) => set("targeting", { ...value.targeting, [f.key]: v })}
              placeholder={f.placeholder}
            />
          </Field>
        ))}
      </div>
    </div>
  );
}

/** Enter or comma adds a chip, Backspace on an empty input removes the last one. */
function ChipsInput({ value, onChange, placeholder }: { value: string[]; onChange: (v: string[]) => void; placeholder?: string }) {
  const [text, setText] = useState("");

  const add = () => {
    const parts = text
      .split(",")
      .map((p) => p.trim())
      .filter((p) => p && !value.includes(p));
    if (parts.length) onChange([...value, ...parts]);
    setText("");
  };

  return (
    <div className="flex min-h-10 flex-wrap items-center gap-1.5 rounded-lg border border-border-strong bg-card px-2 py-1.5 focus-within:border-foreground/40 focus-within:ring-4 focus-within:ring-black/5">
      {value.map((chip) => (
        <span key={chip} className="inline-flex items-center gap-1 rounded-md bg-black/5 py-0.5 pr-1 pl-2 text-xs">
          {chip}
          <button
            type="button"
            onClick={() => onChange(value.filter((c) => c !== chip))}
            className="rounded p-0.5 hover:bg-black/10"
            aria-label={`Remove ${chip}`}
          >
            <X className="size-3" />
          </button>
        </span>
      ))}
      <input
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === ",") {
            e.preventDefault();
            add();
          } else if (e.key === "Backspace" && !text && value.length) {
            onChange(value.slice(0, -1));
          }
        }}
        onBlur={add}
        placeholder={value.length ? "" : placeholder}
        className="min-w-24 flex-1 bg-transparent px-1 text-sm outline-none placeholder:text-subtle-foreground"
      />
    </div>
  );
}
