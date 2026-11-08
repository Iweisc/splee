import { Check, ExternalLink, User } from "lucide-react";
import { flag } from "@/components/marketing/flag";
import { LinkedinIcon } from "@/components/marketing/linkedin-icon";
import { XIcon } from "@/components/marketing/x-icon";
import { LogoMark } from "@/components/ui/logo";
import { Avatar, Badge, Card, EmptyState, Favicon, Skeleton } from "@/components/ui/misc";
import type { DemoState, Lead } from "@/lib/types";
import { cn, compact } from "@/lib/utils";
import { CompanyChips, CompanyFacts, DetailsLookOff } from "./company";
import { segmentIcon } from "./steps";

export function StepDetail({
  step,
  state,
  loading,
  activeSegmentId,
  onCorrect,
}: {
  step: number;
  state: DemoState | undefined;
  loading: boolean;
  activeSegmentId: string | null;
  onCorrect: () => void;
}) {
  if (step === 2) return <Comparison state={state} loading={loading || !state} />;
  if (loading || !state) return <SkeletonRows />;
  switch (step) {
    case 1:
      return <CompanyDetail state={state} onCorrect={onCorrect} />;
    case 3:
      return <Campaigns state={state} activeSegmentId={activeSegmentId} />;
    case 4:
      return <Companies state={state} />;
    case 5:
      return <People leads={state.leads} />;
    default:
      return <Emails state={state} />;
  }
}

const skeletonWidths = ["w-44", "w-56", "w-36", "w-52", "w-40"];

function SkeletonRows() {
  return (
    <div className="mt-2 overflow-hidden rounded-2xl bg-card shadow-plaque" aria-label="Loading">
      {Array.from({ length: 9 }, (_, i) => (
        <div key={i} className="flex items-center gap-4 border-b border-border px-5 py-3.5 last:border-b-0">
          <Skeleton className="size-6 rounded-full" />
          <Skeleton className={cn("h-3.5", skeletonWidths[i % skeletonWidths.length])} />
          <Skeleton className="ml-auto h-3.5 w-20" />
        </div>
      ))}
    </div>
  );
}

function CompanyDetail({ state, onCorrect }: { state: DemoState; onCorrect: () => void }) {
  const { project } = state;
  const company = project.company;
  return (
    <Card className="mx-auto mt-2 flex w-full max-w-[440px] flex-col gap-4 p-5">
      <div className="flex items-start gap-3">
        <span className="flex size-9 shrink-0 items-center justify-center rounded-lg border border-border">
          {project.domain ? <Favicon domain={project.domain} size={20} /> : <LogoMark className="size-4 opacity-40" />}
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate font-semibold">{company.name || project.name}</p>
          {project.domain && <p className="truncate text-muted-foreground">{project.domain}</p>}
        </div>
        <span className="inline-flex items-center gap-1 rounded-md border border-border px-2 py-1 text-sm font-medium text-muted-foreground">
          <Check className="size-3.5" />
          Found
        </span>
      </div>
      {(company.description || project.business_description) && (
        <p className="leading-relaxed text-muted-foreground">{company.description || project.business_description}</p>
      )}
      <DetailsLookOff onClick={onCorrect} />
      <CompanyChips company={company} />
      <CompanyFacts company={company} className="border-t border-border pt-4" />
    </Card>
  );
}

function Comparison({ state, loading }: { state: DemoState | undefined; loading: boolean }) {
  const competitors = state?.competitors ?? [];
  return (
    <Card className="mx-auto mt-2 w-full max-w-[560px] p-6 md:p-8">
      <div className="flex items-center gap-4">
        <div className="flex-1 rounded-xl bg-accent p-5">
          <p className="flex items-center gap-1.5 text-sm">
            <LogoMark className="size-3.5" />
            Splee
          </p>
          <p className="mt-3 text-5xl font-semibold tracking-tight">Free</p>
          <p className="mt-3 text-muted-foreground">to start</p>
        </div>
        <span className="text-sm text-muted-foreground">vs</span>
        <div className="flex-1 rounded-xl bg-accent p-5">
          <p className="flex items-center gap-1.5 text-sm text-muted-foreground">
            <User className="size-4" />
            In-house SDR
          </p>
          <p className="mt-3 text-5xl font-semibold tracking-tight text-muted-foreground">$5,000+</p>
          <p className="mt-3 text-muted-foreground">per month</p>
        </div>
      </div>
      <div className="mt-6 border-t border-border pt-6">
        <p className="mb-3 text-xs tracking-wider text-muted-foreground uppercase">
          Competitors <span className="ml-1.5">{loading ? "" : competitors.length}</span>
        </p>
        {loading ? (
          <div className="grid grid-cols-2 gap-2 sm:grid-cols-3">
            {Array.from({ length: 6 }, (_, i) => (
              <Skeleton key={i} className="h-14 rounded-lg" />
            ))}
          </div>
        ) : competitors.length === 0 ? (
          <p className="text-sm text-muted-foreground">The agent didn&apos;t find close competitors.</p>
        ) : (
          <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
            {competitors.map((c) => (
              <a
                key={c.id}
                href={`https://${c.domain}`}
                target="_blank"
                rel="noreferrer"
                className="group flex min-w-0 gap-2.5 rounded-lg border border-border p-3 transition-colors hover:bg-accent"
              >
                <Favicon domain={c.domain} size={16} className="mt-0.5" />
                <span className="min-w-0 flex-1">
                  <span className="flex items-center gap-1.5 text-sm font-medium">
                    <span className="truncate">{c.name || c.domain}</span>
                    <ExternalLink className="size-3 shrink-0 text-muted-foreground opacity-0 group-hover:opacity-100" />
                  </span>
                  <span className="block truncate text-xs text-muted-foreground">{c.domain}</span>
                  {c.note && <span className="mt-1 line-clamp-2 block text-xs text-muted-foreground">{c.note}</span>}
                </span>
              </a>
            ))}
          </div>
        )}
      </div>
    </Card>
  );
}

function Campaigns({ state, activeSegmentId }: { state: DemoState; activeSegmentId: string | null }) {
  if (state.segments.length === 0) return <Empty>The agent didn&apos;t define any campaigns.</Empty>;
  return (
    <div className="mx-auto mt-2 grid w-full max-w-[880px] grid-cols-1 gap-3 lg:grid-cols-2">
      {state.segments.map((s) => {
        const Icon = segmentIcon(s.icon);
        return (
          <Card key={s.id} className={cn("flex flex-col gap-3 p-5", s.id === activeSegmentId && "ring-1 ring-foreground/80")}>
            <div className="flex items-center gap-3">
              <span className="flex size-9 shrink-0 items-center justify-center rounded-lg bg-accent">
                <Icon className="size-4" />
              </span>
              <div className="min-w-0 flex-1">
                <p className="truncate font-medium">{s.name}</p>
                <p className="text-sm text-muted-foreground">Size {compact(s.est_size)}</p>
              </div>
              <Badge tone="green">{s.fit_score}% fit</Badge>
            </div>
            {s.description && <p className="text-sm leading-relaxed text-muted-foreground">{s.description}</p>}
            {!!s.targeting.roles?.length && (
              <div className="flex flex-wrap gap-1">
                {s.targeting.roles.map((r) => (
                  <Badge key={r}>{r}</Badge>
                ))}
              </div>
            )}
            {s.angle && (
              <p className="text-sm leading-relaxed">
                <span className="text-muted-foreground">Angle: </span>
                {s.angle}
              </p>
            )}
          </Card>
        );
      })}
    </div>
  );
}

function Companies({ state }: { state: DemoState }) {
  if (state.companies.length === 0) return <Empty>The agent didn&apos;t find matching companies for this campaign.</Empty>;
  return (
    <Card className="mt-2 overflow-x-auto">
      <table className="w-full text-left text-sm">
        <thead className="bg-accent text-xs tracking-wide text-muted-foreground uppercase">
          <tr>
            <th className="px-4 py-2.5 font-medium">Company</th>
            <th className="px-4 py-2.5 font-medium">Location</th>
            <th className="px-4 py-2.5 font-medium">Employees</th>
            <th className="px-4 py-2.5 font-medium">Fit</th>
            <th className="px-4 py-2.5 font-medium">Why</th>
          </tr>
        </thead>
        <tbody>
          {state.companies.map((c) => (
            <tr key={c.id} className="animate-fade-up border-t border-border align-top">
              <td className="px-4 py-3">
                <div className="flex items-center gap-2.5">
                  <Favicon domain={c.domain} size={18} />
                  <div className="min-w-0">
                    <p className="font-medium">{c.name}</p>
                    <a href={`https://${c.domain}`} target="_blank" rel="noreferrer" className="text-muted-foreground hover:underline">
                      {c.domain}
                    </a>
                  </div>
                </div>
              </td>
              <td className="px-4 py-3 whitespace-nowrap text-muted-foreground">
                {flag(c.country_code)} {c.city || c.country_code.toUpperCase()}
              </td>
              <td className="px-4 py-3 whitespace-nowrap text-muted-foreground">{c.employees || "–"}</td>
              <td className="px-4 py-3 tabular-nums">{c.fit_score}%</td>
              <td className="min-w-64 px-4 py-3 text-muted-foreground">{c.fit_reason}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </Card>
  );
}

const emailBadge: Partial<Record<Lead["email_status"], { tone: "green" | "amber"; label: string }>> = {
  verified: { tone: "green", label: "verified" },
  found: { tone: "green", label: "found" },
  guessed: { tone: "amber", label: "guessed" },
};

function People({ leads }: { leads: Lead[] }) {
  if (leads.length === 0) return <Empty>The agent didn&apos;t find decision makers at these companies.</Empty>;
  return (
    <div className="mx-auto mt-2 grid w-full max-w-[880px] grid-cols-1 gap-3 lg:grid-cols-2">
      {leads.map((l) => {
        const badge = emailBadge[l.email_status];
        return (
          <Card key={l.id} className="flex animate-fade-up flex-col gap-3 p-4">
            <div className="flex items-center gap-3">
              <Avatar name={l.full_name} className="size-10" />
              <div className="min-w-0 flex-1">
                <p className="truncate font-medium">{l.full_name}</p>
                <p className="truncate text-sm text-muted-foreground">{[l.title, l.company_name].filter(Boolean).join(" · ")}</p>
              </div>
              {l.linkedin_url && (
                <a href={l.linkedin_url} target="_blank" rel="noreferrer" aria-label="LinkedIn profile" className="text-muted-foreground hover:text-foreground">
                  <LinkedinIcon className="size-4" />
                </a>
              )}
              {l.x_url && (
                <a href={l.x_url} target="_blank" rel="noreferrer" aria-label="X profile" className="text-muted-foreground hover:text-foreground">
                  <XIcon className="size-4" />
                </a>
              )}
            </div>
            {l.email ? (
              <p className="flex min-w-0 items-center gap-2 text-sm">
                <span className="truncate text-muted-foreground">{l.email}</span>
                {badge && <Badge tone={badge.tone}>{badge.label}</Badge>}
              </p>
            ) : (
              <p className="text-sm text-muted-foreground">No email found yet</p>
            )}
          </Card>
        );
      })}
    </div>
  );
}

function Emails({ state }: { state: DemoState }) {
  if (state.emails.length === 0) return <Empty>No emails were written.</Empty>;
  const leads = new Map(state.leads.map((l) => [l.id, l]));
  return (
    <div className="mx-auto mt-2 flex w-full max-w-[680px] flex-col gap-4">
      {state.emails.map((e) => {
        const lead = leads.get(e.lead_id);
        return (
          <Card key={e.id} className="animate-fade-up overflow-hidden">
            <div className="border-b border-border px-5 py-3 text-sm">
              <span className="text-muted-foreground">To: </span>
              <span className="font-medium">{lead?.full_name ?? e.to_email}</span>
              {lead && <span className="text-muted-foreground"> · {[lead.title, lead.company_name].filter(Boolean).join(", ")}</span>}
            </div>
            <div className="flex gap-3 border-b border-border px-5 py-3">
              <span className="text-muted-foreground">Subject</span>
              <span className="font-medium">{e.subject}</span>
            </div>
            <p className="px-5 py-4 leading-relaxed whitespace-pre-wrap text-muted-foreground">{e.body}</p>
          </Card>
        );
      })}
    </div>
  );
}

function Empty({ children }: { children: React.ReactNode }) {
  return (
    <Card className="mx-auto mt-2 w-full max-w-[560px]">
      <EmptyState title="Nothing here">{children}</EmptyState>
    </Card>
  );
}
