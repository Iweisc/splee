"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import useSWR from "swr";
import { ArrowRight } from "lucide-react";
import { ChipInput } from "@/components/app/settings/chip-input";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { LoadError, MicroLabel } from "@/components/app/work/states";
import { Field } from "@/components/ui/form";
import { Skeleton } from "@/components/ui/misc";
import { fetcher } from "@/hooks/use-me";
import type { CompanyProfile, ProjectDetail, Targeting } from "@/lib/types";
import { cn } from "@/lib/utils";

// Same buckets the agent writes into companies.employees.
const SIZES = ["1-10", "11-50", "51-200", "201-500", "501-1000", "1001-5000", "5000+"];

export default function TargetingPage() {
  return (
    <ProjectSettingsPage
      title="Targeting"
      description="Limits that apply to every campaign in this project. Each campaign still has its own targeting on top."
    >
      {(s, update) => {
        const set = (key: keyof Targeting, value: string[]) => update({ targeting: { ...s.targeting, [key]: value } });
        const sizes = s.targeting.company_sizes ?? [];
        return (
          <>
            <Section title="Countries" description="The agent only looks for companies in these countries and skips the excluded ones. Leave empty to search anywhere.">
              <div className="grid gap-4 sm:grid-cols-2">
                <Field label="Include" group>
                  <ChipInput value={s.targeting.countries ?? []} onChange={(v) => set("countries", v)} placeholder="e.g. United States, Germany" />
                </Field>
                <Field label="Exclude" group>
                  <ChipInput value={s.targeting.exclude_countries ?? []} onChange={(v) => set("exclude_countries", v)} placeholder="e.g. Russia" />
                </Field>
              </div>
            </Section>

            <Section title="Company sizes" description="Only companies with this many employees are added to campaigns. Select none to allow any size.">
              <div className="flex flex-wrap gap-2">
                {SIZES.map((size) => {
                  const on = sizes.includes(size);
                  return (
                    <button
                      key={size}
                      type="button"
                      aria-pressed={on}
                      onClick={() => set("company_sizes", on ? sizes.filter((x) => x !== size) : SIZES.filter((x) => x === size || sizes.includes(x)))}
                      className={cn(
                        "rounded-lg border px-3 py-1.5 text-sm tabular-nums transition-colors",
                        on ? "border-foreground bg-primary text-primary-foreground" : "border-border-strong bg-card hover:bg-accent",
                      )}
                    >
                      {size}
                    </button>
                  );
                })}
              </div>
            </Section>

            <Section title="Excluded keywords" description="Companies whose website or description mentions any of these are skipped, e.g. competitors' niches or industries you don't serve.">
              <ChipInput value={s.targeting.exclude_keywords ?? []} onChange={(v) => set("exclude_keywords", v)} placeholder="e.g. agency, government, nonprofit" label="Excluded keywords" />
            </Section>

            <ResearchProfile />
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}

function ResearchProfile() {
  const { projectId } = useParams<{ projectId: string }>();
  const { data, error, mutate } = useSWR<ProjectDetail>(`/projects/${projectId}`, fetcher);
  const company: CompanyProfile | undefined = data?.project.company;

  return (
    <Section
      title="Research profile"
      description="What the agent learned about your company. It uses this to pick segments and write emails."
      actions={
        <Link href={`/app/p/${projectId}`} className="inline-flex items-center gap-1 text-sm font-medium hover:underline">
          Re-run research on Overview <ArrowRight className="size-3.5" />
        </Link>
      }
    >
      {error ? (
        <LoadError error={error} what="the research profile" retry={() => mutate()} />
      ) : !data ? (
        <div className="space-y-2">
          <Skeleton className="h-4 w-full" />
          <Skeleton className="h-4 w-2/3" />
        </div>
      ) : !company?.description && !company?.industry ? (
        <p className="text-sm text-muted-foreground">No research yet. It appears here once the agent finishes researching your company.</p>
      ) : (
        <div className="space-y-5 text-sm">
          {company.description && <p className="leading-relaxed">{company.description}</p>}
          <div className="grid gap-4 sm:grid-cols-3">
            <ProfileFact label="Industry" value={company.industry} />
            <ProfileFact label="Employees" value={company.employees} />
            <ProfileFact label="Location" value={[company.city, company.country_code].filter(Boolean).join(", ")} />
          </div>
          <ProfileList label="Products" items={company.products} />
          <ProfileList label="Value propositions" items={company.value_props} />
        </div>
      )}
    </Section>
  );
}

function ProfileFact({ label, value }: { label: string; value?: string }) {
  return (
    <div>
      <MicroLabel>{label}</MicroLabel>
      <p className="mt-1">{value || "—"}</p>
    </div>
  );
}

function ProfileList({ label, items }: { label: string; items?: string[] }) {
  if (!items?.length) return null;
  return (
    <div>
      <MicroLabel>{label}</MicroLabel>
      <ul className="mt-1.5 list-disc space-y-1 pl-5">
        {items.map((item) => (
          <li key={item}>{item}</li>
        ))}
      </ul>
    </div>
  );
}
