-- Buying committees (agent/committee.rs): who at a company takes part in buying, the roles it lacks, and finding people in them.

-- One row per time someone asked, for the company of the lead they asked from. committee is null until the run maps it.
-- found is how many new leads the search added.
create table buying_committees (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    company_id uuid not null references companies(id) on delete cascade,
    lead_id uuid references leads(id) on delete set null,
    run_id uuid references agent_runs(id) on delete set null,
    committee jsonb,
    found int not null default 0,
    created_at timestamptz not null default now()
);
create index buying_committees_company_idx on buying_committees (company_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee'));
