"use client";

import { useState } from "react";
import { Mail } from "lucide-react";
import useSWR from "swr";
import { Select } from "@/components/ui/form";
import { Card, EmptyState, Tabs } from "@/components/ui/misc";
import { qs } from "@/lib/api";
import type { EmailRow, Paginated, ProjectSummary } from "@/lib/types";
import { cn, isPast, timeAgo } from "@/lib/utils";
import { fetcher } from "@/hooks/use-me";
import { EmailPreview } from "./email-preview";
import { ListKeysHint, useListKeys } from "./list-keys";
import { SearchField } from "./search-field";
import { ListSkeleton, LoadError, Pager, PER_PAGE, useDebounced } from "./states";

type Tab = "drafts" | "scheduled" | "sent" | "failed";

// Approved emails wait for the sender exactly like scheduled ones, so the Scheduled tab shows both.
const TAB_STATUS: Record<Tab, string> = {
  drafts: "draft",
  scheduled: "approved,scheduled,sending",
  sent: "sent",
  failed: "failed",
};

const EMPTY: Record<Tab, string> = {
  drafts: "No drafts to review",
  scheduled: "Nothing scheduled",
  sent: "Nothing sent yet",
  failed: "No failed emails",
};

/** Outbound emails by status. Without `projectId` it covers every project in the org and shows a project filter. */
export function EmailsView({ projectId, segmentId }: { projectId?: string; segmentId?: string }) {
  const [tab, setTab] = useState<Tab>("drafts");
  const [project, setProject] = useState("");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(1);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const query = useDebounced(q);

  const { data: projects } = useSWR<ProjectSummary[]>(projectId ? null : "/projects", fetcher);
  const key = `/emails${qs({
    project_id: projectId ?? project,
    segment_id: segmentId,
    status: TAB_STATUS[tab],
    order: tab === "scheduled" ? "soonest" : undefined,
    q: query,
    page,
    per_page: PER_PAGE,
  })}`;
  const { data, error, mutate } = useSWR<Paginated<EmailRow>>(key, fetcher, { keepPreviousData: true });
  const selected = data?.items.find((e) => e.id === selectedId) ?? data?.items[0];
  // The list and the open email sit side by side from lg, so that's where the keys start.
  const listRef = useListKeys(data?.items.map((e) => e.id), selected?.id, setSelectedId, "64rem");

  const reset = () => {
    setPage(1);
    setSelectedId(null);
  };

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <Tabs
          value={tab}
          onChange={(t) => {
            setTab(t);
            reset();
          }}
          options={[
            { value: "drafts", label: "Drafts" },
            { value: "scheduled", label: "Scheduled" },
            { value: "sent", label: "Sent" },
            { value: "failed", label: "Failed" },
          ]}
        />
        <span className="flex-1" />
        {!projectId && (
          <Select
            value={project}
            onChange={(e) => {
              setProject(e.target.value);
              reset();
            }}
            className="h-9 w-auto"
            aria-label="Project"
          >
            <option value="">All projects</option>
            {projects?.map((p) => (
              <option key={p.id} value={p.id}>
                {p.name || p.domain}
              </option>
            ))}
          </Select>
        )}
        <SearchField
          value={q}
          onChange={(v) => {
            setQ(v);
            reset();
          }}
          label="Search emails"
          className="w-full sm:w-64"
        />
      </div>

      {error ? (
        <Card>
          <LoadError error={error} retry={() => mutate()} />
        </Card>
      ) : !data ? (
        <Card>
          <ListSkeleton />
        </Card>
      ) : data.items.length === 0 ? (
        <Card>
          <EmptyState icon={<Mail className="size-5" />} title={query ? "No emails match your search" : EMPTY[tab]}>
            {tab === "drafts" && !query && "Run \"Write emails\" on a campaign and the drafts show up here for review."}
          </EmptyState>
        </Card>
      ) : (
        <div className="grid grid-cols-[minmax(0,1fr)] items-start gap-4 lg:grid-cols-[minmax(0,2fr)_minmax(0,3fr)]">
          <Card className="overflow-hidden">
            <ul ref={listRef}>
              {data.items.map((e) => (
                <li key={e.id} data-key={e.id} className="border-t border-border first:border-t-0">
                  <button
                    onClick={() => setSelectedId(e.id)}
                    className={cn("flex w-full flex-col gap-0.5 px-4 py-3 text-left hover:bg-accent", selected?.id === e.id && "bg-accent")}
                  >
                    <span className="flex items-center justify-between gap-2 text-sm">
                      <span className="truncate font-medium">{e.lead_name || e.lead_email}</span>
                      <span className="shrink-0 text-xs text-muted-foreground">
                        {/* In the queue, an email without a time or past it goes out at the sender's next chance. */}
                        {tab === "scheduled" && (!e.scheduled_for || isPast(e.scheduled_for))
                          ? "Next up"
                          : timeAgo(e.sent_at ?? e.scheduled_for ?? e.created_at)}
                      </span>
                    </span>
                    <span className="truncate text-xs text-muted-foreground">
                      {[e.company_name, !projectId && e.project_name].filter(Boolean).join(" · ")}
                    </span>
                    <span className="truncate text-sm text-foreground/80">{e.subject}</span>
                  </button>
                </li>
              ))}
            </ul>
            <Pager page={page} total={data.total} onChange={setPage} />
            <ListKeysHint className="hidden border-t border-border px-4 py-2 text-xs text-muted-foreground lg:block" />
          </Card>
          {selected && (
            <Card className="p-5 lg:sticky lg:top-6">
              <EmailPreview key={`${selected.id}:${selected.status}:${selected.subject}:${selected.body}`} email={selected} showProject={!projectId} />
            </Card>
          )}
        </div>
      )}
    </div>
  );
}
