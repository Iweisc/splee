import { cn } from "@/lib/utils";

/** Standard top of every /app page: title, optional description, actions on the right. */
export function PageHeader({
  title,
  description,
  actions,
  className,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  className?: string;
}) {
  return (
    <div className={cn("flex flex-wrap items-end justify-between gap-4 border-b border-border px-4 pt-6 pb-5 sm:px-8 sm:pt-8", className)}>
      <div className="min-w-0">
        <h1 className="text-2xl font-semibold tracking-tight">{title}</h1>
        {description && <p className="mt-1 max-w-2xl text-sm text-muted-foreground">{description}</p>}
      </div>
      {actions && <div className="flex max-w-full flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}

export function PageBody({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("px-4 py-6 sm:px-8", className)} {...props} />;
}

/** For pages whose body fills the rest of the screen and scrolls inside, like the inbox. 3.5rem is the shell's mobile top bar. */
export function ScreenPage({ children }: { children: React.ReactNode }) {
  return <div className="flex h-[calc(100dvh-3.5rem)] flex-col lg:h-dvh">{children}</div>;
}
