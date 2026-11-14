"use client";

import { useParams } from "next/navigation";
import useSWR from "swr";
import { ProjectSettingsPage, Section } from "@/components/app/settings/layout";
import { OptionCards } from "@/components/app/settings/option-cards";
import { Field, NumberInput, Switch } from "@/components/ui/form";
import { fetcher } from "@/hooks/use-me";
import type { AutopilotStatus, ProjectSettings } from "@/lib/types";
import { money, num } from "@/lib/utils";

const MAX_BUDGET_USD = 100_000;
const MAX_LEADS = 100_000;

const APPROVAL: { value: "approve" | "send"; label: string; description: string }[] = [
  { value: "approve", label: "Approve each email", description: "What it writes waits on the Emails page until you approve it." },
  { value: "send", label: "Send without approval", description: "What it writes joins the campaign's queue and goes out within your daily limit and schedule." },
];

function problem(s: ProjectSettings) {
  if (s.autopilot_budget_micros < 0 || s.autopilot_budget_micros > MAX_BUDGET_USD * 1_000_000) return `Budget must be between $0 and ${money(MAX_BUDGET_USD * 1_000_000, { cents: false })}`;
  if (s.autopilot_lead_cap < 0 || s.autopilot_lead_cap > MAX_LEADS) return `New leads must be between 0 and ${num(MAX_LEADS)}`;
  return null;
}

export default function AutopilotPage() {
  const { projectId } = useParams<{ projectId: string }>();
  const { data: status } = useSWR<AutopilotStatus>(`/projects/${projectId}/autopilot`, fetcher);

  return (
    <ProjectSettingsPage
      title="Autopilot"
      description="Campaigns that keep themselves stocked with new leads and emails, within a monthly budget."
      problem={problem}
    >
      {(s, update) => (
        <>
          <Section
            title="Run on autopilot"
            description="Every 10 minutes it checks your sending campaigns. When one has less than 3 days of emails written, it finds companies, then the people at them, then writes their emails, one run at a time. Campaigns that get poor results pause themselves."
            actions={<Switch checked={s.autopilot} onChange={(autopilot) => update({ autopilot })} label="Run on autopilot" />}
          />

          <Section
            title="Monthly limits"
            description="Both reset on the first of each month (UTC). Autopilot stops at either one and tells your team when the budget runs out. At the lead cap, first emails also wait for next month."
          >
            <div className="grid gap-4 sm:grid-cols-2">
              <Field
                label="Claude budget"
                hint={status ? `${money(status.spent_micros)} spent so far, counting your own runs.` : "Counting your own runs."}
              >
                <div className="relative">
                  <span className="pointer-events-none absolute top-1/2 left-3 -translate-y-1/2 text-sm text-muted-foreground">$</span>
                  <NumberInput
                    min={0}
                    max={MAX_BUDGET_USD}
                    step={1}
                    value={s.autopilot_budget_micros / 1_000_000}
                    onValueChange={(usd) => update({ autopilot_budget_micros: Math.round(usd * 1_000_000) })}
                    className="pr-16 pl-7 tabular-nums"
                  />
                  <span className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm text-muted-foreground">/month</span>
                </div>
              </Field>
              <Field
                label="New leads"
                hint={status ? `${num(status.contacted)} emailed so far.` : "Leads getting their first email."}
              >
                <div className="relative">
                  <NumberInput
                    min={0}
                    max={MAX_LEADS}
                    step={1}
                    value={s.autopilot_lead_cap}
                    onValueChange={(n) => update({ autopilot_lead_cap: Math.round(n) })}
                    className="pr-16 tabular-nums"
                  />
                  <span className="pointer-events-none absolute top-1/2 right-3 -translate-y-1/2 text-sm text-muted-foreground">/month</span>
                </div>
              </Field>
            </div>
          </Section>

          <Section
            title="Nudge quiet deals"
            description="When an open deal goes 14 days without an email either way, a call or a stage change, the agent reads the conversation and drafts a short email in the thread with a reason to answer now, like their company's news or a page of yours they read. It waits on the Emails page for your approval, and your chats hear about it. Each quiet stretch gets one nudge."
            actions={<Switch checked={s.nudge_quiet_deals} onChange={(nudge_quiet_deals) => update({ nudge_quiet_deals })} label="Nudge quiet deals" />}
          />

          <Section
            title="Rate deal health"
            description="The agent reads each open deal's conversation, close plan and proposal, and rates it on track, at risk or stalled, with how likely it is to close, why, and the next step. It shows on the pipeline and weights the forecast. A deal is rated again when it changes, or every 3 days."
            actions={<Switch checked={s.deal_health} onChange={(deal_health) => update({ deal_health })} label="Rate deal health" />}
          />

          <Section
            title="Qualify deals"
            description="The agent checks each open deal against MEDDICC: the metrics, economic buyer, decision criteria and process, paper process, pain, champion and competition. Each counts only when the buyer said or showed it, and every gap gets a question to ask. Meeting briefs ask those questions first. A deal is checked again after something happens in it."
            actions={<Switch checked={s.qualify_deals} onChange={(qualify_deals) => update({ qualify_deals })} label="Qualify deals" />}
          />

          <Section
            title="Forecast the pipeline"
            description="Every Monday morning the agent sorts the project's open deals into what will close this month, what could and what won't, from their health, qualification, close plan and proposal. It picks the deals to work on this week and the ones slipping, shows it on the Pipeline page's Forecast tab and posts it to your chats."
            actions={<Switch checked={s.pipeline_forecast} onChange={(pipeline_forecast) => update({ pipeline_forecast })} label="Forecast the pipeline" />}
          />

          <Section title="Approval" description="Follow-ups in a sending campaign go out on their own either way.">
            <OptionCards
              name="autopilot_auto_send"
              value={s.autopilot_auto_send ? "send" : "approve"}
              onChange={(v) => update({ autopilot_auto_send: v === "send" })}
              options={APPROVAL}
              className="@xl:grid-cols-2"
            />
          </Section>
        </>
      )}
    </ProjectSettingsPage>
  );
}
