import { CircleHelp, Users } from "lucide-react";
import { flag } from "@/components/marketing/flag";
import type { CompanyProfile } from "@/lib/types";
import { cn } from "@/lib/utils";

const chip = "inline-flex items-center gap-1.5 rounded-md border border-border px-2 py-1 text-sm text-muted-foreground";

export function CompanyChips({ company }: { company: CompanyProfile }) {
  if (!company.employees && !company.city && !company.country_code) return null;
  return (
    <div className="flex flex-wrap gap-1.5">
      {company.employees && (
        <span className={chip}>
          <Users className="size-3.5" />
          {company.employees} emp
        </span>
      )}
      {(company.city || company.country_code) && (
        <span className={chip}>
          {flag(company.country_code)} {company.city || company.country_code?.toUpperCase()}
        </span>
      )}
    </div>
  );
}

export function DetailsLookOff({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} className="inline-flex items-center gap-1.5 text-sm text-muted-foreground transition-colors hover:text-foreground">
      <CircleHelp className="size-3.5" />
      Details look off?
    </button>
  );
}

/** Industry, products, value props: the "More" part of the company card. */
export function CompanyFacts({ company, className }: { company: CompanyProfile; className?: string }) {
  const rows = [
    company.industry && { label: "Industry", value: <p>{company.industry}</p> },
    company.products?.length && {
      label: "Products",
      value: (
        <div className="flex flex-wrap gap-1">
          {company.products.map((p) => (
            <span key={p} className="rounded-md bg-accent px-1.5 py-0.5">
              {p}
            </span>
          ))}
        </div>
      ),
    },
    company.value_props?.length && {
      label: "Value props",
      value: (
        <ul className="list-disc space-y-1 pl-4">
          {company.value_props.map((v) => (
            <li key={v}>{v}</li>
          ))}
        </ul>
      ),
    },
  ].filter((r): r is { label: string; value: React.ReactElement } => !!r);

  if (rows.length === 0) return null;
  return (
    <dl className={cn("flex flex-col gap-3 text-sm", className)}>
      {rows.map((r) => (
        <div key={r.label}>
          <dt className="mb-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">{r.label}</dt>
          <dd>{r.value}</dd>
        </div>
      ))}
    </dl>
  );
}
