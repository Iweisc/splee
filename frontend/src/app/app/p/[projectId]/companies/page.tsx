"use client";

import { use, useState } from "react";
import { PageBody, PageHeader } from "@/components/app/page-header";
import { CompaniesTable } from "@/components/app/work/companies-table";
import { DirectoryView } from "@/components/app/work/directory-view";
import { HiringSignals } from "@/components/app/work/hiring";
import { LocalBusinesses } from "@/components/app/work/local-businesses";
import { LookalikesView } from "@/components/app/work/lookalikes";
import { NewsSignals } from "@/components/app/work/news";
import { ResearchView } from "@/components/app/work/research";
import { TechSignals } from "@/components/app/work/tech";
import { VisitorsView } from "@/components/app/work/visitors";
import { Tabs } from "@/components/ui/misc";

const TABS = ["yours", "database", "local", "hiring", "news", "tech", "visitors"] as const;
type Tab = (typeof TABS)[number];

const DESCRIPTIONS: Record<Tab, string> = {
  yours: "Every company in your campaigns, scored by how well it fits your targeting and what it did lately. Open a score to see why.",
  database: "Businesses with their own website, from Overture Maps' open data. Add them to a campaign and the agent finds their decision makers.",
  local: "Dentists, gyms, restaurants and other local businesses from Google Maps, through your own key. Add them to a campaign and Splee finds an email on each website.",
  hiring: "Your companies that are hiring for the roles you watch. A job post makes a warm opener, and the agent uses it in their emails.",
  news: "Your companies that just raised money or announced a launch, an acquisition, a new office or a new leader. The agent can open their emails with it.",
  tech: "The tools your companies' websites show they use. The ones you look for come first, and the agent can open their emails with them.",
  visitors: "Companies that visited your website, the pages they read and how they found you. Add the ones you aren't talking to yet to a campaign.",
};

const RESEARCH = "Ask the agent a question about every company, like \"Do they sell to hospitals?\". It answers from each company's website, with the page that shows it.";

const LOOKALIKES = "Companies in the database like the ones you won deals with, ranked by what they share with them, with the reasons.";

export default function CompaniesPage({ params, searchParams }: PageProps<"/app/p/[projectId]/companies">) {
  const { projectId } = use(params);
  // ?tab=visitors is where hot-visitor alerts link to, and ?tab=news funding alerts.
  const { tab: linked } = use(searchParams);
  // Research is a second view of your companies, for the same reason as "Like your customers" below.
  const [research, setResearch] = useState(linked === "research");
  // "Like your customers" is a second view of the company database, since a seventh tab doesn't fit a phone.
  const [tab, setTab] = useState<Tab>(linked === "lookalikes" ? "database" : (TABS.find((t) => t === linked) ?? "yours"));
  const [lookalikes, setLookalikes] = useState(linked === "lookalikes");
  return (
    <>
      <PageHeader
        title="Companies"
        description={tab === "database" && lookalikes ? LOOKALIKES : tab === "yours" && research ? RESEARCH : DESCRIPTIONS[tab]}
      />
      <PageBody>
        {/* Not a flex column with a gap: the country loader above the search keeps its own margin. */}
        <div className="mb-4">
          <Tabs
            value={tab}
            onChange={setTab}
            // Seven short labels only fit a 390px phone with tighter padding and slightly smaller text.
            className="max-sm:[&>button]:px-1.5 max-sm:[&>button]:text-[13px]"
            options={[
              // Seven tabs don't fit a phone with their full names.
              { value: "yours", label: <ShortLabel short="Yours" full="Your companies" /> },
              { value: "database", label: <ShortLabel short="Database" full="Company database" /> },
              { value: "local", label: <ShortLabel short="Local" full="Local businesses" /> },
              { value: "hiring", label: "Hiring" },
              { value: "news", label: "News" },
              { value: "tech", label: <ShortLabel short="Tech" full="Tech stack" /> },
              { value: "visitors", label: <ShortLabel short="Visits" full="Visitors" /> },
            ]}
          />
        </div>
        {tab === "yours" && (
          <>
            <div className="mb-4">
              <Tabs
                value={research ? "research" : "scores"}
                onChange={(v) => setResearch(v === "research")}
                options={[
                  { value: "scores", label: "Scores" },
                  { value: "research", label: "Research" },
                ]}
              />
            </div>
            {research ? <ResearchView projectId={projectId} /> : <CompaniesTable projectId={projectId} />}
          </>
        )}
        {tab === "database" && (
          <>
            <div className="mb-4">
              <Tabs
                value={lookalikes ? "lookalikes" : "search"}
                onChange={(v) => setLookalikes(v === "lookalikes")}
                options={[
                  { value: "search", label: "Search" },
                  { value: "lookalikes", label: "Like your customers" },
                ]}
              />
            </div>
            {lookalikes ? <LookalikesView projectId={projectId} /> : <DirectoryView projectId={projectId} />}
          </>
        )}
        {tab === "local" && <LocalBusinesses projectId={projectId} />}
        {tab === "hiring" && <HiringSignals projectId={projectId} />}
        {tab === "news" && <NewsSignals projectId={projectId} />}
        {tab === "tech" && <TechSignals projectId={projectId} />}
        {tab === "visitors" && <VisitorsView projectId={projectId} />}
      </PageBody>
    </>
  );
}

function ShortLabel({ short, full }: { short: string; full: string }) {
  return (
    <>
      <span className="sm:hidden">{short}</span>
      <span className="max-sm:hidden">{full}</span>
    </>
  );
}
