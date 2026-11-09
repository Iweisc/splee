"use client";

import { useEffect, useRef, useState } from "react";
import { ChevronLeft, ChevronRight, TriangleAlert } from "lucide-react";
import { toast } from "sonner";
import { Button, LinkButton } from "@/components/ui/button";
import { EmptyState, Skeleton } from "@/components/ui/misc";
import { ApiError } from "@/lib/api";
import { cn, num } from "@/lib/utils";

export const PER_PAGE = 50;

export function toastError(e: unknown) {
  toast.error(e instanceof Error ? e.message : "Something went wrong");
}

export function MicroLabel({ className, ...props }: React.HTMLAttributes<HTMLParagraphElement>) {
  return <p className={cn("font-mono text-[11px] tracking-wide text-balance text-muted-foreground uppercase", className)} {...props} />;
}

export function LoadError({ error, retry, what = "this", className }: { error: unknown; retry?: () => void; what?: string; className?: string }) {
  // A deleted project or campaign, or an old link to one. Trying again can't help, so offer a way back instead.
  const notFound = error instanceof ApiError && error.status === 404;
  return (
    <EmptyState
      className={className}
      icon={<TriangleAlert className="size-5 text-danger" />}
      title={notFound ? `Couldn't find ${what}` : `Couldn't load ${what}`}
      action={
        notFound ? (
          <LinkButton href="/app" variant="outline" size="sm">
            Go to your projects
          </LinkButton>
        ) : (
          retry && (
            <Button variant="outline" size="sm" onClick={retry}>
              Try again
            </Button>
          )
        )
      }
    >
      {notFound ? "It may have been deleted, or the link is out of date." : error instanceof Error ? error.message : "The server didn't respond."}
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
  const ref = useRef<HTMLDivElement>(null);
  if (total <= PER_PAGE) return null;
  // The next page's rows replace these in place, so a click down here would land at the bottom of the next page.
  // Back to the first row instead: the list above, if it scrolls by itself like Inbox's, and the page if its top is
  // out of view. 72px leaves room for the phone header that sticks to the top.
  const go = (to: number) => {
    onChange(to);
    ref.current?.previousElementSibling?.scrollTo({ top: 0 });
    const top = ref.current?.parentElement?.getBoundingClientRect().top ?? 0;
    if (top < 0) window.scrollBy({ top: top - 72 });
  };
  return (
    <div ref={ref} className="flex items-center justify-between border-t border-border px-4 py-2.5 text-sm text-muted-foreground">
      <span className="tabular-nums">
        {num((page - 1) * PER_PAGE + 1)}–{num(Math.min(page * PER_PAGE, total))} of {num(total)}
      </span>
      <div className="flex gap-1">
        <Button variant="ghost" size="icon" disabled={page <= 1} onClick={() => go(page - 1)} aria-label="Previous page">
          <ChevronLeft className="size-4" />
        </Button>
        <Button variant="ghost" size="icon" disabled={page >= pages} onClick={() => go(page + 1)} aria-label="Next page">
          <ChevronRight className="size-4" />
        </Button>
      </div>
    </div>
  );
}

/**
 * Removing the last rows of the last page would leave you past the end, where the empty state says the list is empty
 * and hides the pager. Steps back to the new last page instead.
 */
export function useLastPageGuard(total: number | undefined, page: number, setPage: (page: number) => void) {
  useEffect(() => {
    const last = Math.max(1, Math.ceil((total ?? 0) / PER_PAGE));
    if (total !== undefined && page > last) setPage(last);
  }, [total, page, setPage]);
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

/** A clickable row also needs a button or link in it, because a keyboard can't reach the row itself. */
export function Tr({ className, onClick, ...props }: React.HTMLAttributes<HTMLTableRowElement>) {
  return (
    <tr
      className={cn("border-t border-border", onClick && "cursor-pointer hover:bg-accent", className)}
      // A link in the row navigates by itself. Handling its click here too would also navigate this tab on a Ctrl+click.
      onClick={onClick && ((e) => (e.target as Element).closest("a") || onClick(e))}
      {...props}
    />
  );
}
