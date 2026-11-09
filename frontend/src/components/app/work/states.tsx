"use client";

import { useEffect, useState } from "react";
import { ChevronLeft, ChevronRight, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { Button } from "@/components/ui/button";
import { EmptyState, Skeleton } from "@/components/ui/misc";
import { cn, num } from "@/lib/utils";

export const PER_PAGE = 50;

export function toastError(e: unknown) {
  toast.error(e instanceof Error ? e.message : "Something went wrong");
}

export function MicroLabel({ className, ...props }: React.HTMLAttributes<HTMLParagraphElement>) {
  return <p className={cn("font-mono text-[11px] tracking-wide text-balance text-muted-foreground uppercase", className)} {...props} />;
}

export function LoadError({ error, retry, what = "this", className }: { error: unknown; retry?: () => void; what?: string; className?: string }) {
  return (
    <EmptyState
      className={className}
      icon={<TriangleAlert className="size-5 text-danger" />}
      title={`Couldn't load ${what}`}
      action={
        retry && (
          <Button variant="outline" size="sm" onClick={retry}>
            Try again
          </Button>
        )
      }
    >
      {error instanceof Error ? error.message : "The server didn't respond."}
    </EmptyState>
  );
}

export function ListSkeleton({ rows = 6, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn("flex flex-col gap-3 p-4", className)}>
      {Array.from({ length: rows }, (_, i) => (
        <Skeleton key={i} className="h-8" />
      ))}
    </div>
  );
}

export function Pager({ page, total, onChange }: { page: number; total: number; onChange: (page: number) => void }) {
  const pages = Math.max(1, Math.ceil(total / PER_PAGE));
  if (total <= PER_PAGE) return null;
  return (
    <div className="flex items-center justify-between border-t border-border px-4 py-2.5 text-sm text-muted-foreground">
      <span className="tabular-nums">
        {num((page - 1) * PER_PAGE + 1)}–{num(Math.min(page * PER_PAGE, total))} of {num(total)}
      </span>
      <div className="flex gap-1">
        <Button variant="ghost" size="icon" disabled={page <= 1} onClick={() => onChange(page - 1)} aria-label="Previous page">
          <ChevronLeft className="size-4" />
        </Button>
        <Button variant="ghost" size="icon" disabled={page >= pages} onClick={() => onChange(page + 1)} aria-label="Next page">
          <ChevronRight className="size-4" />
        </Button>
      </div>
    </div>
  );
}

export function useDebounced<T>(value: T, ms = 300): T {
  const [debounced, setDebounced] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setDebounced(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return debounced;
}

export function Th({ className, ...props }: React.ThHTMLAttributes<HTMLTableCellElement>) {
  return (
    <th
      className={cn("px-4 py-2.5 text-left font-mono text-[11px] font-normal tracking-wide text-muted-foreground uppercase", className)}
      {...props}
    />
  );
}

export function Td({ className, ...props }: React.TdHTMLAttributes<HTMLTableCellElement>) {
  return <td className={cn("px-4 py-2.5 align-middle", className)} {...props} />;
}

export function Tr({ className, ...props }: React.HTMLAttributes<HTMLTableRowElement>) {
  return <tr className={cn("border-t border-border", props.onClick && "cursor-pointer hover:bg-accent", className)} {...props} />;
}
