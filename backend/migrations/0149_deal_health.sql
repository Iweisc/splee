-- Deal health (agent/health.rs): the agent reads each open deal's conversation and rates it on track, at risk or stalled,
-- with how likely it is to close, why, and the next step. The board and the forecast show the latest rating.

-- On by default: a rating costs about a cent, and only deals that changed or haven't been rated in a while get one.
alter table project_settings add column deal_health boolean not null default true;

-- One row per rating, the newest being the deal's health. health and likelihood are null until the run writes them.
create table deal_health (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    health text check (health in ('on_track', 'at_risk', 'stalled')),
    likelihood int check (likelihood between 0 and 100),
    why text not null default '',
    next_step text not null default '',
    created_at timestamptz not null default now()
);
create index deal_health_deal_idx on deal_health (deal_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health'));
