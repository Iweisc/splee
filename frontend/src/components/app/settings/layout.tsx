"use client";

import { AlertCircle } from "lucide-react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { Card, EmptyState, Skeleton } from "@/components/ui/misc";
import type { ProjectSettings } from "@/lib/types";
import { cn } from "@/lib/utils";
import { SaveBar } from "./save-bar";
import { useProjectSettings, type ProjectSettingsForm } from "./use-project-settings";

export function SettingsColumn({ className, ...props }: React.HTMLAttributes<HTMLDivElement>) {
  return <div className={cn("mx-auto flex max-w-3xl flex-col gap-5", className)} {...props} />;
}

export function MicroLabel({ className, ...props }: React.HTMLAttributes<HTMLParagraphElement>) {
  return <p className={cn("font-mono text-[11px] tracking-wide text-muted-foreground uppercase", className)} {...props} />;
}

/** A settings card: title, one line on what it changes, then the controls. */
export function Section({
  title,
  description,
  actions,
  children,
  className,
}: {
  title: React.ReactNode;
  description?: React.ReactNode;
  actions?: React.ReactNode;
  children?: React.ReactNode;
  className?: string;
}) {
  return (
    <Card className={cn("p-6", className)}>
      <div className="flex items-start justify-between gap-4">
        <div className="min-w-0">
          <h2 className="font-semibold">{title}</h2>
          {description && <p className="mt-0.5 text-sm text-muted-foreground">{description}</p>}
        </div>
        {actions && <div className="flex shrink-0 items-center gap-2">{actions}</div>}
      </div>
      {children && <div className="mt-5">{children}</div>}
    </Card>
  );
}

export function SectionsSkeleton({ count = 2 }: { count?: number }) {
  return (
    <>
      {Array.from({ length: count }, (_, i) => (
        <Card key={i} className="space-y-3 p-6">
          <Skeleton className="h-5 w-40" />
          <Skeleton className="h-4 w-72" />
          <Skeleton className="mt-5 h-10 w-full" />
        </Card>
      ))}
    </>
  );
}

export function LoadError({ error, what = "this page" }: { error: unknown; what?: string }) {
  return (
    <Card>
      <EmptyState icon={<AlertCircle className="size-6" />} title={`Couldn't load ${what}`}>
        {error instanceof Error ? error.message : null}
      </EmptyState>
    </Card>
  );
}

/** Shell for every /app/p/[projectId] settings page: loads the settings, renders the cards, shows the save bar. */
export function ProjectSettingsPage({
  title,
  description,
  problem,
  children,
}: {
  title: string;
  description: string;
  problem?: (s: ProjectSettings) => string | null;
  children: (s: ProjectSettings, update: ProjectSettingsForm["update"], dirty: boolean) => React.ReactNode;
}) {
  const form = useProjectSettings();
  const s = form.settings;
  return (
    <>
      <PageHeader title={title} description={description} />
      <PageBody>
        <SettingsColumn>
          {s ? children(s, form.update, form.dirty) : form.error ? <LoadError error={form.error} what="settings" /> : <SectionsSkeleton />}
          <SaveBar form={form} problem={s && problem ? problem(s) : null} />
        </SettingsColumn>
      </PageBody>
    </>
  );
}
