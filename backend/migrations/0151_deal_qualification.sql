-- Deal qualification (agent/qualify.rs): the agent fills in a MEDDICC scorecard for each open deal from its conversation,
-- with what's confirmed, the evidence, and the question that would fill each gap. Meeting briefs ask those questions.

-- On by default: a scorecard costs about two cents, and a deal only gets a new one after something happened in it.
alter table project_settings add column qualify_deals boolean not null default true;

-- One row per scorecard, the newest being the deal's. items is null until the run writes it: a list of
-- {key, status, evidence, ask}, one per MEDDICC element in the order of agent::qualify::ELEMENTS.
create table deal_qualification (
    id uuid primary key default gen_random_uuid(),
    project_id uuid not null references projects(id) on delete cascade,
    deal_id uuid not null references deals(id) on delete cascade,
    run_id uuid references agent_runs(id) on delete set null,
    items jsonb,
    created_at timestamptz not null default now()
);
create index deal_qualification_deal_idx on deal_qualification (deal_id, created_at desc);

alter table agent_runs drop constraint agent_runs_kind_check;
alter table agent_runs add constraint agent_runs_kind_check check (kind in ('explore', 'research_company', 'find_companies', 'find_people',
  'write_emails', 'classify_reply', 'draft_reply', 'linkedin_sequence', 'explorer', 'segment_from_prompt', 'social', 'write_texts',
  'summarize_call', 'meeting_notes', 'research_companies', 'meeting_brief', 'campaign_insights', 'ask', 'buying_committee', 'deal_nudge',
  'close_plan', 'proposal', 'deal_health', 'deal_review', 'qualify_deal'));
