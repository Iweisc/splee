-- Account expansion (agent/expansions.rs): once a customer has been with us a month, news at their company, new job
-- openings or someone there reading our pricing has the agent decide whether there's room to sell more, and draft the
-- email to the person who bought.

alter table project_settings add column grow_accounts boolean not null default true;

-- One row per time the agent looked at a won deal. signal is what prompted it, empty when someone asked. A row is done
-- once written_at is set: with email_id for a draft, or skipped saying why not.
create table deal_expansions (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    email_id uuid references email_messages(id) on delete set null,
    signal text not null default '',
    why text not null default '',
    skipped text not null default '',
    written_at timestamptz,
    dismissed_at timestamptz,
    created_at timestamptz not null default now()
);
create index deal_expansions_deal_idx on deal_expansions (deal_id, created_at desc);
create index deal_expansions_project_idx on deal_expansions (project_id, created_at desc);

alter table email_messages drop constraint email_messages_kind_check;
alter table email_messages add constraint email_messages_kind_check
    check (kind in ('initial', 'follow_up', 'auto_reply', 'manual_reply', 'inbound', 'meeting_follow_up', 'deal_nudge', 'win_back', 'expansion'));

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal', 'pipeline_forecast', 'buyer_profile', 'coach_call', 'call_prep',
  'win_back', 'battlecard', 'expansion'));
