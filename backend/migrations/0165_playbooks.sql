-- Sales playbooks: the agent reads what a project's team has learned (the buyer profile, win and loss reviews, battlecards,
-- campaign insights, its best coached calls and the emails that got hot replies) and writes the playbook a new rep reads first.
-- One row per time someone asked. sources counts what it read.
create table playbooks (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    playbook jsonb,
    sources jsonb not null default '{}',
    created_at timestamptz not null default now()
);
create index playbooks_project_idx on playbooks (project_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion', 'referral_ask', 'churn_risk', 'practice', 'one_on_one', 'renewal', 'playbook'));
