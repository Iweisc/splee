-- Buyer profiles (agent/buyers.rs): the agent reads a project's won deals next to its lost ones and says what the companies
-- that buy have in common, then drafts a campaign to find more like them. The Pipeline page's Win/loss tab shows the latest.

-- One row per time someone asked. profile is null until the run writes it: {summary, traits: [{trait, count, why, deal_ids}],
-- avoid: [{trait, count, why, deal_ids}], campaign}. won is how many won deals it read.
create table buyer_profiles (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    profile jsonb,
    won int not null default 0,
    created_at timestamptz not null default now()
);
create index buyer_profiles_project_idx on buyer_profiles (project_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile'));
