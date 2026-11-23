import { createContext, useContext, useId, useState } from "react";
import { cn } from "@/lib/utils";

const field =
  "w-full rounded-lg border border-border-strong bg-card px-3 text-sm text-foreground placeholder:text-muted-foreground focus:outline-hidden transition-shadow focus:border-foreground/40 focus:ring-4 focus:ring-black/5 disabled:opacity-60";

// The enclosing Field's hint or error, so a screen reader focusing the control hears it, and whether it's an error.
const FieldDescription = createContext<{ id?: string; invalid?: boolean }>({});

function useDescribed(props: { "aria-describedby"?: string; "aria-invalid"?: React.AriaAttributes["aria-invalid"] }) {
  const { id, invalid } = useContext(FieldDescription);
  return {
    "aria-describedby": [props["aria-describedby"], id].filter(Boolean).join(" ") || undefined,
    "aria-invalid": props["aria-invalid"] ?? (invalid || undefined),
  };
}

export function Input({ className, ...props }: React.ComponentProps<"input">) {
  const described = useDescribed(props);
  return <input className={cn(field, "h-10", className)} {...props} {...described} />;
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

export function Textarea({ className, ...props }: React.ComponentProps<"textarea">) {
  const described = useDescribed(props);
  return <textarea className={cn(field, "min-h-24 py-2 leading-relaxed", className)} {...props} {...described} />;
}

/** With `w-auto` a select is as wide as its longest option, like a long campaign name. max-w-full stops that from widening the page. */
export function Select({ className, ...props }: React.SelectHTMLAttributes<HTMLSelectElement>) {
  const described = useDescribed(props);
  return <select className={cn(field, "h-10 max-w-full pr-8", className)} {...props} {...described} />;
}

const FieldLabelId = createContext<string | undefined>(undefined);

/** The id of the enclosing `group` Field's label, for an input inside it to name itself with. */
export const useFieldLabelId = () => useContext(FieldLabelId);

/**
 * Label + control + optional hint/error, stacked. The <label> wraps the control so it gets an accessible name without ids.
 * A label sends its clicks to the first button or input inside it, so a control that starts with buttons, like a row of
 * day toggles or chips with remove buttons, passes `group`: the label is then plain text naming the group, and clicking
 * it no longer toggles a day or removes a chip.
 */
export function Field({
  label,
  hint,
  error,
  group,
  children,
  className,
}: {
  label: React.ReactNode;
  hint?: React.ReactNode;
  error?: string | null;
  group?: boolean;
  children: React.ReactNode;
  className?: string;
}) {
  const id = useId();
  const descId = useId();
  const desc = error || hint ? descId : undefined;
  const labelClass = "text-sm font-medium text-foreground";
  return (
    <div className={cn("flex flex-col gap-1.5", className)}>
      {group ? (
        // A group is described as a whole, so its inputs don't each repeat it.
        <div role="group" aria-labelledby={id} aria-describedby={desc} className="flex flex-col gap-1.5">
          <span id={id} className={labelClass}>
            {label}
          </span>
          <FieldLabelId.Provider value={id}>
            <FieldDescription.Provider value={{ invalid: !!error }}>{children}</FieldDescription.Provider>
          </FieldLabelId.Provider>
        </div>
      ) : (
        <label className="flex flex-col gap-1.5">
          <span className={labelClass}>{label}</span>
          <FieldDescription.Provider value={{ id: desc, invalid: !!error }}>{children}</FieldDescription.Provider>
        </label>
      )}
      {/* Always mounted, so a screen reader is already listening when an error appears, like after a submit that leaves focus
          on the button. Out of the flow, so it adds no gap. */}
      <span role="status" className="sr-only">
        {error}
      </span>
      {error ? (
        <p id={descId} className="text-xs text-danger">
          {error}
        </p>
      ) : hint ? (
        <p id={descId} className="text-xs text-muted-foreground">
          {hint}
        </p>
      ) : null}
    </div>
  );
}

export function Switch({
  checked,
  onChange,
  disabled,
  loading,
  label,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
  loading?: boolean;
  label?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      // While it saves it stays focusable and ignores presses, like Button's loading, so keyboard focus isn't dropped to the page.
      aria-disabled={loading || undefined}
      onClick={() => !loading && onChange(!checked)}
      // Drawn in system colors in Windows high contrast, which would otherwise drop the track and the thumb, both backgrounds.
      className={cn(
        "relative inline-flex h-5 w-9 shrink-0 items-center rounded-full transition-colors disabled:opacity-50 aria-disabled:opacity-50",
        "forced-color-adjust-none forced-colors:border forced-colors:border-[ButtonText]",
        checked ? "bg-brand-600 forced-colors:border-[Highlight] forced-colors:bg-[Highlight]" : "bg-black/15 forced-colors:bg-[Canvas]",
      )}
    >
      <span
        className={cn(
          "size-4 rounded-full bg-white shadow transition-transform",
          checked ? "translate-x-4.5 forced-colors:bg-[HighlightText]" : "translate-x-0.5 forced-colors:bg-[ButtonText]",
        )}
      />
    </button>
  );
}
