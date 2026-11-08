import { useState } from "react";
import { cn } from "@/lib/utils";

const field =
  "w-full rounded-lg border border-border-strong bg-card px-3 text-sm text-foreground placeholder:text-subtle-foreground outline-none transition-shadow focus:border-foreground/40 focus:ring-4 focus:ring-black/5 disabled:opacity-60";

export function Input({ className, ...props }: React.InputHTMLAttributes<HTMLInputElement>) {
  return <input className={cn(field, "h-10", className)} {...props} />;
}

/**
 * A number field you can clear and retype. The text being typed lives here until blur, so an empty box or a
 * half-typed "1." doesn't snap to 0 or get clamped mid-word. The parent only hears about values that parse.
 * The wheel blurs it, because browsers otherwise change a focused number field when you scroll the page over it.
 */
export function NumberInput({
  value,
  onValueChange,
  ...props
}: Omit<React.InputHTMLAttributes<HTMLInputElement>, "type" | "value" | "onChange" | "onBlur" | "onWheel"> & {
  value: number;
  onValueChange: (value: number) => void;
}) {
  const [draft, setDraft] = useState<string | null>(null);
  return (
    <Input
      {...props}
      type="number"
      value={draft ?? String(value)}
      onChange={(e) => {
        setDraft(e.target.value);
        if (e.target.value !== "" && Number.isFinite(e.target.valueAsNumber)) onValueChange(e.target.valueAsNumber);
      }}
      onBlur={() => setDraft(null)}
      onWheel={(e) => e.currentTarget.blur()}
    />
  );
}

export function Textarea({ className, ...props }: React.TextareaHTMLAttributes<HTMLTextAreaElement>) {
  return <textarea className={cn(field, "min-h-24 py-2 leading-relaxed", className)} {...props} />;
}

/** With `w-auto` a select is as wide as its longest option, like a long campaign name. max-w-full stops that from widening the page. */
export function Select({ className, ...props }: React.SelectHTMLAttributes<HTMLSelectElement>) {
  return <select className={cn(field, "h-10 max-w-full pr-8", className)} {...props} />;
}

export function Label({ className, ...props }: React.LabelHTMLAttributes<HTMLLabelElement>) {
  return <label className={cn("text-sm font-medium text-foreground", className)} {...props} />;
}

/** Label + control + optional hint/error, stacked. The <label> wraps the control so it gets an accessible name without ids. */
export function Field({
  label,
  hint,
  error,
  children,
  className,
}: {
  label: React.ReactNode;
  hint?: React.ReactNode;
  error?: string | null;
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      <label className="flex flex-col gap-1.5">
        <span className="text-sm font-medium text-foreground">{label}</span>
        {children}
      </label>
      {error ? <p className="text-xs text-danger">{error}</p> : hint ? <p className="text-xs text-muted-foreground">{hint}</p> : null}
    </div>
  );
}

export function Switch({
  checked,
  onChange,
  disabled,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors disabled:opacity-50",
        checked ? "bg-brand-600" : "bg-black/15",
      )}
    >
      <span className={cn("size-4 rounded-full bg-white shadow transition-transform", checked ? "translate-x-4.5" : "translate-x-0.5")} />
    </button>
  );
}
