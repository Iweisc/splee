"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import useSWR from "swr";
import { ArrowRight, X } from "lucide-react";
import { ChipInput } from "@/components/app/settings/chip-input";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { LoadError, MicroLabel, toastError } from "@/components/app/work/states";
import { useTech } from "@/components/app/work/tech";
import { Button } from "@/components/ui/button";
import { Field, Select } from "@/components/ui/form";
import { Skeleton, Tabs } from "@/components/ui/misc";
import { api } from "@/lib/api";
import { fetcher } from "@/hooks/use-me";
import type { CompanyProfile, ProjectDetail, Targeting, TechPick } from "@/lib/types";
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

            <HiringRoles roles={s.targeting.hiring_roles} onChange={(v) => set("hiring_roles", v)} />

            <TechPicks />

            <ResearchProfile />
          </>
        );
      }}
    </ProjectSettingsPage>
  );
}

/** Until the project picks its own roles, the box shows its campaigns' roles, which the hiring worker uses then too. */
function HiringRoles({ roles, onChange }: { roles: string[] | undefined; onChange: (v: string[]) => void }) {
  const { projectId } = useParams<{ projectId: string }>();
  const { data } = useSWR<{ default_roles: string[] }>(`/projects/${projectId}/hiring/roles`, fetcher);
  return (
    <Section
      title="Hiring signals"
      description="Every day Splee reads your companies' public job boards. A company hiring for one of these roles gets a Hiring badge, and the agent can open its email with the job post. Leave it empty to stop checking."
    >
      <ChipInput value={roles ?? data?.default_roles ?? []} onChange={onChange} placeholder="e.g. SDR, sales development, account executive" label="Roles to watch" />
      {roles === undefined && !!data?.default_roles.length && (
        <p className="mt-2 text-xs text-muted-foreground">Suggested from the roles your campaigns target.</p>
      )}
    </Section>
  );
}

/** Saved on every change rather than with the page's Save button, since picks are their own list on the server. */
function TechPicks() {
  const { projectId } = useParams<{ projectId: string }>();
  const { data, error, mutate } = useTech(projectId);
  const picks = data?.picks ?? [];
  const categoryOf = new Map(data?.catalogue.map((t) => [t.name, data.categories.find((c) => c.key === t.category)?.label ?? ""]));

  const save = async (next: TechPick[]) => {
    if (!data) return;
    mutate({ ...data, picks: next }, { revalidate: false });
    try {
      await api(`/projects/${projectId}/tech/picks`, { method: "PUT", body: { picks: next } });
    } catch (e) {
      mutate(data, { revalidate: false });
      toastError(e);
    }
  };

  return (
    <Section
      title="Tech stack"
      description="Splee checks your companies' websites and DNS for the tools they use. Companies with the ones you pick show them first, and the agent can open their emails with them. Picks save as you make them."
    >
      {error ? (
        <LoadError error={error} what="the tool list" retry={() => mutate()} />
      ) : !data ? (
        <Skeleton className="h-10 w-full" />
      ) : (
        <div className="space-y-3">
          {picks.length > 0 && (
            <ul className="divide-y divide-border rounded-xl border border-border">
              {picks.map((p) => (
                // On phones the toggle takes a row of its own at full width, with the remove button up beside the name.
                <li key={p.tech} className="grid grid-cols-[1fr_auto] items-center gap-x-3 gap-y-2 py-2 pr-2 pl-4 sm:grid-cols-[1fr_auto_auto]">
                  <span className="min-w-0">
                    <span className="block truncate text-sm font-medium">{p.tech}</span>
                    <span className="block truncate text-xs text-muted-foreground">{categoryOf.get(p.tech)}</span>
                  </span>
                  <Tabs
                    value={p.kind}
                    onChange={(kind) => save(picks.map((x) => (x.tech === p.tech ? { ...x, kind } : x)))}
                    options={[
                      { value: "uses", label: "Good sign" },
                      { value: "competitor", label: "Competitor" },
                    ]}
                    className="order-last col-span-2 flex *:flex-1 sm:order-none sm:col-span-1 sm:inline-flex"
                  />
                  <Button variant="ghost" size="icon" onClick={() => save(picks.filter((x) => x.tech !== p.tech))} aria-label={`Stop looking for ${p.tech}`} className="size-9">
                    <X className="size-4" />
                  </Button>
                </li>
              ))}
            </ul>
          )}
          <Select value="" onChange={(e) => e.target.value && save([...picks, { tech: e.target.value, kind: "uses" }])} aria-label="Add a tool" className="h-9 sm:w-72">
            <option value="">Add a tool…</option>
            {data.categories.map((c) => (
              <optgroup key={c.key} label={c.label}>
                {data.catalogue
                  .filter((t) => t.category === c.key && !picks.some((p) => p.tech === t.name))
                  .map((t) => (
                    <option key={t.name} value={t.name}>
                      {t.name}
                    </option>
                  ))}
              </optgroup>
            ))}
          </Select>
          <p className="text-xs text-muted-foreground">Competitor means your product can replace it.</p>
        </div>
      )}
    </Section>
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
