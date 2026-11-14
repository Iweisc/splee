"use client";

import { use, useState } from "react";
import { HeartHandshake } from "lucide-react";
import useSWR, { useSWRConfig } from "swr";
import { PageBody, PageHeader, ScreenPage } from "@/components/app/page-header";
import { ChurnPanel } from "@/components/app/work/churn-risk";
import { CustomerList } from "@/components/app/work/customers";
import { DealDialog } from "@/components/app/work/deal-dialog";
import { ExpansionPanel } from "@/components/app/work/expansion";
import { ReferralPanel } from "@/components/app/work/referral";
import { RenewalsPanel } from "@/components/app/work/renewals";
import { Select } from "@/components/ui/form";
import { Card, EmptyState, Tabs } from "@/components/ui/misc";
import type { Deal, ProjectSummary } from "@/lib/types";
import { fetcher } from "@/hooks/use-me";

type View = "overview" | "renewals" | "risk" | "expansion" | "referrals";
const VIEWS: View[] = ["overview", "renewals", "risk", "expansion", "referrals"];

export default function CustomersPage({ searchParams }: PageProps<"/app/customers">) {
  // Chat alerts and other pages link here with ?view= for a tab and ?deal= to open a customer.
  const { deal: linked, view: linkedView } = use(searchParams);
  const [view, setView] = useState<View>(VIEWS.find((v) => v === linkedView) ?? "overview");
  const [project, setProject] = useState("");
  const [opened, setOpened] = useState<string | null>(typeof linked === "string" ? linked : null);
  const { data: projects } = useSWR<ProjectSummary[]>("/projects", fetcher);
  const { data: deal } = useSWR<Deal>(opened ? `/deals/${opened}` : null, fetcher);
  // Customers belong to one project. With a single project there is nothing to pick.
  const projectId = project || projects?.[0]?.id || "";
  const { mutate } = useSWRConfig();
  // What the agent did in the deal's window, like an expansion draft, shows in the list once it closes.
  const close = () => {
    setOpened(null);
    mutate(`/projects/${projectId}/customers`);
  };

  return (
    <ScreenPage>
      <PageHeader
        title="Customers"
        description="Everyone who bought, with what they paid and when you last talked. The agent preps each renewal and watches them for signs they might leave, room to sell more, and a good moment to ask for introductions."
        actions={
          <>
            {(projects?.length ?? 0) > 1 && (
              <Select value={projectId} onChange={(e) => setProject(e.target.value)} aria-label="Project" className="h-8 w-auto max-sm:basis-full">
                {projects?.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.name || p.domain}
                  </option>
                ))}
              </Select>
            )}
            <Tabs
              value={view}
              onChange={setView}
              options={[
                { value: "overview", label: "Overview" },
                { value: "renewals", label: "Renewals" },
                { value: "risk", label: "At risk" },
                { value: "expansion", label: "Expansion" },
                { value: "referrals", label: "Referrals" },
              ]}
              className="max-sm:flex-1 max-sm:[&>button]:flex-1"
            />
          </>
        }
      />
      <PageBody className="flex min-h-0 flex-1 flex-col gap-4 overflow-y-auto">
        {projects && !projectId ? (
          <Card>
            <EmptyState icon={<HeartHandshake className="size-5" />} title="No projects yet">
              Create a project and win your first deal to see your customers here.
            </EmptyState>
          </Card>
        ) : !projectId ? null : view === "overview" ? (
          <CustomerList key={projectId} projectId={projectId} onOpen={setOpened} />
        ) : view === "renewals" ? (
          <RenewalsPanel key={projectId} projectId={projectId} onOpen={setOpened} />
        ) : view === "risk" ? (
          <ChurnPanel key={projectId} projectId={projectId} onOpen={setOpened} />
        ) : view === "expansion" ? (
          <ExpansionPanel key={projectId} projectId={projectId} onOpen={setOpened} />
        ) : (
          <ReferralPanel key={projectId} projectId={projectId} onOpen={setOpened} />
        )}
      </PageBody>
      {deal && deal.id === opened && <DealDialog key={deal.id} deal={deal} onClose={close} onSaved={close} />}
    </ScreenPage>
  );
}
