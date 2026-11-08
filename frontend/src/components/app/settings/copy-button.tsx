"use client";

import { useState } from "react";
import { Check, Copy } from "lucide-react";
import { toast } from "sonner";
import { Button, type ButtonVariant, type ButtonSize } from "@/components/ui/button";

/** Pass `label={null}` for an icon-only button. */
export function CopyButton({
  value,
  label = "Copy",
  variant = "outline",
  size = "sm",
  className,
}: {
  value: string;
  label?: string | null;
  variant?: ButtonVariant;
  size?: ButtonSize;
  className?: string;
}) {
  const [copied, setCopied] = useState(false);
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      setTimeout(() => setCopied(false), 1500);
    } catch {
      toast.error("Couldn't access the clipboard. Select the text and copy it manually.");
    }
  };
  return (
    <Button type="button" variant={variant} size={size} onClick={copy} className={className} aria-label={label ?? "Copy"}>
      {copied ? <Check className="size-4 text-brand-500" /> : <Copy className="size-4" />}
      {label}
    </Button>
  );
}

/** Read-only mono field with a copy button, for links and secrets. `children` are extra buttons placed after Copy. */
export function CopyField({ value, children }: { value: string; children?: React.ReactNode }) {
  return (
    // The value is never cut off. When it doesn't fit beside the buttons it takes its own line and wraps, and the buttons go under it.
    <div className="flex flex-wrap items-center justify-end gap-2">
      <code className="min-w-0 grow-[999] rounded-lg border border-border-strong bg-muted px-3 py-2 font-mono text-sm break-all select-all">{value}</code>
      <div className="flex gap-2 max-sm:grow max-sm:[&>*]:grow">
        <CopyButton value={value} />
        {children}
      </div>
    </div>
  );
}
