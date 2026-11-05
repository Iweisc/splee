"use client";

import { useEffect, useRef, useState } from "react";
import { Loader2, X } from "lucide-react";
import { cn, favicon, initials } from "@/lib/utils";

export function Card({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("rounded-2xl bg-card shadow-card", className)} {...props} />;
}

const badgeTones = {
  neutral: "bg-black/5 text-foreground",
  green: "bg-brand-50 text-brand-700",
  amber: "bg-amber-50 text-amber-700",
  red: "bg-red-50 text-red-700",
  blue: "bg-sky-50 text-sky-700",
  dark: "bg-primary text-primary-foreground",
};

export type BadgeTone = keyof typeof badgeTones;

export function Badge({ tone = "neutral", className, ...props }: React.HTMLAttributes<HTMLSpanElement> & { tone?: BadgeTone }) {
  return (
    <span
      className={cn("inline-flex items-center gap-1 rounded-full px-2 py-0.5 text-xs font-medium whitespace-nowrap", badgeTones[tone], className)}
      {...props}
    />
  );
}

export function Spinner({ className }: { className?: string }) {
  return <Loader2 className={cn("size-4 animate-spin text-muted-foreground", className)} />;
}

export function Skeleton({ className }: { className?: string }) {
  return <div className={cn("animate-pulse rounded-md bg-black/[0.06]", className)} />;
}

export function EmptyState({
  icon,
  title,
  children,
  action,
  className,
}: {
  icon?: React.ReactNode;
  title: string;
  children?: React.ReactNode;
  action?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-col items-center justify-center gap-2 px-6 py-14 text-center", className)}>
      {icon && <div className="mb-1 text-muted-foreground">{icon}</div>}
      <p className="font-medium">{title}</p>
      {children && <div className="max-w-sm text-sm text-muted-foreground">{children}</div>}
      {action && <div className="mt-3">{action}</div>}
    </div>
  );
}

/** Google favicon with a letter fallback, like the competitor chips in the Explee demo. */
export function Favicon({ domain, size = 16, className }: { domain: string; size?: number; className?: string }) {
  const [failed, setFailed] = useState(false);
  if (failed) {
    return (
      <span
        className={cn("inline-flex items-center justify-center rounded-full border border-border text-[9px] font-medium text-muted-foreground", className)}
        style={{ width: size, height: size }}
      >
        {domain[0]?.toUpperCase()}
      </span>
    );
  }
  return (
    // eslint-disable-next-line @next/next/no-img-element
    <img src={favicon(domain, size * 2)} alt="" width={size} height={size} className={cn("shrink-0 rounded-sm", className)} onError={() => setFailed(true)} />
  );
}

export function Avatar({ name, className }: { name: string; className?: string }) {
  return (
    <span className={cn("inline-flex size-8 shrink-0 items-center justify-center rounded-full bg-black/5 text-xs font-medium text-foreground/70", className)}>
      {initials(name) || "?"}
    </span>
  );
}

/** Native <dialog> modal. Closes on Escape and backdrop click. */
export function Modal({
  open,
  onClose,
  title,
  children,
  className,
}: {
  open: boolean;
  onClose: () => void;
  title?: React.ReactNode;
  children: React.ReactNode;
  className?: string;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  return (
    <dialog
      ref={ref}
      onClose={onClose}
      onClick={(e) => e.target === ref.current && onClose()}
      className={cn(
        "m-auto w-[calc(100%-2rem)] max-w-lg rounded-2xl bg-card p-0 text-foreground shadow-pop backdrop:bg-black/30 backdrop:backdrop-blur-[2px]",
        className,
      )}
    >
      {open && (
        <div className="p-6">
          <div className="mb-4 flex items-start justify-between gap-4">
            {title && <h2 className="text-lg font-semibold">{title}</h2>}
            <button onClick={onClose} className="-m-1 rounded-md p-1 text-muted-foreground hover:bg-accent" aria-label="Close">
              <X className="size-4" />
            </button>
          </div>
          {children}
        </div>
      )}
    </dialog>
  );
}

export function Tabs<T extends string>({
  value,
  onChange,
  options,
  className,
}: {
  value: T;
  onChange: (v: T) => void;
  options: { value: T; label: React.ReactNode }[];
  className?: string;
}) {
  return (
    <div className={cn("inline-flex max-w-full overflow-x-auto rounded-xl bg-black/5 p-1", className)}>
      {options.map((o) => (
        <button
          key={o.value}
          onClick={() => onChange(o.value)}
          className={cn(
            "shrink-0 rounded-lg px-3 py-1.5 text-sm font-medium whitespace-nowrap transition-colors",
            value === o.value ? "bg-card text-foreground shadow-card" : "text-muted-foreground hover:text-foreground",
          )}
        >
          {o.label}
        </button>
      ))}
    </div>
  );
}
