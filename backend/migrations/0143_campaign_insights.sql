-- Campaign insights (agent/insights.rs): the agent reads a campaign's replies and says what's working, which objections
-- come up and how often, who answers, and how to change the campaign's pain points or angle, which a click applies.

-- One row per time someone asked. insights is null until the run writes it. replies is how many replies it read.
-- applied holds the indexes of the suggestions already used, so the button doesn't offer one twice.
create table campaign_insights (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    segment_id uuid not null references segments(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    insights jsonb,
    replies int not null default 0,
    applied jsonb not null default '[]',
    created_at timestamptz not null default now()
);
create index campaign_insights_segment_idx on campaign_insights (segment_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights'));
